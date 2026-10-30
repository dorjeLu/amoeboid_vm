//! The preflight decision (design §Resiliency): facts about the host in, every problem out at
//! once. Pure; `super::observe` gathers the facts.

use std::fmt;

use crate::diagnostic::Diagnostic;
use crate::domain::{
    AbsDir, KernelVersion, KernelVersionError, KvmAccess, Pid, Switch, Tool, ToolPresence,
    UsernsSwitches, VmName, kernel_version,
};
use crate::rails::{NonEmpty, Validated};

/// What the start found of the VM's runtime dir.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum RuntimeFact {
    Absent,
    /// A directory is there; `locked` is whether the VM lock is held.
    Present {
        locked: bool,
    },
    /// Something else is at the runtime dir's path.
    NotADirectory {
        path: AbsDir,
    },
}

/// A qemu left running by the Python launcher under its own runtime layout.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct LegacyRun {
    pub vm: VmName,
    pub runtime: AbsDir,
    pub pid: Pid,
}

/// Everything the preflight looks at.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Facts {
    pub kvm: KvmAccess,
    pub nix: ToolPresence,
    pub ssh_keygen: ToolPresence,
    pub userns: UsernsSwitches,
    pub kernel: Result<KernelVersion, KernelVersionError>,
    pub runtime: RuntimeFact,
    pub legacy: Option<LegacyRun>,
}

/// Why user namespaces are unavailable (the first switch that forbids them).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum UsernsBlock {
    /// `max_user_namespaces` is 0.
    NoneAllowed,
    /// `unprivileged_userns_clone` is 0.
    Disabled,
    /// `apparmor_restrict_unprivileged_userns` is 1.
    AppArmor,
}

impl fmt::Display for UsernsBlock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::NoneAllowed => "user/max_user_namespaces is 0",
            Self::Disabled => "kernel/unprivileged_userns_clone is 0",
            Self::AppArmor => "kernel/apparmor_restrict_unprivileged_userns is 1",
        })
    }
}

#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum PreflightError {
    #[error("/dev/kvm doesn't exist")]
    KvmAbsent,
    #[error("/dev/kvm isn't accessible to this user")]
    KvmDenied,
    #[error("{tool} not found on PATH")]
    ToolMissing { tool: Tool },
    #[error("unprivileged user namespaces are off: {reason}")]
    UserNamespaces { reason: UsernsBlock },
    #[error("the kernel is {found}, but amoeboid needs {needed} or newer")]
    KernelTooOld {
        found: KernelVersion,
        needed: KernelVersion,
    },
    #[error("can't tell the kernel version: {0}")]
    KernelUnrecognised(KernelVersionError),
    #[error("{path}: not a directory")]
    RuntimeDirNotADirectory { path: AbsDir },
    #[error("VM '{vm}' is still running under the Python launcher (qemu pid {pid} in {runtime})")]
    LegacyRun {
        vm: VmName,
        runtime: AbsDir,
        pid: Pid,
    },
}

impl Diagnostic for PreflightError {
    fn code(&self) -> &'static str {
        match self {
            Self::KvmAbsent => "preflight.kvm-absent",
            Self::KvmDenied => "preflight.kvm-denied",
            Self::ToolMissing { .. } => "preflight.tool-missing",
            Self::UserNamespaces { .. } => "preflight.user-namespaces",
            Self::KernelTooOld { .. } => "preflight.kernel-too-old",
            Self::KernelUnrecognised(_) => "preflight.kernel-unrecognised",
            Self::RuntimeDirNotADirectory { .. } => "preflight.runtime-dir-not-a-directory",
            Self::LegacyRun { .. } => "preflight.legacy-run",
        }
    }

    fn message(&self) -> String {
        self.to_string()
    }

    fn hint(&self) -> Option<String> {
        Some(
            match self {
                Self::KvmAbsent => {
                    "load the kvm module (kvm_intel or kvm_amd) and enable virtualisation in the firmware"
                }
                Self::KvmDenied => "join the kvm group and log in again",
                Self::ToolMissing {
                    tool: Tool::Nix,
                } => "install Nix, or add it to PATH",
                Self::ToolMissing {
                    tool: Tool::SshKeygen,
                } => "install OpenSSH, or add it to PATH",
                Self::UserNamespaces { .. } => {
                    "allow unprivileged user namespaces; virtiofsd's sandbox needs one"
                }
                Self::KernelTooOld { .. } => "boot a newer kernel",
                Self::KernelUnrecognised(_) => "check that /proc is mounted",
                Self::RuntimeDirNotADirectory { .. } => {
                    "remove it; amoeboid keeps the VM's sockets there"
                }
                Self::LegacyRun { .. } => {
                    "stop it with the Python launcher, then start it with this one"
                }
            }
            .to_owned(),
        )
    }
}

