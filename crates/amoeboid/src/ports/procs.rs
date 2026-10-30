//! `Procs`: processes we started, identified by [`ProcIdentity`] (design §Ports and adapters). A
//! pid is only trusted together with its start time and executable name, and a signal goes only to
//! a process whose identity is the recorded one.

use std::fmt;
use std::future::Future;

use crate::diagnostic::Diagnostic;
use crate::domain::{AbsDir, AbsPath, Pid, ProcIdentity, ProgramNameError, StatError};
use crate::plan::Argv;
use crate::ports::Cause;
use crate::ports::vm_lock::{HeldLock, LockError};

/// What the call was doing when it failed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ProcOp {
    /// Reading `/proc/PID/stat`.
    ReadStat,
    /// Reading the link `/proc/PID/exe`.
    ReadExe,
    /// Getting a handle on the process (`pidfd_open`).
    Open,
    /// Sending the signal.
    Signal,
    /// Waiting for the process to end.
    Wait,
}

impl ProcOp {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ReadStat => "read the status of",
            Self::ReadExe => "read the executable of",
            Self::Open => "get a handle on",
            Self::Signal => "signal",
            Self::Wait => "wait for",
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum ProcError {
    #[error("can't {op} process {pid}: {cause}", op = .op.as_str())]
    Io { op: ProcOp, pid: Pid, cause: Cause },
    #[error("no process {pid} is running")]
    NoSuchProcess { pid: Pid },
    #[error("can't read the status of process {pid}: {error}")]
    Stat { pid: Pid, error: StatError },
    #[error("can't name the program of process {pid}: {error}")]
    Program { pid: Pid, error: ProgramNameError },
    #[error("can't name the program of process {pid}: its path isn't UTF-8")]
    NotUnicode { pid: Pid },
    /// The blocking task did not finish (the runtime was shutting down).
    #[error("can't inspect process {pid}: the operation was aborted")]
    Aborted { pid: Pid },
}

impl Diagnostic for ProcError {
    fn code(&self) -> &'static str {
        match self {
            Self::Io { .. } => "procs.io",
            Self::NoSuchProcess { .. } => "procs.no-such-process",
            Self::Stat { .. } => "procs.stat",
            Self::Program { .. } => "procs.program",
            Self::NotUnicode { .. } => "procs.not-unicode",
            Self::Aborted { .. } => "procs.aborted",
        }
    }

    fn message(&self) -> String {
        self.to_string()
    }

    fn hint(&self) -> Option<String> {
        Some(
            match self {
                Self::Io { .. } | Self::Stat { .. } | Self::Program { .. } => {
                    "check that /proc is mounted and readable"
                }
                Self::NoSuchProcess { .. } => "start the VM again",
                Self::NotUnicode { .. } => "run the program from a path made of UTF-8 text",
                Self::Aborted { .. } => "run the command again",
            }
            .to_owned(),
        )
    }
}

/// Whether the process a recorded identity names is still the one running under its pid.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Presence {
    /// Pid, start time and program all match: it is ours and running.
    Alive,
    /// No such process, or it has exited and is only waiting to be reaped.
    Gone,
    /// Something runs under the pid, but not our process: the pid was reused, or the process
    /// belongs to someone else (its executable can't be read).
    Replaced,
}

/// The signals amoeboid sends: a polite stop, then the unconditional one.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Signal {
    Terminate,
    Kill,
}

/// What [`Procs::signal`] did.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Signalled {
    /// Delivered to the verified process.
    Sent,
    /// Nothing sent: the process is gone.
    Gone,
    /// Nothing sent: the pid belongs to a different process now.
    Replaced,
}

/// How a child gets the VM lock (design §One lock).
#[derive(Debug)]
pub enum LockHandoff<'a, H> {
    /// As the child's stdin (virtiofsd, which does not read it): [`HeldLock::stdin`].
    Stdin(&'a H),
    /// As a descriptor without `FD_CLOEXEC` ([`HeldLock::inheritable`]), open across the spawn
    /// and closed in the parent right after it (qemu, which needs its stdin).
    Inherited(&'a H),
    /// The child does not hold the lock.
    None,
}

// Only references: copyable whatever `H` is (a derive would demand `H: Copy`).
impl<H> Clone for LockHandoff<'_, H> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<H> Copy for LockHandoff<'_, H> {}

/// Where a child's output goes. Its stdin is the lock (with [`LockHandoff::Stdin`]), else the
/// terminal's for [`ChildIo::Inherit`] and nothing otherwise. `Inherit` also keeps the terminal's
/// process group (the foreground qemu owns the terminal); `Null` and `Log` start a new one, so
/// terminal signals reach neither a helper nor a detached VM.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ChildIo {
    /// Stdin, stdout and stderr are `/dev/null`.
    Null,
    /// The child shares the launcher's terminal.
    Inherit,
    /// Stdout and stderr append to the file, which is created (0600) if missing.
    Log(AbsPath),
}

