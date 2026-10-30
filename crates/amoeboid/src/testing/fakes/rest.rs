//! Fakes of `Nix`, `System`, `Ssh`, `Qmp` and `Session`: configurable answers, recorded effects.

#![allow(
    clippy::disallowed_types,
    reason = "boundary: fixtures name raw values"
)]

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex};

use super::{Fault, OpId, Script, held, step};
use crate::domain::{
    AbsDir, AbsPath, FlakeDir, FlakePath, HostPublicKey, KernelVersion, KvmAccess, Linger,
    LoopbackPort, Pid, SshBanner, Switch, Tool, ToolPresence, UserName, UsernsSwitches,
};
use crate::layout::{SocketPath, VmPaths};
use crate::ports::nix::{BuildStatus, Nix, NixError};
use crate::ports::qmp::{Qmp, QmpError};
use crate::ports::session::{Session, SessionError};
use crate::ports::ssh::{BannerError, FreePortError, KeyError, Ssh};
use crate::ports::system::{KernelProbe, System, SystemError};

/// `nix build` that records what it was asked and answers with a set status.
#[derive(Clone)]
pub struct FakeNix {
    script: Script,
    status: Arc<Mutex<BuildStatus>>,
    builds: Arc<Mutex<Vec<AbsPath>>>,
}

impl FakeNix {
    pub(crate) fn new(script: &Script) -> Self {
        Self {
            script: script.clone(),
            status: Arc::new(Mutex::new(BuildStatus::Built)),
            builds: Arc::default(),
        }
    }

    /// How later builds end.
    pub fn answer(&self, status: BuildStatus) {
        *held(&self.status) = status;
    }

    /// The out-links of the builds run so far.
    #[must_use]
    pub fn builds(&self) -> Vec<AbsPath> {
        held(&self.builds).clone()
    }
}

impl Nix for FakeNix {
    fn build(
        &self,
        _flake: &FlakeDir,
        _host_module: &FlakePath,
        out_link: &AbsPath,
    ) -> impl Future<Output = Result<BuildStatus, NixError>> + Send {
        let out_link = out_link.clone();
        step(&self.script, OpId::NixBuild, Fault::nix, move || {
            held(&self.builds).push(out_link);
            Ok(*held(&self.status))
        })
    }
}

/// A host that answers as configured: healthy unless a test changes a field.
#[derive(Clone)]
pub struct FakeSystem {
    script: Script,
    /// What the host answers; edit before running.
    pub answers: Arc<Mutex<HostAnswers>>,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct HostAnswers {
    pub kvm: KvmAccess,
    pub tools: HashMap<Tool, ToolPresence>,
    pub userns: UsernsSwitches,
    pub kernel: KernelProbe,
    pub legacy_qemu: Option<Pid>,
}

impl FakeSystem {
    pub(crate) fn new(script: &Script) -> Self {
        let answers = HostAnswers {
            kvm: KvmAccess::Usable,
            tools: Tool::ALL
                .into_iter()
                .map(|tool| (tool, ToolPresence::Found))
                .collect(),
            userns: UsernsSwitches {
                max_namespaces: Switch::On,
                unprivileged_clone: Switch::Absent,
                apparmor_restricts: Switch::Absent,
            },
            kernel: KernelVersion::parse("6.12.3"),
            legacy_qemu: None,
        };
        Self {
            script: script.clone(),
            answers: Arc::new(Mutex::new(answers)),
        }
    }

    fn answers(&self) -> HostAnswers {
        held(&self.answers).clone()
    }
}

impl System for FakeSystem {
    fn kvm(&self) -> impl Future<Output = Result<KvmAccess, SystemError>> + Send {
        step(&self.script, OpId::SystemKvm, Fault::system, move || {
            Ok(self.answers().kvm)
        })
    }

    fn tool(&self, tool: Tool) -> impl Future<Output = Result<ToolPresence, SystemError>> + Send {
        step(&self.script, OpId::SystemTool, Fault::system, move || {
            Ok(self
                .answers()
                .tools
                .get(&tool)
                .copied()
                .unwrap_or(ToolPresence::Missing))
        })
    }

    fn userns(&self) -> impl Future<Output = Result<UsernsSwitches, SystemError>> + Send {
        step(&self.script, OpId::SystemUserns, Fault::system, move || {
            Ok(self.answers().userns)
        })
    }

    fn kernel(&self) -> impl Future<Output = Result<KernelProbe, SystemError>> + Send {
        step(&self.script, OpId::SystemKernel, Fault::system, move || {
            Ok(self.answers().kernel)
        })
    }