/// The runtime dir as the start finds it. `Stale` is a directory no process holds: the new
/// holder clears it. `Locked` is a VM that is running or dangling; the lock decides which.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RuntimeState {
    Absent,
    Stale,
    Locked,
}

/// The host passed every check.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Preflight {
    runtime: RuntimeState,
}

impl Preflight {
    #[must_use]
    pub const fn runtime(&self) -> RuntimeState {
        self.runtime
    }
}

fn kvm(access: KvmAccess) -> Result<(), PreflightError> {
    match access {
        KvmAccess::Usable => Ok(()),
        KvmAccess::Absent => Err(PreflightError::KvmAbsent),
        KvmAccess::Denied => Err(PreflightError::KvmDenied),
    }
}

fn tool(tool: Tool, presence: ToolPresence) -> Result<(), PreflightError> {
    match presence {
        ToolPresence::Found => Ok(()),
        ToolPresence::Missing => Err(PreflightError::ToolMissing { tool }),
    }
}

/// The first switch that forbids unprivileged user namespaces is the reason.
fn userns(switches: UsernsSwitches) -> Result<(), PreflightError> {
    let reason = if switches.max_namespaces == Switch::Off {
        UsernsBlock::NoneAllowed
    } else if switches.unprivileged_clone == Switch::Off {
        UsernsBlock::Disabled
    } else if switches.apparmor_restricts == Switch::On {
        UsernsBlock::AppArmor
    } else {
        return Ok(());
    };
    Err(PreflightError::UserNamespaces { reason })
}

fn kernel(probe: &Result<KernelVersion, KernelVersionError>) -> Result<(), PreflightError> {
    match probe {
        Ok(found) if *found >= kernel_version::OPENAT2 => Ok(()),
        Ok(found) => Err(PreflightError::KernelTooOld {
            found: *found,
            needed: kernel_version::OPENAT2,
        }),
        Err(error) => Err(PreflightError::KernelUnrecognised(error.clone())),
    }
}

fn runtime(fact: &RuntimeFact) -> Result<RuntimeState, PreflightError> {
    match fact {
        RuntimeFact::Absent => Ok(RuntimeState::Absent),
        RuntimeFact::Present { locked: false } => Ok(RuntimeState::Stale),
        RuntimeFact::Present { locked: true } => Ok(RuntimeState::Locked),
        RuntimeFact::NotADirectory { path } => {
            Err(PreflightError::RuntimeDirNotADirectory { path: path.clone() })
        }
    }
}

fn legacy(run: Option<&LegacyRun>) -> Result<(), PreflightError> {
    match run {
        None => Ok(()),
        Some(run) => Err(PreflightError::LegacyRun {
            vm: run.vm.clone(),
            runtime: run.runtime.clone(),
            pid: run.pid,
        }),
    }
}

