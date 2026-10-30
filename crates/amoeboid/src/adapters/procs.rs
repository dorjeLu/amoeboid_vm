//! `StdProcs`: [`Procs`] on `/proc` and `pidfd`. A process counts as ours only if its pid, start
//! time (`/proc/PID/stat`) and executable name (`/proc/PID/exe`) all equal the recorded identity;
//! signals go through a `pidfd` opened before that check, so the pid can't be reused in between.

use std::fs;
use std::future::Future;
use std::io;
use std::os::unix::fs::OpenOptionsExt as _;
use std::os::unix::process::ExitStatusExt as _;
use std::path::PathBuf;
use std::process::Stdio;

use rustix::io::Errno;
use rustix::process::{self, PidfdFlags};

use crate::domain::{AbsDir, FileName, Pid, ProcIdentity, ProgramName, StartTime, parse_stat};
use crate::plan::Argv;
use crate::ports::Cause;
use crate::ports::procs::{
    ChildIo, Exit, LockHandoff, Presence, ProcError, ProcOp, Procs, Signal, Signalled, SpawnError,
    Spawned,
};
use crate::ports::vm_lock::HeldLock;
use crate::refined;

const PROC: FileName = refined!(FileName, "proc");

/// The kernel's process filesystem, rooted at `/proc` unless a test points it elsewhere.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct StdProcs {
    root: AbsDir,
}

impl Default for StdProcs {
    fn default() -> Self {
        Self::at(AbsDir::root().join_dir(&PROC))
    }
}

impl StdProcs {
    /// Reads `root/PID/stat` and `root/PID/exe`; for tests with a fake procfs.
    #[must_use]
    pub const fn at(root: AbsDir) -> Self {
        Self { root }
    }

    fn entry(&self, pid: Pid, file: &str) -> PathBuf {
        PathBuf::from(format!("{}/{pid}/{file}", self.root))
    }
}

/// The process has no `/proc` entry (any more).
fn is_gone(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::NotFound
        || error.raw_os_error() == Some(Errno::SRCH.raw_os_error())
}

/// What `/proc/PID/exe` says.
#[derive(Clone, PartialEq, Eq, Debug)]
enum Exe {
    /// No link: the process is gone, a zombie or a kernel thread.
    Gone,
    /// Not ours to read (another user's process).
    Unreadable,
    Program(ProgramName),
}

impl StdProcs {
    /// `None` when the process is gone.
    fn start_time(&self, pid: Pid) -> Result<Option<StartTime>, ProcError> {
        let path = self.entry(pid, "stat");
        match fs::read(&path) {
            Ok(bytes) => parse_stat(&String::from_utf8_lossy(&bytes))
                .map(Some)
                .map_err(|error| ProcError::Stat { pid, error }),
            Err(error) if is_gone(&error) => Ok(None),
            Err(error) => Err(ProcError::Io {
                op: ProcOp::ReadStat,
                pid,
                cause: Cause::of(&error),
            }),
        }
    }

    fn exe(&self, pid: Pid) -> Result<Exe, ProcError> {
        let target = match fs::read_link(self.entry(pid, "exe")) {
            Ok(target) => target,
            Err(error) if is_gone(&error) => return Ok(Exe::Gone),
            Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
                return Ok(Exe::Unreadable);
            }
            Err(error) => {
                return Err(ProcError::Io {
                    op: ProcOp::ReadExe,
                    pid,
                    cause: Cause::of(&error),
                });
            }
        };
        let name = match target.file_name() {
            None => String::new(),
            Some(name) => name
                .to_str()
                .ok_or(ProcError::NotUnicode { pid })?
                .to_owned(),
        };
        ProgramName::parse(name)
            .map(Exe::Program)
            .map_err(|error| ProcError::Program { pid, error })
    }

    fn identify_now(&self, pid: Pid) -> Result<ProcIdentity, ProcError> {
        let Some(start) = self.start_time(pid)? else {
            return Err(ProcError::NoSuchProcess { pid });
        };
        match self.exe(pid)? {
            Exe::Program(program) => Ok(ProcIdentity {
                pid,
                start,
                program,
            }),
            Exe::Gone => Err(ProcError::NoSuchProcess { pid }),
            Exe::Unreadable => Err(ProcError::Io {
                op: ProcOp::ReadExe,
                pid,
                cause: Cause {
                    kind: io::ErrorKind::PermissionDenied,
                    errno: Some(Errno::ACCESS.raw_os_error()),
                },
            }),
        }
    }

    fn presence_now(&self, who: &ProcIdentity) -> Result<Presence, ProcError> {
        let Some(start) = self.start_time(who.pid)? else {
            return Ok(Presence::Gone);
        };
        if start != who.start {
            return Ok(Presence::Replaced);
        }
        Ok(match self.exe(who.pid)? {
            Exe::Gone => Presence::Gone,
            Exe::Program(program) if program == who.program => Presence::Alive,
            Exe::Unreadable | Exe::Program(_) => Presence::Replaced,
        })
    }

    fn signal_now(&self, who: &ProcIdentity, signal: Signal) -> Result<Signalled, ProcError> {
        // A pid beyond i32::MAX names no process.
        let Some(raw) = i32::try_from(*who.pid.get())
            .ok()
            .and_then(process::Pid::from_raw)
        else {
            return Ok(Signalled::Gone);
        };
        let handle = match process::pidfd_open(raw, PidfdFlags::empty()) {
            Ok(handle) => handle,
            Err(Errno::SRCH) => return Ok(Signalled::Gone),
            Err(errno) => {
                return Err(ProcError::Io {
                    op: ProcOp::Open,
                    pid: who.pid,
                    cause: Cause::of(&io::Error::from(errno)),
                });
            }
        };
        match self.presence_now(who)? {
            Presence::Gone => return Ok(Signalled::Gone),
            Presence::Replaced => return Ok(Signalled::Replaced),
            Presence::Alive => {}
        }
        let number = match signal {
            Signal::Terminate => process::Signal::TERM,
            Signal::Kill => process::Signal::KILL,
        };
        match process::pidfd_send_signal(&handle, number) {
            Ok(()) => Ok(Signalled::Sent),
            Err(Errno::SRCH) => Ok(Signalled::Gone),
            Err(errno) => Err(ProcError::Io {
                op: ProcOp::Signal,
                pid: who.pid,
                cause: Cause::of(&io::Error::from(errno)),
            }),
        }
    }
}

