//! `StdSystem`: [`System`] on `/dev/kvm`, `PATH`, `/proc` and a Python run's pid file.

use std::fs;
use std::future::Future;
use std::path::{Path, PathBuf};

use rustix::fs::{Access, AtFlags, CWD, accessat};
use rustix::io::Errno;

use crate::domain::{
    AbsDir, AbsPath, Executable, FileName, KernelVersion, KvmAccess, Pid, Switch, Tool,
    ToolPresence, UsernsSwitches,
};
use crate::ports::system::{KernelProbe, System, SystemError};
use crate::refined;

const DEV: FileName = refined!(FileName, "dev");
const KVM: FileName = refined!(FileName, "kvm");
const PROC: FileName = refined!(FileName, "proc");
const SYS: FileName = refined!(FileName, "sys");
const USER: FileName = refined!(FileName, "user");
const KERNEL: FileName = refined!(FileName, "kernel");
const MAX_USER_NAMESPACES: FileName = refined!(FileName, "max_user_namespaces");
const UNPRIVILEGED_CLONE: FileName = refined!(FileName, "unprivileged_userns_clone");
const APPARMOR_RESTRICT: FileName = refined!(FileName, "apparmor_restrict_unprivileged_userns");
const OSRELEASE: FileName = refined!(FileName, "osrelease");
const QEMU_PID: FileName = refined!(FileName, "qemu.pid");

/// What the Python launcher's executable-name check looked for in `/proc/PID/exe`.
const QEMU_PROGRAM: &str = "qemu";

/// Observes the real host; the roots are fields so tests can point them at a temp dir.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct StdSystem {
    kvm: AbsPath,
    proc: AbsDir,
    path: Vec<AbsDir>,
}

impl StdSystem {
    /// `/dev/kvm` and `/proc`, with the tools looked up in the absolute directories of `path`
    /// (the value of `$PATH`; empty and relative entries are skipped, as a tool found in the
    /// current directory would be whatever the guest left there).
    #[must_use]
    pub fn new(path: &str) -> Self {
        Self::at(
            AbsDir::root().join_dir(&DEV).join(&KVM),
            AbsDir::root().join_dir(&PROC),
            path.split(':')
                .filter_map(|dir| AbsDir::normalising(dir).ok())
                .collect(),
        )
    }

    /// Observes `kvm` and `proc` instead of the real ones; for tests.
    #[must_use]
    pub const fn at(kvm: AbsPath, proc: AbsDir, path: Vec<AbsDir>) -> Self {
        Self { kvm, proc, path }
    }

    fn sys(&self, dir: &FileName, file: &FileName) -> AbsPath {
        self.proc.join_dir(&SYS).join_dir(dir).join(file)
    }
}

impl System for StdSystem {
    fn kvm(&self) -> impl Future<Output = Result<KvmAccess, SystemError>> + Send {
        let kvm = self.kvm.clone();
        blocking(move || kvm_access(&kvm))
    }

    fn tool(&self, tool: Tool) -> impl Future<Output = Result<ToolPresence, SystemError>> + Send {
        let candidates: Vec<AbsPath> = self
            .path
            .iter()
            .map(|dir| dir.join(&tool.file_name()))
            .collect();
        blocking(move || tool_presence(&candidates))
    }

    fn userns(&self) -> impl Future<Output = Result<UsernsSwitches, SystemError>> + Send {
        let (max, clone, apparmor) = (
            self.sys(&USER, &MAX_USER_NAMESPACES),
            self.sys(&KERNEL, &UNPRIVILEGED_CLONE),
            self.sys(&KERNEL, &APPARMOR_RESTRICT),
        );
        blocking(move || UsernsSwitches {
            max_namespaces: switch(&max),
            unprivileged_clone: switch(&clone),
            apparmor_restricts: switch(&apparmor),
        })
    }

    fn kernel(&self) -> impl Future<Output = Result<KernelProbe, SystemError>> + Send {
        let release = self.sys(&KERNEL, &OSRELEASE);
        blocking(move || KernelVersion::parse(&read(&release).unwrap_or_default()))
    }

    fn legacy_qemu(
        &self,
        runtime: &AbsDir,
    ) -> impl Future<Output = Result<Option<Pid>, SystemError>> + Send {
        let (pid_file, proc) = (runtime.join(&QEMU_PID), self.proc.clone());
        blocking(move || live_qemu(&pid_file, &proc))
    }
}

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T, SystemError> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|_| SystemError::Aborted)
}