/// How a child ended.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Exit {
    Code(i32),
    /// Ended by this signal number.
    Signal(i32),
}

impl fmt::Display for Exit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Code(code) => write!(f, "exit status {code}"),
            Self::Signal(signal) => write!(f, "signal {signal}"),
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum SpawnError {
    /// The lock could not be handed to the child.
    #[error("can't hand the lock to the process: {0}")]
    Lock(LockError),
    #[error("can't open the log {path}: {cause}")]
    Log { path: AbsPath, cause: Cause },
    /// The program could not be started (not found, not executable).
    #[error("can't start the process: {cause}")]
    Exec { cause: Cause },
    /// The process started but could not be identified (it ended at once, or `/proc` failed);
    /// it has been told to stop.
    #[error("can't identify the process: {0}")]
    Identify(ProcError),
    /// The process ended before it could be identified; how it ended.
    #[error("the process ended at once ({exit})")]
    Exited { exit: Exit },
}

impl Diagnostic for SpawnError {
    fn code(&self) -> &'static str {
        match self {
            Self::Lock(_) => "spawn.lock",
            Self::Log { .. } => "spawn.log",
            Self::Exec { .. } => "spawn.exec",
            Self::Identify(_) => "spawn.identify",
            Self::Exited { .. } => "spawn.exited",
        }
    }

    fn message(&self) -> String {
        self.to_string()
    }

    fn hint(&self) -> Option<String> {
        Some(
            match self {
                Self::Lock(_) => "stop the VM first, then try again",
                Self::Log { .. } => "check the permissions of the VM's data directory",
                Self::Exec { .. } => "check that the program exists and is executable",
                Self::Identify(_) => "check that /proc is mounted and the program stays running",
                Self::Exited { .. } => "run the program by hand to see why it stops",
            }
            .to_owned(),
        )
    }
}

/// A process we started. Dropping it sends SIGTERM to the verified identity (unless it was waited
/// for), so a spawned child is guarded from the moment [`Procs::spawn`] returns.
pub trait Spawned {
    /// The identity read when the child was spawned, or the last [`settle`](Self::settle).
    fn identity(&self) -> &ProcIdentity;

    /// Reads the identity again. The runner execs qemu, so `/proc/PID/exe` changes after the
    /// spawn: the pid and start time must be the same, the program may be new; the settled
    /// identity replaces the old one.
    fn settle(&mut self) -> impl Future<Output = Result<ProcIdentity, ProcError>> + Send;

    /// Disarms the guard: the process is left running (it keeps the lock it inherited) and
    /// nothing signals it when the handle goes. Returns the identity to record.
    fn release(self) -> ProcIdentity
    where
        Self: Sized;

    /// Waits for the process to end (and reaps it). Safe to cancel.
    fn wait(&mut self) -> impl Future<Output = Result<Exit, ProcError>> + Send;
}

pub trait Procs {
    type Child: Spawned + Send;

    /// Starts `argv` (program looked up on `PATH` unless absolute) and identifies it from `/proc`
    /// before returning. Synchronous on purpose: no `.await` separates the process from its guard,
    /// so cancelling the caller can't leave it running.
    ///
    /// # Errors
    ///
    /// See [`SpawnError`]; a child that was started but not identified is stopped first.
    fn spawn<H: HeldLock>(
        &self,
        argv: &Argv,
        cwd: &AbsDir,
        lock: LockHandoff<'_, H>,
        io: ChildIo,
    ) -> Result<Self::Child, SpawnError>;

    /// The identity of the live process `pid`, read from `/proc` (call it right after spawning).
    fn identify(&self, pid: Pid) -> impl Future<Output = Result<ProcIdentity, ProcError>> + Send;

    fn presence(
        &self,
        who: &ProcIdentity,
    ) -> impl Future<Output = Result<Presence, ProcError>> + Send;