/// `/dev/null` for stdin unless the child shares the terminal.
fn plain_stdin(io: &ChildIo) -> Stdio {
    match io {
        ChildIo::Inherit => Stdio::inherit(),
        ChildIo::Null | ChildIo::Log(_) => Stdio::null(),
    }
}

/// Stdout and stderr for `io`.
fn outputs(io: &ChildIo) -> Result<(Stdio, Stdio), SpawnError> {
    match io {
        ChildIo::Null => Ok((Stdio::null(), Stdio::null())),
        ChildIo::Inherit => Ok((Stdio::inherit(), Stdio::inherit())),
        ChildIo::Log(path) => {
            let log_error = |error: &io::Error| SpawnError::Log {
                path: path.clone(),
                cause: Cause::of(error),
            };
            let file = fs::OpenOptions::new()
                .append(true)
                .create(true)
                .mode(0o600)
                .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits().cast_signed())
                .open(path.get())
                .map_err(|error| log_error(&error))?;
            let second = file.try_clone().map_err(|error| log_error(&error))?;
            Ok((Stdio::from(file), Stdio::from(second)))
        }
    }
}

/// A process started by [`StdProcs`]: terminated when dropped, unless released or waited for.
/// The signal goes through the child handle: while the child is unreaped its pid is ours, whatever
/// `/proc/PID/exe` says after the runner's exec.
#[derive(Debug)]
pub struct StdChild {
    procs: StdProcs,
    child: tokio::process::Child,
    identity: ProcIdentity,
    /// The child was waited for or released: nothing left to signal.
    reaped: bool,
}

impl Drop for StdChild {
    fn drop(&mut self) {
        if self.reaped {
            return;
        }
        // Best effort; a guard cannot report. An unreaped child keeps its pid, so this can't hit
        // a stranger.
        let target = self
            .child
            .id()
            .and_then(|raw| i32::try_from(raw).ok())
            .and_then(process::Pid::from_raw);
        if let Some(pid) = target {
            let _ = process::kill_process(pid, process::Signal::TERM);
        }
    }
}

impl Spawned for StdChild {
    fn identity(&self) -> &ProcIdentity {
        &self.identity
    }

    async fn settle(&mut self) -> Result<ProcIdentity, ProcError> {
        let (procs, pid, start) = (self.procs.clone(), self.identity.pid, self.identity.start);
        let now = blocking(pid, move || procs.identify_now(pid)).await?;
        if now.start != start {
            return Err(ProcError::NoSuchProcess { pid });
        }
        self.identity = now.clone();
        Ok(now)
    }

    fn release(mut self) -> ProcIdentity {
        self.reaped = true;
        self.identity.clone()
    }

    async fn wait(&mut self) -> Result<Exit, ProcError> {
        let status = self.child.wait().await.map_err(|error| ProcError::Io {
            op: ProcOp::Wait,
            pid: self.identity.pid,
            cause: Cause::of(&error),
        })?;
        self.reaped = true;
        Ok(exit_of(status))
    }
}

fn exit_of(status: std::process::ExitStatus) -> Exit {
    match (status.code(), status.signal()) {
        (Some(code), _) => Exit::Code(code),
        (None, Some(signal)) => Exit::Signal(signal),
        (None, None) => Exit::Code(status.into_raw()),
    }
}

/// A child that could not be identified: if it has already ended (a zombie has no readable
/// `exe`), how it ended, which the caller needs more than the failed lookup; else it is told to
/// stop and the lookup failure is the error.
fn unidentified(child: &mut tokio::process::Child, error: ProcError) -> SpawnError {
    if let Ok(Some(status)) = child.try_wait() {
        return SpawnError::Exited {
            exit: exit_of(status),
        };
    }
    let _ = child.start_kill();
    SpawnError::Identify(error)
}