/// Every problem in `facts`, in the order of the design's list.
///
/// # Errors
/// All failed checks together.
pub fn decide(facts: &Facts) -> Result<Preflight, NonEmpty<PreflightError>> {
    let host = Validated::all(
        [
            kvm(facts.kvm),
            tool(Tool::Nix, facts.nix),
            tool(Tool::SshKeygen, facts.ssh_keygen),
            userns(facts.userns),
            kernel(&facts.kernel),
        ]
        .map(Validated::from_result),
    );
    host.zip(Validated::from_result(runtime(&facts.runtime)))
        .zip(Validated::from_result(legacy(facts.legacy.as_ref())))
        .map(|((_, runtime), ())| Preflight { runtime })
        .into_result()
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) mod tests {
    use super::*;
    use crate::testing::{self, Entry};

    fn dir(text: &str) -> AbsDir {
        AbsDir::parse(text.to_owned()).unwrap()
    }

    fn vm() -> VmName {
        VmName::parse("proj".to_owned()).unwrap()
    }

    fn pid(n: u32) -> Pid {
        Pid::parse(n).unwrap()
    }

    fn kernel(text: &str) -> KernelVersion {
        KernelVersion::parse(text).unwrap()
    }

    fn legacy() -> LegacyRun {
        LegacyRun {
            vm: vm(),
            runtime: dir("/run/user/1000/amoeboid/proj"),
            pid: pid(4242),
        }
    }

    /// Facts with nothing wrong.
    fn healthy() -> Facts {
        Facts {
            kvm: KvmAccess::Usable,
            nix: ToolPresence::Found,
            ssh_keygen: ToolPresence::Found,
            userns: UsernsSwitches {
                max_namespaces: Switch::On,
                unprivileged_clone: Switch::Absent,
                apparmor_restricts: Switch::Absent,
            },
            kernel: Ok(kernel("6.12.3")),
            runtime: RuntimeFact::Absent,
            legacy: None,
        }
    }

    fn witness(e: &PreflightError) -> &'static str {
        match e {
            PreflightError::KvmAbsent => "KvmAbsent",
            PreflightError::KvmDenied => "KvmDenied",
            PreflightError::ToolMissing { .. } => "ToolMissing",
            PreflightError::UserNamespaces { .. } => "UserNamespaces",
            PreflightError::KernelTooOld { .. } => "KernelTooOld",
            PreflightError::KernelUnrecognised(_) => "KernelUnrecognised",
            PreflightError::RuntimeDirNotADirectory { .. } => "RuntimeDirNotADirectory",
            PreflightError::LegacyRun { .. } => "LegacyRun",
        }
    }

    fn errors() -> Vec<PreflightError> {
        vec![
            PreflightError::KvmAbsent,
            PreflightError::KvmDenied,
            PreflightError::ToolMissing { tool: Tool::Nix },
            PreflightError::ToolMissing {
                tool: Tool::SshKeygen,
            },
            PreflightError::UserNamespaces {
                reason: UsernsBlock::NoneAllowed,
            },
            PreflightError::UserNamespaces {
                reason: UsernsBlock::Disabled,
            },
            PreflightError::UserNamespaces {
                reason: UsernsBlock::AppArmor,
            },
            PreflightError::KernelTooOld {
                found: kernel("5.4"),
                needed: kernel_version::OPENAT2,
            },
            PreflightError::KernelUnrecognised(KernelVersionError::NoMajor),
            PreflightError::RuntimeDirNotADirectory {
                path: dir("/run/user/1000/amb_proj"),
            },
            PreflightError::LegacyRun {
                vm: vm(),
                runtime: dir("/run/user/1000/amoeboid/proj"),
                pid: pid(4242),
            },
        ]
    }

    /// One catalogue entry per message and hint shape.
    pub(crate) fn entries() -> Vec<Entry> {
        let all = errors();
        // The catalogue wants one entry per code; the first of each code stands for it.
        let mut seen = Vec::new();
        all.iter()
            .filter(|e| {
                let fresh = !seen.contains(&e.code());
                seen.push(e.code());
                fresh
            })
            .map(Entry::of)
            .collect()
    }

    #[test]
    fn every_error_variant_has_a_table_case() {
        testing::every_variant(
            &errors(),
            witness,
            &[
                "KvmAbsent",
                "KvmDenied",
                "ToolMissing",
                "UserNamespaces",
                "KernelTooOld",
                "KernelUnrecognised",
                "RuntimeDirNotADirectory",
                "LegacyRun",
            ],
        );
    }

    #[test]
    fn messages_and_hints_say_what_to_fix() {
        let shown: Vec<(String, String)> = errors()
            .iter()
            .map(|e| (e.message(), e.hint().unwrap()))
            .collect();
        let pairs = |items: &[(&str, &str)]| -> Vec<(String, String)> {
            items
                .iter()
                .map(|(m, h)| ((*m).to_owned(), (*h).to_owned()))
                .collect()
        };
        assert_eq!(
            shown,
            pairs(&[
                (
                    "/dev/kvm doesn't exist",
                    "load the kvm module (kvm_intel or kvm_amd) and enable virtualisation in the firmware",
                ),
                (
                    "/dev/kvm isn't accessible to this user",
                    "join the kvm group and log in again"
                ),
                ("nix not found on PATH", "install Nix, or add it to PATH"),
                (
                    "ssh-keygen not found on PATH",
                    "install OpenSSH, or add it to PATH"
                ),
                (
                    "unprivileged user namespaces are off: user/max_user_namespaces is 0",
                    "allow unprivileged user namespaces; virtiofsd's sandbox needs one",
                ),
                (
                    "unprivileged user namespaces are off: kernel/unprivileged_userns_clone is 0",
                    "allow unprivileged user namespaces; virtiofsd's sandbox needs one",
                ),
                (
                    "unprivileged user namespaces are off: kernel/apparmor_restrict_unprivileged_userns is 1",
                    "allow unprivileged user namespaces; virtiofsd's sandbox needs one",
                ),
                (
                    "the kernel is 5.4, but amoeboid needs 5.6 or newer",
                    "boot a newer kernel",
                ),
                (
                    "can't tell the kernel version: bad kernel release: it doesn't start with a number",
                    "check that /proc is mounted",
                ),
                (
                    "/run/user/1000/amb_proj: not a directory",
                    "remove it; amoeboid keeps the VM's sockets there",
                ),
                (
                    "VM 'proj' is still running under the Python launcher (qemu pid 4242 in /run/user/1000/amoeboid/proj)",
                    "stop it with the Python launcher, then start it with this one",
                ),
            ])
        );
    }

    fn problems(facts: &Facts) -> Vec<PreflightError> {
        decide(facts)
            .err()
            .map(NonEmpty::into_vec)
            .unwrap_or_default()
    }

    #[test]
    fn a_healthy_host_passes_and_reports_the_runtime_dir() {
        testing::table(
            &[
                (RuntimeFact::Absent, Some(RuntimeState::Absent)),
                (
                    RuntimeFact::Present { locked: false },
                    Some(RuntimeState::Stale),
                ),
                (
                    RuntimeFact::Present { locked: true },
                    Some(RuntimeState::Locked),
                ),
            ],
            |runtime| {
                let facts = Facts {
                    runtime: runtime.clone(),
                    ..healthy()
                };
                decide(&facts).ok().map(|preflight| preflight.runtime())
            },
        );
    }

    fn switches(max: Switch, clone: Switch, apparmor: Switch) -> UsernsSwitches {
        UsernsSwitches {
            max_namespaces: max,
            unprivileged_clone: clone,
            apparmor_restricts: apparmor,
        }
    }

    #[test]
    fn each_check_failing_alone_reports_exactly_its_problem() {
        let with = |edit: fn(&mut Facts)| {
            let mut facts = healthy();
            edit(&mut facts);
            facts
        };
        let userns = |max, clone, apparmor| {
            let mut facts = healthy();
            facts.userns = switches(max, clone, apparmor);
            facts
        };
        let blocked = |reason| vec![PreflightError::UserNamespaces { reason }];
        let too_old = |found: &str| {
            vec![PreflightError::KernelTooOld {
                found: kernel(found),
                needed: kernel_version::OPENAT2,
            }]
        };
        testing::table(
            &[
                (
                    with(|f| f.kvm = KvmAccess::Absent),
                    vec![PreflightError::KvmAbsent],
                ),
                (
                    with(|f| f.kvm = KvmAccess::Denied),
                    vec![PreflightError::KvmDenied],
                ),
                (
                    with(|f| f.nix = ToolPresence::Missing),
                    vec![PreflightError::ToolMissing { tool: Tool::Nix }],
                ),
                (
                    with(|f| f.ssh_keygen = ToolPresence::Missing),
                    vec![PreflightError::ToolMissing {
                        tool: Tool::SshKeygen,
                    }],
                ),
                (
                    userns(Switch::Off, Switch::Absent, Switch::Absent),
                    blocked(UsernsBlock::NoneAllowed),
                ),
                (
                    userns(Switch::On, Switch::Off, Switch::Absent),
                    blocked(UsernsBlock::Disabled),
                ),
                (
                    userns(Switch::On, Switch::On, Switch::On),
                    blocked(UsernsBlock::AppArmor),
                ),
                // the first forbidding switch is the reason
                (
                    userns(Switch::Off, Switch::Off, Switch::On),
                    blocked(UsernsBlock::NoneAllowed),
                ),
                (
                    userns(Switch::On, Switch::Off, Switch::On),
                    blocked(UsernsBlock::Disabled),
                ),
                // switches that allow, or that this kernel doesn't have
                (
                    userns(Switch::Absent, Switch::Absent, Switch::Absent),
                    vec![],
                ),
                (userns(Switch::On, Switch::On, Switch::Off), vec![]),
                (userns(Switch::On, Switch::On, Switch::Absent), vec![]),
                (with(|f| f.kernel = Ok(kernel("5.5.19"))), too_old("5.5")),
                (with(|f| f.kernel = Ok(kernel("4.20"))), too_old("4.20")),
                (with(|f| f.kernel = Ok(kernel("5.6"))), vec![]),
                (with(|f| f.kernel = Ok(kernel("5.10.0"))), vec![]),
                (with(|f| f.kernel = Ok(kernel("6.0"))), vec![]),
                (
                    with(|f| f.kernel = Err(KernelVersionError::NoMinor)),
                    vec![PreflightError::KernelUnrecognised(
                        KernelVersionError::NoMinor,
                    )],
                ),
                (
                    with(|f| {
                        f.runtime = RuntimeFact::NotADirectory {
                            path: dir("/run/user/1000/amb_proj"),
                        };
                    }),
                    vec![PreflightError::RuntimeDirNotADirectory {
                        path: dir("/run/user/1000/amb_proj"),
                    }],
                ),
                (
                    with(|f| f.runtime = RuntimeFact::Present { locked: true }),
                    vec![],
                ),
                (
                    with(|f| f.legacy = Some(legacy())),
                    vec![PreflightError::LegacyRun {
                        vm: vm(),
                        runtime: dir("/run/user/1000/amoeboid/proj"),
                        pid: pid(4242),
                    }],
                ),
                (healthy(), vec![]),
            ],
            problems,
        );
    }

    #[test]
    fn every_failing_check_is_reported_together_in_the_designs_order() {
        let facts = Facts {
            kvm: KvmAccess::Denied,
            nix: ToolPresence::Missing,
            ssh_keygen: ToolPresence::Missing,
            userns: switches(Switch::On, Switch::On, Switch::On),
            kernel: Ok(kernel("5.4")),
            runtime: RuntimeFact::NotADirectory {
                path: dir("/run/user/1000/amb_proj"),
            },
            legacy: Some(legacy()),
        };
        let codes: Vec<&str> = problems(&facts).iter().map(Diagnostic::code).collect();
        assert_eq!(
            codes,
            [
                "preflight.kvm-denied",
                "preflight.tool-missing",
                "preflight.tool-missing",
                "preflight.user-namespaces",
                "preflight.kernel-too-old",
                "preflight.runtime-dir-not-a-directory",
                "preflight.legacy-run",
            ]
        );
        assert_eq!(
            decide(&facts)
                .map(|p| p.runtime())
                .err()
                .map(|e| e.len().get()),
            Some(7)
        );
    }
}
