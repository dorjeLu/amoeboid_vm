//! The preflight (design §Resiliency): observe the host once, then decide on all of it. The
//! decision is [`decide`]; this module only gathers the facts.

mod decide;

pub use decide::{
    Facts, LegacyRun, Preflight, PreflightError, RuntimeFact, RuntimeState, UsernsBlock, decide,
};

#[cfg(test)]
pub(crate) use decide::tests as decide_tests;

use crate::diagnostic::Diagnostic;
use crate::domain::{Observation, Tool};
use crate::layout::{Paths, VmPaths};
use crate::ports::host_fs::{FsError, HostFs};
use crate::ports::system::{System, SystemError};
use crate::ports::vm_lock::{Availability, LockError, VmLock};
use crate::rails::NonEmpty;

/// Why the preflight did not pass: the host has problems, or it could not be looked at.
#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum PreflightFailure {
    /// Every check that failed, in the design's order.
    #[error("{}", join(.0))]
    Problems(NonEmpty<PreflightError>),
    #[error(transparent)]
    Inspect(SystemError),
    #[error(transparent)]
    Probe(FsError),
    #[error(transparent)]
    Lock(LockError),
}

fn join(problems: &NonEmpty<PreflightError>) -> String {
    let messages: Vec<String> = problems.iter().map(Diagnostic::message).collect();
    messages.join("; ")
}

impl Diagnostic for PreflightFailure {
    fn code(&self) -> &'static str {
        match self {
            Self::Problems(_) => "preflight.problems",
            Self::Inspect(_) => "preflight.inspect",
            Self::Probe(_) => "preflight.probe",
            Self::Lock(_) => "preflight.lock",
        }
    }

    fn message(&self) -> String {
        self.to_string()
    }

    fn hint(&self) -> Option<String> {
        match self {
            Self::Problems(problems) => problems.first().hint(),
            Self::Inspect(error) => error.hint(),
            Self::Probe(error) => error.hint(),
            Self::Lock(error) => error.hint(),
        }
    }
}

/// Looks at the host and at VM `vm`'s runtime dir, then decides.
///
/// # Errors
/// [`PreflightFailure::Problems`] with every failed check, or the failure to observe one.
pub async fn observe(
    system: &impl System,
    fs: &impl HostFs,
    lock: &impl VmLock,
    paths: &Paths,
    vm: &VmPaths,
) -> Result<Preflight, PreflightFailure> {
    let facts = Facts {
        kvm: system.kvm().await.map_err(PreflightFailure::Inspect)?,
        nix: system
            .tool(Tool::Nix)
            .await
            .map_err(PreflightFailure::Inspect)?,
        ssh_keygen: system
            .tool(Tool::SshKeygen)
            .await
            .map_err(PreflightFailure::Inspect)?,
        userns: system.userns().await.map_err(PreflightFailure::Inspect)?,
        kernel: system.kernel().await.map_err(PreflightFailure::Inspect)?,
        runtime: runtime_fact(fs, lock, vm).await?,
        legacy: legacy_run(system, paths, vm).await?,
    };
    decide(&facts).map_err(PreflightFailure::Problems)
}

async fn runtime_fact(
    fs: &impl HostFs,
    lock: &impl VmLock,
    vm: &VmPaths,
) -> Result<RuntimeFact, PreflightFailure> {
    let dir = vm.runtime().path();
    match fs
        .observe(&dir.to_path())
        .await
        .map_err(PreflightFailure::Probe)?
    {
        Observation::Missing => Ok(RuntimeFact::Absent),
        Observation::Directory { .. } => {
            let availability = lock
                .probe(&vm.lock())
                .await
                .map_err(PreflightFailure::Lock)?;
            Ok(RuntimeFact::Present {
                locked: availability == Availability::Held,
            })
        }
        Observation::File { .. } | Observation::Other => {
            Ok(RuntimeFact::NotADirectory { path: dir.clone() })
        }
    }
}