impl StdProcs {
    /// Starts `program`; needs a tokio runtime context. No `.await` follows the spawn.
    fn spawn_now<H: HeldLock>(
        &self,
        program: &str,
        args: &[String],
        cwd: &AbsDir,
        lock: LockHandoff<'_, H>,
        io: &ChildIo,
    ) -> Result<StdChild, SpawnError> {
        let mut command = tokio::process::Command::new(program);
        command.args(args).current_dir(cwd.get());
        match io {
            ChildIo::Inherit => {}
            // A group of its own: Ctrl-C and the terminal's other group signals miss it. (A new
            // session would also cut the controlling terminal, but `setsid` needs `unsafe`.)
            ChildIo::Null | ChildIo::Log(_) => {
                command.process_group(0);
            }
        }
        let (stdout, stderr) = outputs(io)?;
        command.stdout(stdout).stderr(stderr);
        // Open across the spawn so the child inherits it; dropped right after.
        let mut handed = None;
        match lock {
            LockHandoff::Stdin(held) => {
                command.stdin(held.stdin().map_err(SpawnError::Lock)?);
            }
            LockHandoff::Inherited(held) => {
                handed = Some(held.inheritable().map_err(SpawnError::Lock)?);
                command.stdin(plain_stdin(io));
            }
            LockHandoff::None => {
                command.stdin(plain_stdin(io));
            }
        }
        let spawned = command.spawn();
        drop(handed);
        let mut child = spawned.map_err(|error| SpawnError::Exec {
            cause: Cause::of(&error),
        })?;
        // From here the child must not outlive an early return.
        let Some(pid) = child.id().and_then(|raw| Pid::parse(raw).ok()) else {
            let _ = child.start_kill();
            return Err(SpawnError::Exec {
                cause: Cause {
                    kind: io::ErrorKind::InvalidData,
                    errno: None,
                },
            });
        };
        match self.identify_now(pid) {
            Ok(identity) => Ok(StdChild {
                procs: self.clone(),
                child,
                identity,
                reaped: false,
            }),
            Err(error) => Err(unidentified(&mut child, error)),
        }
    }
}

async fn blocking<T: Send + 'static>(
    pid: Pid,
    work: impl FnOnce() -> Result<T, ProcError> + Send + 'static,
) -> Result<T, ProcError> {
    tokio::task::spawn_blocking(work)
        .await
        .unwrap_or(Err(ProcError::Aborted { pid }))
}

impl Procs for StdProcs {
    type Child = StdChild;

    fn spawn<H: HeldLock>(
        &self,
        argv: &Argv,
        cwd: &AbsDir,
        lock: LockHandoff<'_, H>,
        io: ChildIo,
    ) -> Result<StdChild, SpawnError> {
        self.spawn_now(argv.program(), argv.args(), cwd, lock, &io)
    }

    fn identify(&self, pid: Pid) -> impl Future<Output = Result<ProcIdentity, ProcError>> + Send {
        let procs = self.clone();
        blocking(pid, move || procs.identify_now(pid))
    }

    fn presence(
        &self,
        who: &ProcIdentity,
    ) -> impl Future<Output = Result<Presence, ProcError>> + Send {
        let (procs, who) = (self.clone(), who.clone());
        blocking(who.pid, move || procs.presence_now(&who))
    }