fn read(path: &AbsPath) -> Option<String> {
    fs::read_to_string(Path::new(path.get())).ok()
}

/// `faccessat` with the effective ids, as `open` would check.
fn kvm_access(kvm: &AbsPath) -> KvmAccess {
    match accessat(
        CWD,
        Path::new(kvm.get()),
        Access::READ_OK | Access::WRITE_OK,
        AtFlags::EACCESS,
    ) {
        Ok(()) => KvmAccess::Usable,
        Err(Errno::NOENT | Errno::NOTDIR) => KvmAccess::Absent,
        Err(_) => KvmAccess::Denied,
    }
}

fn tool_presence(candidates: &[AbsPath]) -> ToolPresence {
    let found = candidates
        .iter()
        .any(|file| Executable::observe(file, super::fs::observe).is_ok());
    if found {
        ToolPresence::Found
    } else {
        ToolPresence::Missing
    }
}

/// A sysctl file: absent when unreadable, off for `0`, else on.
fn switch(file: &AbsPath) -> Switch {
    match read(file) {
        None => Switch::Absent,
        Some(text) if text.trim() == "0" => Switch::Off,
        Some(_) => Switch::On,
    }
}

/// The pid in `pid_file` when `/proc/PID/exe` is a program named like qemu.
fn live_qemu(pid_file: &AbsPath, proc: &AbsDir) -> Option<Pid> {
    let pid = Pid::parse(read(pid_file)?.trim().parse().ok()?).ok()?;
    let exe: PathBuf = [proc.get(), &pid.get().to_string(), "exe"].iter().collect();
    let target = fs::read_link(exe).ok()?;
    let name = target.file_name()?.to_str()?;
    name.contains(QEMU_PROGRAM).then_some(pid)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use std::os::unix::fs::{PermissionsExt as _, symlink};

    use super::*;
    use crate::testing::{self, TempDir};

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }

    fn abs(path: &Path) -> AbsDir {
        AbsDir::normalising(path.to_str().unwrap()).unwrap()
    }

    fn system(temp: &TempDir, path: Vec<AbsDir>) -> StdSystem {
        StdSystem::at(
            abs(temp.path()).join(&KVM),
            abs(&temp.path().join("proc")),
            path,
        )
    }

    fn touch(path: &Path, mode: u32) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, b"").unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
    }

    #[test]
    fn the_real_roots_are_dev_kvm_and_proc_and_path_keeps_absolute_entries_only() {
        let system = StdSystem::new("/usr/bin::bin:/opt/x/:/a/../b");
        assert_eq!(system.kvm.get(), "/dev/kvm");
        assert_eq!(system.proc.get(), "/proc");
        let dirs: Vec<&str> = system.path.iter().map(|dir| dir.get().as_str()).collect();
        assert_eq!(dirs, ["/usr/bin", "/opt/x"]);
    }

    #[test]
    fn kvm_is_usable_absent_or_denied() {
        let temp = TempDir::new("system-kvm").unwrap();
        let system = system(&temp, vec![]);
        let seen = || runtime().block_on(system.kvm());
        assert_eq!(seen(), Ok(KvmAccess::Absent));
        touch(&temp.path().join("kvm"), 0o600);
        assert_eq!(seen(), Ok(KvmAccess::Usable));
        touch(&temp.path().join("kvm"), 0o000);
        // root passes any access check
        let expected = if rustix::process::geteuid().is_root() {
            KvmAccess::Usable
        } else {
            KvmAccess::Denied
        };
        assert_eq!(seen(), Ok(expected));
    }

    #[test]
    fn a_kvm_below_a_file_is_absent() {
        let temp = TempDir::new("system-kvm-file").unwrap();
        touch(&temp.path().join("file"), 0o600);
        let system = StdSystem::at(
            abs(&temp.path().join("file")).join(&KVM),
            abs(temp.path()),
            vec![],
        );
        assert_eq!(runtime().block_on(system.kvm()), Ok(KvmAccess::Absent));
    }

    #[test]
    fn a_tool_is_found_only_as_an_executable_file_in_a_listed_directory() {
        let temp = TempDir::new("system-tool").unwrap();
        let bin = |name: &str| temp.path().join(name);
        touch(&bin("first/nix"), 0o644);
        touch(&bin("second/nix"), 0o755);
        touch(&bin("second/ssh-keygen"), 0o644);
        fs::create_dir_all(bin("second/ssh-keygen-dir")).unwrap();
        symlink(bin("second/nix"), bin("third-link")).unwrap();
        fs::create_dir_all(bin("dangling")).unwrap();
        symlink(bin("nowhere"), bin("dangling/ssh-keygen")).unwrap();
        let both = system(&temp, vec![abs(&bin("first")), abs(&bin("second"))]);
        let seen = |system: &StdSystem, tool| runtime().block_on(system.tool(tool));
        assert_eq!(seen(&both, Tool::Nix), Ok(ToolPresence::Found));
        // present but not executable, and a dangling link, count as missing
        assert_eq!(seen(&both, Tool::SshKeygen), Ok(ToolPresence::Missing));
        let first_only = system(&temp, vec![abs(&bin("first")), abs(&bin("dangling"))]);
        assert_eq!(seen(&first_only, Tool::Nix), Ok(ToolPresence::Missing));
        assert_eq!(
            seen(&first_only, Tool::SshKeygen),
            Ok(ToolPresence::Missing)
        );
        let none = system(&temp, vec![]);
        assert_eq!(seen(&none, Tool::Nix), Ok(ToolPresence::Missing));
    }

    fn sysctl(temp: &TempDir, dir: &str, name: &str, text: &str) {
        let path = temp.path().join("proc/sys").join(dir).join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    #[test]
    fn user_namespace_switches_are_absent_off_or_on() {
        let temp = TempDir::new("system-userns").unwrap();
        let system = system(&temp, vec![]);
        let seen = || runtime().block_on(system.userns()).unwrap();
        assert_eq!(
            seen(),
            UsernsSwitches {
                max_namespaces: Switch::Absent,
                unprivileged_clone: Switch::Absent,
                apparmor_restricts: Switch::Absent,
            }
        );
        sysctl(&temp, "user", "max_user_namespaces", "0\n");
        sysctl(&temp, "kernel", "unprivileged_userns_clone", "1\n");
        sysctl(
            &temp,
            "kernel",
            "apparmor_restrict_unprivileged_userns",
            " 0 ",
        );
        assert_eq!(
            seen(),
            UsernsSwitches {
                max_namespaces: Switch::Off,
                unprivileged_clone: Switch::On,
                apparmor_restricts: Switch::Off,
            }
        );
        sysctl(&temp, "user", "max_user_namespaces", "63000\n");
        sysctl(&temp, "kernel", "unprivileged_userns_clone", "garbage");
        assert_eq!(seen().max_namespaces, Switch::On);
        assert_eq!(seen().unprivileged_clone, Switch::On);
    }

    #[test]
    fn the_kernel_is_read_from_osrelease_and_unreadable_counts_as_unrecognised() {
        let temp = TempDir::new("system-kernel").unwrap();
        let system = system(&temp, vec![]);
        let seen = || runtime().block_on(system.kernel()).unwrap();
        assert_eq!(seen(), KernelVersion::parse(""));
        sysctl(&temp, "kernel", "osrelease", "6.12.3-arch1-1\n");
        assert_eq!(seen(), KernelVersion::parse("6.12"));
        sysctl(&temp, "kernel", "osrelease", "weird");
        assert_eq!(seen(), KernelVersion::parse("weird"));
    }

    #[test]
    fn the_real_kernel_is_recognised() {
        let system = StdSystem::new("");
        assert!(runtime().block_on(system.kernel()).unwrap().is_ok());
    }

    #[test]
    fn a_python_run_counts_only_while_its_pid_is_a_live_qemu() {
        let temp = TempDir::new("system-legacy").unwrap();
        let run = temp.path().join("run");
        fs::create_dir_all(&run).unwrap();
        let system = system(&temp, vec![]);
        let seen = || runtime().block_on(system.legacy_qemu(&abs(&run))).unwrap();
        let exe = |pid: u32, target: &str| {
            let dir = temp.path().join("proc").join(pid.to_string());
            fs::create_dir_all(&dir).unwrap();
            let _ = fs::remove_file(dir.join("exe"));
            symlink(target, dir.join("exe")).unwrap();
        };
        // no pid file
        assert_eq!(seen(), None);
        exe(41, "/nix/store/x-qemu-9/bin/qemu-system-x86_64");
        exe(42, "/usr/bin/sleep");
        testing::table(
            &[
                ("41\n", Some(41)),
                ("  41  ", Some(41)),
                ("42\n", None),
                ("43\n", None),
                ("0\n", None),
                ("-1\n", None),
                ("4x\n", None),
                ("", None),
                ("4294967296\n", None),
            ],
            |text| {
                fs::write(run.join("qemu.pid"), text).unwrap();
                seen().map(|pid| *pid.get())
            },
        );
    }
}