    fn legacy_qemu(
        &self,
        _runtime: &AbsDir,
    ) -> impl Future<Output = Result<Option<Pid>, SystemError>> + Send {
        step(
            &self.script,
            OpId::SystemLegacyQemu,
            Fault::system,
            move || Ok(self.answers().legacy_qemu),
        )
    }
}

const KEY: &str =
    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIGq1ZPV0QcVQgRtuRZ8nvRNQdNnwpOAiIUUwwO8M3DJq";

/// ssh with one host key, ports counting up from `first_port`, and a fixed banner.
#[derive(Clone)]
pub struct FakeSsh {
    script: Script,
    next_port: Arc<Mutex<u16>>,
    /// Ports somebody else holds; `port_free` is false for them.
    taken: Arc<Mutex<Vec<LoopbackPort>>>,
    /// Ports that accept connections (the host forward of a running qemu).
    listening: Arc<Mutex<Vec<LoopbackPort>>>,
}

impl FakeSsh {
    pub(crate) fn new(script: &Script) -> Self {
        Self {
            script: script.clone(),
            next_port: Arc::new(Mutex::new(40_000)),
            taken: Arc::default(),
            listening: Arc::default(),
        }
    }

    /// Something else holds `port`.
    pub fn take(&self, port: LoopbackPort) {
        held(&self.taken).push(port);
    }

    /// `port` accepts connections from now on (with a banner).
    pub fn listen(&self, port: LoopbackPort) {
        held(&self.listening).push(port);
    }

    /// The port the next `free_port` hands out.
    #[must_use]
    pub fn next_port(&self) -> u16 {
        *held(&self.next_port)
    }
}

impl Ssh for FakeSsh {
    fn ensure_host_key(
        &self,
        _vm: &VmPaths,
    ) -> impl Future<Output = Result<HostPublicKey, KeyError>> + Send {
        step(
            &self.script,
            OpId::SshEnsureHostKey,
            Fault::key,
            move || {
                HostPublicKey::parse(KEY).map_err(|error| KeyError::Key {
                    path: AbsPath::root(),
                    error,
                })
            },
        )
    }

    fn free_port(&self) -> impl Future<Output = Result<LoopbackPort, FreePortError>> + Send {
        step(
            &self.script,
            OpId::SshFreePort,
            Fault::free_port,
            move || {
                let mut next = held(&self.next_port);
                let port = *next;
                *next = next.saturating_add(1);
                LoopbackPort::parse(port).map_err(FreePortError::Unusable)
            },
        )
    }

    fn banner(
        &self,
        port: LoopbackPort,
    ) -> impl Future<Output = Result<SshBanner, BannerError>> + Send {
        step(&self.script, OpId::SshBanner, Fault::banner, move || {
            if !held(&self.listening).contains(&port) {
                return Err(BannerError::Closed { port });
            }
            SshBanner::parse("SSH-2.0-OpenSSH_9.9".to_owned())
                .map_err(|error| BannerError::Invalid { port, error })
        })
    }

    async fn accepts(&self, port: LoopbackPort) -> bool {
        {
            self.script.enter(OpId::SshAccepts);
            let out = held(&self.listening).contains(&port);
            super::pause().await;
            out
        }
    }

    async fn port_free(&self, port: LoopbackPort) -> bool {
        {
            self.script.enter(OpId::SshPortFree);
            let out = !held(&self.taken).contains(&port);
            super::pause().await;
            out
        }
    }
}

/// qemu's control socket: remembers which sockets were told to power down.
#[derive(Clone)]
pub struct FakeQmp {
    script: Script,
    powered_down: Arc<Mutex<Vec<SocketPath>>>,
}

impl FakeQmp {
    pub(crate) fn new(script: &Script) -> Self {
        Self {
            script: script.clone(),
            powered_down: Arc::default(),
        }
    }

    /// The sockets that accepted a powerdown, in order.
    #[must_use]
    pub fn powered_down(&self) -> Vec<SocketPath> {
        held(&self.powered_down).clone()
    }
}

impl Qmp for FakeQmp {
    fn powerdown(&self, control: &SocketPath) -> impl Future<Output = Result<(), QmpError>> + Send {
        let control = control.clone();
        step(&self.script, OpId::QmpPowerdown, Fault::qmp, move || {
            held(&self.powered_down).push(control);
            Ok(())
        })
    }
}

/// The systemd session: linger per user, off unless set.
#[derive(Clone)]
pub struct FakeSession {
    script: Script,
    lingering: Arc<Mutex<Vec<UserName>>>,
}

impl FakeSession {
    pub(crate) fn new(script: &Script) -> Self {
        Self {
            script: script.clone(),
            lingering: Arc::default(),
        }
    }
}

impl Session for FakeSession {
    fn linger(&self, user: &UserName) -> impl Future<Output = Result<Linger, SessionError>> + Send {
        let user = user.clone();
        step(
            &self.script,
            OpId::SessionLinger,
            Fault::session,
            move || {
                Ok(if held(&self.lingering).contains(&user) {
                    Linger::On
                } else {
                    Linger::Off
                })
            },
        )
    }

    fn enable_linger(
        &self,
        user: &UserName,
    ) -> impl Future<Output = Result<(), SessionError>> + Send {
        let user = user.clone();
        step(
            &self.script,
            OpId::SessionEnableLinger,
            Fault::session,
            move || {
                held(&self.lingering).push(user);
                Ok(())
            },
        )
    }
}