    fn signal(
        &self,
        who: &ProcIdentity,
        signal: Signal,
    ) -> impl Future<Output = Result<Signalled, ProcError>> + Send {
        let (procs, who) = (self.clone(), who.clone());
        blocking(who.pid, move || procs.signal_now(&who, signal))
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use std::os::unix::ffi::OsStrExt as _;
    use std::os::unix::fs::symlink;
    use std::os::unix::process::CommandExt as _;
    use std::process::{Child, Command};
    use std::time::{Duration, Instant};

    use super::*;
    use crate::domain::AbsPath;
    use crate::testing::{self, TempDir};

    /// A copy of `sleep` under another executable name, running for a minute; killed on drop.
    struct Sleeper {
        child: Child,
        id: ProcIdentity,
    }

    impl Drop for Sleeper {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    fn sleep_binary() -> PathBuf {
        let path = std::env::var_os("PATH").unwrap();
        std::env::split_paths(&path)
            .map(|dir| dir.join("sleep"))
            .find(|candidate| candidate.is_file())
            .unwrap()
    }

    fn program_name(text: &str) -> ProgramName {
        ProgramName::parse(text.to_owned()).unwrap()
    }

    fn pid(n: u32) -> Pid {
        Pid::parse(n).unwrap()
    }

    /// Starts `sleep 60` as `bin/NAME` with `argv[0]` set to `shown`.
    fn spawn(temp: &TempDir, name: &str, shown: &str) -> Sleeper {
        // A fork elsewhere that holds the copy's write descriptor makes exec fail with "text file
        // busy"; one test at a time copies and starts.
        let _one_at_a_time = testing::one_spawn_at_a_time();
        let bin = temp.path().join("bin");
        fs::create_dir_all(&bin).unwrap();
        let exe = bin.join(name);
        fs::copy(sleep_binary(), &exe).unwrap();
        let child = Command::new(&exe).arg0(shown).arg("60").spawn().unwrap();
        let id = StdProcs::default().identify_now(pid(child.id())).unwrap();
        Sleeper { child, id }
    }

    fn procs() -> StdProcs {
        StdProcs::default()
    }

    fn stat_of(pid: Pid) -> StartTime {
        let text = fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
        parse_stat(&text).unwrap()
    }

    fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
        let deadline = Instant::now().checked_add(Duration::from_secs(10)).unwrap();
        while !done() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn identity_is_the_pid_the_start_time_and_the_executable_not_argv0() {
        let temp = TempDir::new("procs-identity").unwrap();
        let sleeper = spawn(&temp, "virtiofsd", "microvm@host");
        assert_eq!(sleeper.id.pid, pid(sleeper.child.id()));
        assert_eq!(sleeper.id.start, stat_of(sleeper.id.pid));
        assert_eq!(sleeper.id.program.get(), "virtiofsd");
    }

    #[test]
    fn a_running_process_with_its_recorded_identity_is_alive() {
        let temp = TempDir::new("procs-alive").unwrap();
        let sleeper = spawn(&temp, "qemu-system-x86_64", "qemu");
        assert_eq!(procs().presence_now(&sleeper.id), Ok(Presence::Alive));
    }

    #[test]
    fn a_reaped_process_is_gone() {
        let temp = TempDir::new("procs-reaped").unwrap();
        let mut sleeper = spawn(&temp, "virtiofsd", "virtiofsd");
        sleeper.child.kill().unwrap();
        sleeper.child.wait().unwrap();
        assert_eq!(procs().presence_now(&sleeper.id), Ok(Presence::Gone));
        assert_eq!(
            procs().signal_now(&sleeper.id, Signal::Terminate),
            Ok(Signalled::Gone)
        );
    }

    #[test]
    fn an_exited_process_that_is_not_reaped_yet_is_gone() {
        let temp = TempDir::new("procs-zombie").unwrap();
        let mut sleeper = spawn(&temp, "virtiofsd", "virtiofsd");
        sleeper.child.kill().unwrap();
        let stat = format!("/proc/{}/stat", sleeper.id.pid);
        wait_until("the process to become a zombie", || {
            fs::read_to_string(&stat).is_ok_and(|text| text.contains(") Z "))
        });
        assert!(fs::metadata(&stat).is_ok(), "the zombie still has a status");
        assert_eq!(procs().presence_now(&sleeper.id), Ok(Presence::Gone));
        assert_eq!(
            procs().signal_now(&sleeper.id, Signal::Kill),
            Ok(Signalled::Gone)
        );
    }

    #[test]
    fn a_recycled_pid_is_replaced_whether_the_start_time_or_the_program_differs() {
        let temp = TempDir::new("procs-recycled").unwrap();
        let stranger = spawn(&temp, "not-ours", "not-ours");
        let recorded = |start: u64, program: &str| ProcIdentity {
            pid: stranger.id.pid,
            start: StartTime::parse(start).unwrap(),
            program: ProgramName::parse(program.to_owned()).unwrap(),
        };
        let start = *stranger.id.start.get();
        testing::table(
            &[
                (recorded(start, "not-ours"), Presence::Alive),
                (
                    recorded(start.wrapping_add(1), "not-ours"),
                    Presence::Replaced,
                ),
                (recorded(0, "not-ours"), Presence::Replaced),
                (recorded(start, "virtiofsd"), Presence::Replaced),
                (recorded(start, "not-ours-2"), Presence::Replaced),
                (recorded(start, "sleep"), Presence::Replaced),
            ],
            |who| procs().presence_now(who).unwrap(),
        );
    }

    #[test]
    fn a_process_of_another_user_is_replaced() {
        // Pid 1 belongs to root: its executable can't be read by anyone else, and for root the
        // program name differs from the one recorded here.
        let init = pid(1);
        let who = ProcIdentity {
            pid: init,
            start: stat_of(init),
            program: ProgramName::parse("amoeboid-test-program".to_owned()).unwrap(),
        };
        assert_eq!(procs().presence_now(&who), Ok(Presence::Replaced));
    }

    #[test]
    fn a_verified_process_gets_the_signal() {
        let temp = TempDir::new("procs-term").unwrap();
        let mut sleeper = spawn(&temp, "virtiofsd", "virtiofsd");
        assert_eq!(
            procs().signal_now(&sleeper.id, Signal::Terminate),
            Ok(Signalled::Sent)
        );
        let status = sleeper.child.wait().unwrap();
        assert_eq!(status.signal(), Some(15));
    }

    #[test]
    fn kill_is_sent_as_sigkill() {
        let temp = TempDir::new("procs-kill").unwrap();
        let mut sleeper = spawn(&temp, "virtiofsd", "virtiofsd");
        assert_eq!(
            procs().signal_now(&sleeper.id, Signal::Kill),
            Ok(Signalled::Sent)
        );
        assert_eq!(sleeper.child.wait().unwrap().signal(), Some(9));
    }

    #[test]
    fn a_stranger_under_a_recorded_pid_gets_no_signal() {
        let temp = TempDir::new("procs-stranger").unwrap();
        let mut stranger = spawn(&temp, "not-ours", "not-ours");
        let wrong_program = ProcIdentity {
            program: ProgramName::parse("virtiofsd".to_owned()).unwrap(),
            ..stranger.id.clone()
        };
        let wrong_start = ProcIdentity {
            start: StartTime::parse(stranger.id.start.get().wrapping_add(1)).unwrap(),
            ..stranger.id.clone()
        };
        for who in [wrong_program, wrong_start] {
            for signal in [Signal::Terminate, Signal::Kill] {
                assert_eq!(procs().signal_now(&who, signal), Ok(Signalled::Replaced));
            }
        }
        assert_eq!(stranger.child.try_wait().unwrap(), None, "still running");
    }

    /// A fake procfs with one process whose status and executable are as given.
    fn fake(temp: &TempDir, stat: Option<&str>, exe: Option<&[u8]>) -> StdProcs {
        let entry = temp.path().join("123");
        fs::create_dir(&entry).unwrap();
        if let Some(stat) = stat {
            fs::write(entry.join("stat"), stat).unwrap();
        }
        if let Some(exe) = exe {
            symlink(std::ffi::OsStr::from_bytes(exe), entry.join("exe")).unwrap();
        }
        StdProcs::at(AbsDir::normalising(temp.path().to_str().unwrap()).unwrap())
    }

    const STAT: &str = "123 (sleep) S 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 777 20 21\n";

    #[test]
    fn a_fake_procfs_is_read_like_the_real_one() {
        let temp = TempDir::new("procs-fake").unwrap();
        let procs = fake(
            &temp,
            Some(STAT),
            Some(b"/nix/store/x-qemu/bin/qemu-system-x86_64"),
        );
        let who = procs.identify_now(pid(123)).unwrap();
        assert_eq!(who.start, StartTime::parse(777).unwrap());
        assert_eq!(who.program.get(), "qemu-system-x86_64");
        assert_eq!(procs.presence_now(&who), Ok(Presence::Alive));
    }

    #[test]
    fn a_deleted_executable_keeps_the_kernels_suffix_and_so_is_replaced() {
        let temp = TempDir::new("procs-deleted").unwrap();
        let procs = fake(&temp, Some(STAT), Some(b"/bin/qemu (deleted)"));
        let who = procs.identify_now(pid(123)).unwrap();
        assert_eq!(who.program.get(), "qemu (deleted)");
        let recorded = ProcIdentity {
            program: ProgramName::parse("qemu".to_owned()).unwrap(),
            ..who
        };
        assert_eq!(procs.presence_now(&recorded), Ok(Presence::Replaced));
    }

    type Case<'a> = (
        Option<&'a str>,
        Option<&'a [u8]>,
        Result<Presence, ProcError>,
    );

    #[test]
    fn unexpected_content_of_the_procfs_maps_to_typed_errors() {
        let id = |program: &str| ProcIdentity {
            pid: pid(123),
            start: StartTime::parse(777).unwrap(),
            program: ProgramName::parse(program.to_owned()).unwrap(),
        };
        let cases: [Case<'_>; 6] = [
            (
                Some("garbage"),
                Some(b"/bin/sleep"),
                Err(ProcError::Stat {
                    pid: pid(123),
                    error: crate::domain::StatError::NoName,
                }),
            ),
            (
                Some(STAT),
                Some(b"/"),
                Err(ProcError::Program {
                    pid: pid(123),
                    error: crate::domain::ProgramNameError::Empty,
                }),
            ),
            (
                Some(STAT),
                Some(b"/bin/\xff"),
                Err(ProcError::NotUnicode { pid: pid(123) }),
            ),
            (None, Some(b"/bin/sleep"), Ok(Presence::Gone)),
            (Some(STAT), None, Ok(Presence::Gone)),
            (Some(STAT), Some(b"/bin/sleep"), Ok(Presence::Alive)),
        ];
        for (index, (stat, exe, expected)) in cases.into_iter().enumerate() {
            let temp = TempDir::new("procs-unexpected").unwrap();
            let procs = fake(&temp, stat, exe);
            assert_eq!(procs.presence_now(&id("sleep")), expected, "case {index}");
        }
    }

    #[test]
    fn identifying_a_missing_process_or_an_unreadable_status_is_a_typed_error() {
        let temp = TempDir::new("procs-identify-errors").unwrap();
        let procs = StdProcs::at(AbsDir::normalising(temp.path().to_str().unwrap()).unwrap());
        assert_eq!(
            procs.identify_now(pid(123)),
            Err(ProcError::NoSuchProcess { pid: pid(123) })
        );
        // The status is a directory: reading it fails with something other than "not found".
        fs::create_dir_all(temp.path().join("123/stat")).unwrap();
        let error = procs.identify_now(pid(123)).unwrap_err();
        assert!(
            matches!(
                error,
                ProcError::Io {
                    op: ProcOp::ReadStat,
                    ..
                }
            ),
            "{error:?}"
        );
        // A status without an executable link is a zombie or a kernel thread.
        let temp = TempDir::new("procs-identify-zombie").unwrap();
        let procs = fake(&temp, Some(STAT), None);
        assert_eq!(
            procs.identify_now(pid(123)),
            Err(ProcError::NoSuchProcess { pid: pid(123) })
        );
    }

    #[test]
    fn identifying_a_process_whose_executable_is_unreadable_is_a_permission_error() {
        if rustix::process::geteuid().is_root() {
            return; // root can read every link
        }
        let init = procs().identify_now(pid(1));
        assert_eq!(
            init,
            Err(ProcError::Io {
                op: ProcOp::ReadExe,
                pid: pid(1),
                cause: Cause {
                    kind: io::ErrorKind::PermissionDenied,
                    errno: Some(Errno::ACCESS.raw_os_error())
                }
            })
        );
    }

    #[test]
    fn the_port_identifies_checks_and_signals_through_the_blocking_pool() {
        let temp = TempDir::new("procs-port").unwrap();
        let mut sleeper = spawn(&temp, "virtiofsd", "virtiofsd");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        runtime.block_on(async {
            let procs = StdProcs::default();
            let who = procs.identify(sleeper.id.pid).await.unwrap();
            assert_eq!(who, sleeper.id);
            assert_eq!(procs.presence(&who).await, Ok(Presence::Alive));
            assert_eq!(
                procs.signal(&who, Signal::Terminate).await,
                Ok(Signalled::Sent)
            );
        });
        sleeper.child.wait().unwrap();
        runtime.block_on(async {
            let procs = StdProcs::default();
            assert_eq!(procs.presence(&sleeper.id).await, Ok(Presence::Gone));
        });
    }

    // Spawning through the port (real processes).

    use std::os::unix::fs::PermissionsExt as _;

    use crate::adapters::StdLock;
    use crate::ports::vm_lock::{Availability, VmLock as _};

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }

    /// A copy of `sleep` as `bin/NAME`.
    fn sleep_copy(temp: &TempDir, name: &str) -> String {
        let bin = temp.path().join("bin");
        fs::create_dir_all(&bin).unwrap();
        let exe = bin.join(name);
        fs::copy(sleep_binary(), &exe).unwrap();
        exe.to_str().unwrap().to_owned()
    }

    fn no_lock() -> LockHandoff<'static, crate::adapters::HeldFile> {
        LockHandoff::None
    }

    fn sleeping(seconds: &str) -> Vec<String> {
        vec![seconds.to_owned()]
    }

    fn sh(script: &str) -> Vec<String> {
        vec!["-c".to_owned(), script.to_owned()]
    }

    #[test]
    fn a_spawned_child_is_identified_before_the_call_returns() {
        let _one = testing::one_spawn_at_a_time();
        let temp = TempDir::new("procs-spawn-identity").unwrap();
        let program = sleep_copy(&temp, "virtiofsd");
        let rt = runtime();
        let _guard = rt.enter();
        let child = procs()
            .spawn_now(
                &program,
                &sleeping("60"),
                &AbsDir::root(),
                no_lock(),
                &ChildIo::Null,
            )
            .unwrap();
        let id = child.identity().clone();
        assert_eq!(id.program.get(), "virtiofsd");
        assert_eq!(id.start, stat_of(id.pid));
        assert_eq!(procs().presence_now(&id), Ok(Presence::Alive));
    }

    #[test]
    fn dropping_a_child_sends_sigterm_to_its_verified_identity() {
        let _one = testing::one_spawn_at_a_time();
        let temp = TempDir::new("procs-spawn-drop").unwrap();
        let program = sleep_copy(&temp, "virtiofsd");
        let rt = runtime();
        let _guard = rt.enter();
        let child = procs()
            .spawn_now(
                &program,
                &sleeping("60"),
                &AbsDir::root(),
                no_lock(),
                &ChildIo::Null,
            )
            .unwrap();
        let id = child.identity().clone();
        drop(child);
        wait_until("the dropped child to end", || {
            procs().presence_now(&id) == Ok(Presence::Gone)
        });
    }

    #[test]
    fn a_child_runs_in_the_given_directory() {
        let _one = testing::one_spawn_at_a_time();
        let temp = TempDir::new("procs-spawn-cwd").unwrap();
        let dir = AbsDir::normalising(temp.path().to_str().unwrap()).unwrap();
        let out = AbsPath::normalising(temp.path().join("pwd.out").to_str().unwrap()).unwrap();
        let rt = runtime();
        rt.block_on(async {
            let mut child = procs()
                .spawn_now(
                    "sh",
                    &sh("pwd -P > pwd.out"),
                    &dir,
                    no_lock(),
                    &ChildIo::Null,
                )
                .unwrap();
            child.wait().await.unwrap();
        });
        let seen = fs::read_to_string(out.get()).unwrap();
        let expected = fs::canonicalize(temp.path()).unwrap();
        assert_eq!(seen.trim_end(), expected.to_str().unwrap());
    }

    #[test]
    fn only_a_child_without_the_terminal_gets_a_process_group_of_its_own() {
        let _one = testing::one_spawn_at_a_time();
        let rt = runtime();
        rt.block_on(async {
            let mine = rustix::process::getpgrp();
            for (io, own) in [(ChildIo::Null, true), (ChildIo::Inherit, false)] {
                let mut child = procs()
                    .spawn_now("sleep", &sleeping("60"), &AbsDir::root(), no_lock(), &io)
                    .unwrap();
                let raw = i32::try_from(*child.identity().pid.get()).unwrap();
                let group = rustix::process::getpgid(rustix::process::Pid::from_raw(raw)).unwrap();
                assert_eq!(group != mine, own, "{io:?}");
                let id = child.identity().clone();
                procs().signal_now(&id, Signal::Kill).unwrap();
                child.wait().await.unwrap();
            }
        });
    }

    #[test]
    fn settling_follows_a_child_that_execs_another_program() {
        let _one = testing::one_spawn_at_a_time();
        let temp = TempDir::new("procs-spawn-settle").unwrap();
        let program = sleep_copy(&temp, "qemu-system-test");
        let rt = runtime();
        rt.block_on(async {
            // The script sleeps, then becomes `qemu-system-test`, as the runner becomes qemu.
            let mut child = procs()
                .spawn_now(
                    "sh",
                    &sh(&format!("sleep 0.2; exec {program} 60")),
                    &AbsDir::root(),
                    no_lock(),
                    &ChildIo::Null,
                )
                .unwrap();
            let before = child.identity().clone();
            assert_ne!(before.program.get(), "qemu-system-test");
            wait_until("the exec", || {
                procs()
                    .exe(before.pid)
                    .is_ok_and(|exe| exe == Exe::Program(program_name("qemu-system-test")))
            });
            let after = child.settle().await.unwrap();
            assert_eq!(after.pid, before.pid);
            assert_eq!(after.start, before.start);
            assert_eq!(after.program.get(), "qemu-system-test");
            assert_eq!(child.identity(), &after);
            assert_eq!(procs().presence_now(&after), Ok(Presence::Alive));
            assert_eq!(procs().presence_now(&before), Ok(Presence::Replaced));
        });
    }

    #[test]
    fn settling_a_child_that_has_gone_is_an_error() {
        let _one = testing::one_spawn_at_a_time();
        let rt = runtime();
        rt.block_on(async {
            let mut child = procs()
                .spawn_now(
                    "sleep",
                    &sleeping("60"),
                    &AbsDir::root(),
                    no_lock(),
                    &ChildIo::Null,
                )
                .unwrap();
            let id = child.identity().clone();
            procs().signal_now(&id, Signal::Kill).unwrap();
            wait_until("the child to die", || {
                procs().presence_now(&id) == Ok(Presence::Gone)
            });
            assert_eq!(
                child.settle().await,
                Err(ProcError::NoSuchProcess { pid: id.pid })
            );
            child.wait().await.unwrap();
        });
    }

    #[test]
    fn dropping_a_child_that_became_another_program_still_ends_it() {
        let _one = testing::one_spawn_at_a_time();
        let temp = TempDir::new("procs-spawn-exec-drop").unwrap();
        let program = sleep_copy(&temp, "qemu-system-test");
        let rt = runtime();
        let _guard = rt.enter();
        let child = procs()
            .spawn_now(
                "sh",
                &sh(&format!("exec {program} 60")),
                &AbsDir::root(),
                no_lock(),
                &ChildIo::Null,
            )
            .unwrap();
        let pid = child.identity().pid;
        wait_until("the exec", || {
            procs()
                .exe(pid)
                .is_ok_and(|exe| exe == Exe::Program(program_name("qemu-system-test")))
        });
        drop(child);
        wait_until("the dropped child to end", || {
            procs().start_time(pid).ok().flatten().is_none()
                || fs::read_to_string(format!("/proc/{pid}/stat"))
                    .is_ok_and(|stat| stat.contains(") Z "))
        });
    }

    #[test]
    fn a_released_child_is_not_signalled_when_its_handle_goes() {
        let _one = testing::one_spawn_at_a_time();
        let rt = runtime();
        let _guard = rt.enter();
        let child = procs()
            .spawn_now(
                "sleep",
                &sleeping("60"),
                &AbsDir::root(),
                no_lock(),
                &ChildIo::Null,
            )
            .unwrap();
        let expected = child.identity().clone();
        let id = child.release();
        assert_eq!(id, expected);
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert_eq!(procs().presence_now(&id), Ok(Presence::Alive));
        assert_eq!(procs().signal_now(&id, Signal::Kill), Ok(Signalled::Sent));
    }

    #[test]
    fn waiting_reports_the_exit_code_or_the_signal() {
        let _one = testing::one_spawn_at_a_time();
        let temp = TempDir::new("procs-spawn-wait").unwrap();
        let program = sleep_copy(&temp, "virtiofsd");
        let rt = runtime();
        rt.block_on(async {
            let mut coded = procs()
                .spawn_now(
                    "sh",
                    &sh("sleep 0.3; exit 3"),
                    &AbsDir::root(),
                    no_lock(),
                    &ChildIo::Null,
                )
                .unwrap();
            assert_eq!(coded.wait().await, Ok(Exit::Code(3)));
            let mut killed = procs()
                .spawn_now(
                    &program,
                    &sleeping("60"),
                    &AbsDir::root(),
                    no_lock(),
                    &ChildIo::Null,
                )
                .unwrap();
            let id = killed.identity().clone();
            assert_eq!(procs().signal_now(&id, Signal::Kill), Ok(Signalled::Sent));
            assert_eq!(killed.wait().await, Ok(Exit::Signal(9)));
        });
    }

    #[test]
    fn a_missing_program_is_an_exec_error() {
        let _one = testing::one_spawn_at_a_time();
        let rt = runtime();
        let _guard = rt.enter();
        let error = procs()
            .spawn_now(
                "/nonexistent/amoeboid-test",
                &[],
                &AbsDir::root(),
                no_lock(),
                &ChildIo::Null,
            )
            .unwrap_err();
        assert_eq!(
            error,
            SpawnError::Exec {
                cause: Cause {
                    kind: io::ErrorKind::NotFound,
                    errno: Some(2)
                }
            }
        );
    }

    #[test]
    fn a_child_that_ended_before_it_was_identified_reports_how_it_ended() {
        let _one = testing::one_spawn_at_a_time();
        let rt = runtime();
        let _guard = rt.enter();
        let mut child = tokio::process::Command::new("sh")
            .args(["-c", "exit 7"])
            .spawn()
            .unwrap();
        let raw = child.id().unwrap();
        // A zombie: ended, not reaped yet.
        wait_until("the child to end", || {
            fs::read_to_string(format!("/proc/{raw}/stat")).is_ok_and(|stat| {
                stat.rsplit_once(") ")
                    .is_some_and(|(_, rest)| rest.starts_with('Z'))
            })
        });
        let error = ProcError::NoSuchProcess { pid: pid(raw) };
        assert_eq!(
            unidentified(&mut child, error),
            SpawnError::Exited {
                exit: Exit::Code(7)
            }
        );
    }

    #[test]
    fn a_child_that_cannot_be_identified_is_stopped_and_reported() {
        let _one = testing::one_spawn_at_a_time();
        let temp = TempDir::new("procs-spawn-unidentified").unwrap();
        let name = format!("amoeboid-unidentified-{}", std::process::id());
        let program = sleep_copy(&temp, &name);
        let empty = TempDir::new("procs-spawn-empty-proc").unwrap();
        let blind = StdProcs::at(AbsDir::normalising(empty.path().to_str().unwrap()).unwrap());
        let rt = runtime();
        let _guard = rt.enter();
        let error = blind
            .spawn_now(
                &program,
                &sleeping("60"),
                &AbsDir::root(),
                no_lock(),
                &ChildIo::Null,
            )
            .unwrap_err();
        assert!(matches!(error, SpawnError::Identify(_)), "{error:?}");
        let running = || {
            fs::read_dir("/proc").unwrap().any(|entry| {
                fs::read_link(entry.unwrap().path().join("exe"))
                    .is_ok_and(|exe| exe.file_name().is_some_and(|n| n == name.as_str()))
            })
        };
        wait_until("the unidentified child to be stopped", || !running());
    }

    #[test]
    fn a_log_receives_stdout_and_stderr_appended_and_is_private() {
        let _one = testing::one_spawn_at_a_time();
        let temp = TempDir::new("procs-spawn-log").unwrap();
        let log = AbsPath::normalising(temp.path().join("console.log").to_str().unwrap()).unwrap();
        let rt = runtime();
        rt.block_on(async {
            for script in [
                "echo out1; echo err1 >&2; sleep 0.2",
                "echo out2; sleep 0.2",
            ] {
                let mut child = procs()
                    .spawn_now(
                        "sh",
                        &sh(script),
                        &AbsDir::root(),
                        no_lock(),
                        &ChildIo::Log(log.clone()),
                    )
                    .unwrap();
                child.wait().await.unwrap();
            }
        });
        let text = fs::read_to_string(log.get()).unwrap();
        let mut lines: Vec<&str> = text.lines().collect();
        lines.sort_unstable();
        assert_eq!(lines, ["err1", "out1", "out2"]);
        let mode = fs::metadata(log.get()).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn a_log_that_cannot_be_opened_is_a_log_error_and_starts_nothing() {
        let temp = TempDir::new("procs-spawn-badlog").unwrap();
        let log = AbsPath::normalising(temp.path().join("missing/console.log").to_str().unwrap())
            .unwrap();
        let rt = runtime();
        let _guard = rt.enter();
        let error = procs()
            .spawn_now(
                "true",
                &[],
                &AbsDir::root(),
                no_lock(),
                &ChildIo::Log(log.clone()),
            )
            .unwrap_err();
        assert_eq!(
            error,
            SpawnError::Log {
                path: log,
                cause: Cause {
                    kind: io::ErrorKind::NotFound,
                    errno: Some(2)
                }
            }
        );
    }

    #[test]
    fn a_child_given_the_lock_holds_it_until_it_ends_whichever_way_it_was_handed() {
        let _one = testing::one_spawn_at_a_time();
        let temp = TempDir::new("procs-spawn-lock").unwrap();
        let program = sleep_copy(&temp, "virtiofsd");
        let lock_path = AbsPath::normalising(temp.path().join("lock").to_str().unwrap()).unwrap();
        let rt = runtime();
        rt.block_on(async {
            for stdin in [true, false] {
                let held = StdLock.acquire(&lock_path).await.unwrap();
                let handoff = if stdin {
                    LockHandoff::Stdin(&held)
                } else {
                    LockHandoff::Inherited(&held)
                };
                let mut child = procs()
                    .spawn_now(
                        &program,
                        &sleeping("60"),
                        &AbsDir::root(),
                        handoff,
                        &ChildIo::Null,
                    )
                    .unwrap();
                drop(held);
                assert_eq!(StdLock.probe(&lock_path).await, Ok(Availability::Held));
                let id = child.identity().clone();
                procs().signal_now(&id, Signal::Kill).unwrap();
                child.wait().await.unwrap();
                assert_eq!(StdLock.probe(&lock_path).await, Ok(Availability::Free));
            }
        });
    }

    #[test]
    fn a_child_spawned_without_the_lock_leaves_it_free_when_the_launcher_drops_it() {
        let _one = testing::one_spawn_at_a_time();
        let temp = TempDir::new("procs-spawn-nolock").unwrap();
        let program = sleep_copy(&temp, "virtiofsd");
        let lock_path = AbsPath::normalising(temp.path().join("lock").to_str().unwrap()).unwrap();
        let rt = runtime();
        rt.block_on(async {
            let held = StdLock.acquire(&lock_path).await.unwrap();
            let _child = procs()
                .spawn_now(
                    &program,
                    &sleeping("60"),
                    &AbsDir::root(),
                    no_lock(),
                    &ChildIo::Null,
                )
                .unwrap();
            drop(held);
            assert_eq!(StdLock.probe(&lock_path).await, Ok(Availability::Free));
        });
    }
}