async fn legacy_run(
    system: &impl System,
    paths: &Paths,
    vm: &VmPaths,
) -> Result<Option<LegacyRun>, PreflightFailure> {
    let runtime = paths.legacy_runtime(vm.name());
    let pid = system
        .legacy_qemu(&runtime)
        .await
        .map_err(PreflightFailure::Inspect)?;
    Ok(pid.map(|pid| LegacyRun {
        vm: vm.name().clone(),
        runtime,
        pid,
    }))
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) mod tests {
    use std::fs;
    use std::future::{Future, ready};
    use std::io;

    use super::*;
    use crate::adapters::{StdFs, StdLock};
    use crate::domain::{
        AbsDir, AbsPath, KernelVersion, KernelVersionError, KvmAccess, Pid, Switch, ToolPresence,
        Uid, UsernsSwitches, VmName,
    };
    use crate::layout::{Env, Var};
    use crate::ports::Cause;
    use crate::ports::host_fs::FsOp;
    use crate::ports::system::KernelProbe;
    use crate::ports::vm_lock::LockOp;
    use crate::testing::{self, Entry, TempDir};

    /// A host that answers with fixed facts.
    struct Host {
        kvm: KvmAccess,
        tools: ToolPresence,
        userns: UsernsSwitches,
        kernel: KernelProbe,
        legacy: Option<Pid>,
        broken: bool,
    }

    impl Host {
        fn healthy() -> Self {
            Self {
                kvm: KvmAccess::Usable,
                tools: ToolPresence::Found,
                userns: UsernsSwitches {
                    max_namespaces: Switch::On,
                    unprivileged_clone: Switch::Absent,
                    apparmor_restricts: Switch::Absent,
                },
                kernel: KernelVersion::parse("6.12.3"),
                legacy: None,
                broken: false,
            }
        }

        fn answer<T>(&self, value: T) -> impl Future<Output = Result<T, SystemError>> + Send
        where
            T: Send,
        {
            ready(if self.broken {
                Err(SystemError::Aborted)
            } else {
                Ok(value)
            })
        }
    }

    impl System for Host {
        fn kvm(&self) -> impl Future<Output = Result<KvmAccess, SystemError>> + Send {
            self.answer(self.kvm)
        }

        fn tool(
            &self,
            _tool: Tool,
        ) -> impl Future<Output = Result<ToolPresence, SystemError>> + Send {
            self.answer(self.tools)
        }

        fn userns(&self) -> impl Future<Output = Result<UsernsSwitches, SystemError>> + Send {
            self.answer(self.userns)
        }

        fn kernel(&self) -> impl Future<Output = Result<KernelProbe, SystemError>> + Send {
            self.answer(self.kernel.clone())
        }

        fn legacy_qemu(
            &self,
            _runtime: &AbsDir,
        ) -> impl Future<Output = Result<Option<Pid>, SystemError>> + Send {
            self.answer(self.legacy)
        }
    }

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }

    fn setup(label: &str) -> (TempDir, Paths, VmPaths) {
        let temp = TempDir::new(label).unwrap();
        let root = temp.path().to_str().unwrap().to_owned();
        let paths = Paths::resolve(
            &Env {
                home: Var::Text(format!("{root}/home")),
                xdg_data_home: Var::Text(format!("{root}/data")),
                xdg_runtime_dir: Var::Text(format!("{root}/r")),
                ..Env::UNSET
            },
            Uid::parse(1000).unwrap(),
        )
        .unwrap();
        let vm = paths.vm(&VmName::parse("proj".to_owned()).unwrap());
        fs::create_dir_all(vm.state().get()).unwrap();
        (temp, paths, vm)
    }

    fn run(host: &Host, paths: &Paths, vm: &VmPaths) -> Result<Preflight, PreflightFailure> {
        runtime().block_on(observe(host, &StdFs, &StdLock, paths, vm))
    }

    fn state(result: Result<Preflight, PreflightFailure>) -> Option<RuntimeState> {
        result.ok().map(|preflight| preflight.runtime())
    }

    #[test]
    fn a_healthy_host_passes_and_the_runtime_dir_is_classified() {
        let (_temp, paths, vm) = setup("pf-rt");
        let host = Host::healthy();
        assert_eq!(state(run(&host, &paths, &vm)), Some(RuntimeState::Absent));
        fs::create_dir_all(vm.runtime().path().get()).unwrap();
        assert_eq!(state(run(&host, &paths, &vm)), Some(RuntimeState::Stale));
        let held = runtime().block_on(StdLock.acquire(&vm.lock())).unwrap();
        assert_eq!(state(run(&host, &paths, &vm)), Some(RuntimeState::Locked));
        drop(held);
        assert_eq!(state(run(&host, &paths, &vm)), Some(RuntimeState::Stale));
    }

    #[test]
    fn a_file_at_the_runtime_dir_is_a_problem_not_a_directory() {
        let (_temp, paths, vm) = setup("pf-file");
        fs::create_dir_all(vm.runtime().path().get().rsplit_once('/').unwrap().0).unwrap();
        fs::write(vm.runtime().path().get(), b"").unwrap();
        assert_eq!(
            run(&Host::healthy(), &paths, &vm),
            Err(PreflightFailure::Problems(NonEmpty::singleton(
                PreflightError::RuntimeDirNotADirectory {
                    path: vm.runtime().path().clone()
                }
            )))
        );
    }

    #[test]
    fn a_python_run_is_reported_with_its_runtime_dir_and_pid() {
        let (_temp, paths, vm) = setup("pf-py");
        let host = Host {
            legacy: Some(Pid::parse(77).unwrap()),
            ..Host::healthy()
        };
        assert_eq!(
            run(&host, &paths, &vm),
            Err(PreflightFailure::Problems(NonEmpty::singleton(
                PreflightError::LegacyRun {
                    vm: vm.name().clone(),
                    runtime: paths.legacy_runtime(vm.name()),
                    pid: Pid::parse(77).unwrap(),
                }
            )))
        );
    }

    #[test]
    fn all_failing_checks_of_a_bad_host_come_back_together() {
        let (_temp, paths, vm) = setup("pf-all");
        let host = Host {
            kvm: KvmAccess::Absent,
            tools: ToolPresence::Missing,
            userns: UsernsSwitches {
                max_namespaces: Switch::Off,
                unprivileged_clone: Switch::On,
                apparmor_restricts: Switch::On,
            },
            kernel: Err(KernelVersionError::NoMajor),
            legacy: Some(Pid::parse(77).unwrap()),
            broken: false,
        };
        let Err(PreflightFailure::Problems(problems)) = run(&host, &paths, &vm) else {
            return;
        };
        let codes: Vec<&str> = problems.iter().map(Diagnostic::code).collect();
        assert_eq!(
            codes,
            [
                "preflight.kvm-absent",
                "preflight.tool-missing",
                "preflight.tool-missing",
                "preflight.user-namespaces",
                "preflight.kernel-unrecognised",
                "preflight.legacy-run",
            ]
        );
    }

    #[test]
    fn a_host_that_cannot_be_inspected_is_a_failure_not_a_problem() {
        let (_temp, paths, vm) = setup("pf-brk");
        let host = Host {
            broken: true,
            ..Host::healthy()
        };
        assert_eq!(
            run(&host, &paths, &vm),
            Err(PreflightFailure::Inspect(SystemError::Aborted))
        );
    }

    #[test]
    fn a_runtime_dir_that_cannot_be_looked_at_is_a_probe_failure() {
        struct Blind;
        impl HostFs for Blind {
            fn write_file(
                &self,
                _: &AbsPath,
                _: Vec<u8>,
                _: crate::domain::FileMode,
            ) -> impl Future<Output = Result<(), FsError>> + Send {
                ready(Ok(()))
            }
            fn observe(
                &self,
                path: &AbsPath,
            ) -> impl Future<Output = Result<Observation, FsError>> + Send {
                ready(Err(FsError::Aborted {
                    op: FsOp::Stat,
                    path: path.clone(),
                }))
            }
            fn read_file(
                &self,
                _: &AbsPath,
            ) -> impl Future<Output = Result<Vec<u8>, FsError>> + Send {
                ready(Ok(vec![]))
            }
            fn file_mode(
                &self,
                path: &AbsPath,
            ) -> impl Future<Output = Result<crate::domain::FileMode, FsError>> + Send {
                ready(Err(FsError::Aborted {
                    op: FsOp::Stat,
                    path: path.clone(),
                }))
            }
            fn read_tree(
                &self,
                _: &AbsDir,
            ) -> impl Future<Output = Result<Vec<crate::domain::TreeFile>, FsError>> + Send
            {
                ready(Ok(vec![]))
            }
            fn remove_file(&self, _: &AbsPath) -> impl Future<Output = Result<(), FsError>> + Send {
                ready(Ok(()))
            }
            fn clear_dir(&self, _: &AbsDir) -> impl Future<Output = Result<(), FsError>> + Send {
                ready(Ok(()))
            }
            fn create_dir_all(
                &self,
                _: &AbsDir,
                _: crate::domain::FileMode,
            ) -> impl Future<Output = Result<(), FsError>> + Send {
                ready(Ok(()))
            }
            fn replace_dir(
                &self,
                _: &AbsDir,
                _: &AbsDir,
            ) -> impl Future<Output = Result<crate::ports::host_fs::Replacement, FsError>> + Send
            {
                ready(Ok(crate::ports::host_fs::Replacement::Atomic))
            }
        }
        let (_temp, paths, vm) = setup("pf-blind");
        let seen = runtime().block_on(observe(&Host::healthy(), &Blind, &StdLock, &paths, &vm));
        assert!(matches!(seen, Err(PreflightFailure::Probe(_))), "{seen:?}");
    }

    fn lock_error() -> LockError {
        LockError::Io {
            op: LockOp::Open,
            path: AbsPath::parse("/d/proj/lock".to_owned()).unwrap(),
            cause: Cause {
                kind: io::ErrorKind::PermissionDenied,
                errno: Some(13),
            },
        }
    }

    fn fs_error() -> FsError {
        FsError::Aborted {
            op: FsOp::Stat,
            path: AbsPath::parse("/run/amb_proj".to_owned()).unwrap(),
        }
    }

    fn failures() -> Vec<PreflightFailure> {
        vec![
            PreflightFailure::Problems(NonEmpty::new(
                PreflightError::KvmAbsent,
                vec![PreflightError::KvmDenied],
            )),
            PreflightFailure::Inspect(SystemError::Aborted),
            PreflightFailure::Probe(fs_error()),
            PreflightFailure::Lock(lock_error()),
        ]
    }

    fn witness(e: &PreflightFailure) -> &'static str {
        match e {
            PreflightFailure::Problems(_) => "Problems",
            PreflightFailure::Inspect(_) => "Inspect",
            PreflightFailure::Probe(_) => "Probe",
            PreflightFailure::Lock(_) => "Lock",
        }
    }

    /// One catalogue entry per variant.
    pub(crate) fn entries() -> Vec<Entry> {
        failures().iter().map(Entry::of).collect()
    }

    #[test]
    fn every_failure_variant_has_a_table_case() {
        testing::every_variant(
            &failures(),
            witness,
            &["Problems", "Inspect", "Probe", "Lock"],
        );
    }

    #[test]
    fn problems_are_joined_in_the_message_and_hinted_by_the_first() {
        let [problems, inspect, probe, lock] =
            <[PreflightFailure; 4]>::try_from(failures()).unwrap();
        assert_eq!(
            problems.message(),
            "/dev/kvm doesn't exist; /dev/kvm isn't accessible to this user"
        );
        assert_eq!(problems.hint(), PreflightError::KvmAbsent.hint());
        assert_eq!(inspect.hint(), SystemError::Aborted.hint());
        assert_eq!(probe.hint(), fs_error().hint());
        assert_eq!(lock.hint(), lock_error().hint());
    }
}