    /// Sends `signal` to `who` only if it is [`Presence::Alive`] under that identity. The check
    /// and the send address one process: a handle on the pid (`pidfd`) is taken first, the
    /// identity verified, and the signal sent through the handle, so a pid reused in between can
    /// never receive it.
    fn signal(
        &self,
        who: &ProcIdentity,
        signal: Signal,
    ) -> impl Future<Output = Result<Signalled, ProcError>> + Send;
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) mod tests {
    use std::io;

    use super::*;
    use crate::testing::{self, Entry};

    fn pid(n: u32) -> Pid {
        Pid::parse(n).unwrap()
    }

    fn witness(e: &ProcError) -> &'static str {
        match e {
            ProcError::Io { .. } => "Io",
            ProcError::NoSuchProcess { .. } => "NoSuchProcess",
            ProcError::Stat { .. } => "Stat",
            ProcError::Program { .. } => "Program",
            ProcError::NotUnicode { .. } => "NotUnicode",
            ProcError::Aborted { .. } => "Aborted",
        }
    }

    pub(crate) fn errors() -> Vec<ProcError> {
        vec![
            ProcError::Io {
                op: ProcOp::ReadExe,
                pid: pid(42),
                cause: Cause {
                    kind: io::ErrorKind::PermissionDenied,
                    errno: Some(13),
                },
            },
            ProcError::NoSuchProcess { pid: pid(42) },
            ProcError::Stat {
                pid: pid(42),
                error: StatError::NoName,
            },
            ProcError::Program {
                pid: pid(42),
                error: ProgramNameError::Empty,
            },
            ProcError::NotUnicode { pid: pid(42) },
            ProcError::Aborted { pid: pid(42) },
        ]
    }

    /// One catalogue entry per variant.
    pub(crate) fn entries() -> Vec<Entry> {
        errors().iter().map(Entry::of).collect()
    }

    #[test]
    fn every_error_variant_has_a_table_case() {
        testing::every_variant(
            &errors(),
            witness,
            &[
                "Io",
                "NoSuchProcess",
                "Stat",
                "Program",
                "NotUnicode",
                "Aborted",
            ],
        );
    }

    #[test]
    fn every_operation_reads_as_a_verb_phrase() {
        testing::table(
            &[
                (ProcOp::ReadStat, "read the status of"),
                (ProcOp::ReadExe, "read the executable of"),
                (ProcOp::Open, "get a handle on"),
                (ProcOp::Signal, "signal"),
                (ProcOp::Wait, "wait for"),
            ],
            |op| op.as_str(),
        );
    }

    pub(crate) fn spawn_errors() -> Vec<SpawnError> {
        let cause = Cause {
            kind: io::ErrorKind::NotFound,
            errno: Some(2),
        };
        vec![
            SpawnError::Lock(LockError::Aborted {
                path: AbsPath::parse("/d/proj/lock".to_owned()).unwrap(),
            }),
            SpawnError::Log {
                path: AbsPath::parse("/d/proj/console.log".to_owned()).unwrap(),
                cause,
            },
            SpawnError::Exec { cause },
            SpawnError::Identify(ProcError::NoSuchProcess { pid: pid(42) }),
            SpawnError::Exited {
                exit: Exit::Code(1),
            },
        ]
    }

    /// One catalogue entry per variant.
    pub(crate) fn spawn_entries() -> Vec<Entry> {
        spawn_errors().iter().map(Entry::of).collect()
    }

    fn spawn_witness(e: &SpawnError) -> &'static str {
        match e {
            SpawnError::Lock(_) => "Lock",
            SpawnError::Log { .. } => "Log",
            SpawnError::Exec { .. } => "Exec",
            SpawnError::Identify(_) => "Identify",
            SpawnError::Exited { .. } => "Exited",
        }
    }

    #[test]
    fn every_spawn_error_variant_has_a_table_case() {
        testing::every_variant(
            &spawn_errors(),
            spawn_witness,
            &["Lock", "Log", "Exec", "Identify", "Exited"],
        );
    }

    #[test]
    fn spawn_messages_say_which_step_failed() {
        let messages: Vec<String> = spawn_errors().iter().map(Diagnostic::message).collect();
        assert_eq!(
            messages,
            [
                "can't hand the lock to the process: can't lock /d/proj/lock: the operation was aborted",
                "can't open the log /d/proj/console.log: entity not found",
                "can't start the process: entity not found",
                "can't identify the process: no process 42 is running",
                "the process ended at once (exit status 1)",
            ]
        );
    }
}
