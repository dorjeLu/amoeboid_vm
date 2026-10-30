//! `Ssh`: the guest's ssh identity and talking to its sshd (design §Ports and adapters): the host
//! key (generated once, outside the guest-writable share), a free loopback port, and the banner
//! probe. Pinning the key in `known_hosts` is a `HostFs` write done by `ops::host_key`.

use std::fmt;
use std::future::Future;
use std::time::Duration;

use crate::diagnostic::Diagnostic;
use crate::domain::{AbsPath, HostPublicKey, HostPublicKeyError, LoopbackPort, LoopbackPortError};
use crate::domain::{SshBanner, SshBannerError};
use crate::layout::VmPaths;
use crate::ports::Cause;
use crate::ports::host_fs::{FsOp, io_hint};

/// How long the banner probe waits for the connection, and then for the banner.
pub const BANNER_WAIT: Duration = Duration::from_secs(2);

#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum KeyError {
    #[error("can't {op} {path}: {cause}")]
    Io {
        op: FsOp,
        path: AbsPath,
        cause: Cause,
    },
    /// `ssh-keygen` could not be started.
    #[error("can't run ssh-keygen: {cause}")]
    Spawn { cause: Cause },
    /// `ssh-keygen` ran and failed; `code` is `None` when a signal ended it. Its own message went
    /// to the terminal.
    #[error("ssh-keygen failed: {}", describe(*.code))]
    Failed { code: Option<i32> },
    /// The public key file is not an ed25519 key.
    #[error("can't use the host key in {path}: {error}")]
    Key {
        path: AbsPath,
        error: HostPublicKeyError,
    },
    /// The blocking task did not finish (the runtime was shutting down).
    #[error("can't generate the host key: the operation was aborted")]
    Aborted,
}

fn describe(code: Option<i32>) -> String {
    code.map_or_else(
        || "it was ended by a signal".to_owned(),
        |code| format!("exit status {code}"),
    )
}

impl Diagnostic for KeyError {
    fn code(&self) -> &'static str {
        match self {
            Self::Io { .. } => "ssh.key.io",
            Self::Spawn { .. } => "ssh.key.spawn",
            Self::Failed { .. } => "ssh.key.failed",
            Self::Key { .. } => "ssh.key.invalid",
            Self::Aborted => "ssh.key.aborted",
        }
    }

    fn message(&self) -> String {
        self.to_string()
    }

    fn hint(&self) -> Option<String> {
        Some(match self {
            Self::Io { cause, .. } => io_hint(cause.kind).to_owned(),
            Self::Spawn { .. } => "install OpenSSH on the host".to_owned(),
            Self::Failed { .. } => "check the ssh-keygen message above".to_owned(),
            Self::Key { .. } => {
                "delete the VM's ssh directory so a new ed25519 key is generated".to_owned()
            }
            Self::Aborted => "run the command again".to_owned(),
        })
    }
}

#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum FreePortError {
    #[error("can't find a free port: {cause}")]
    Bind { cause: Cause },
    /// The kernel handed out a number that is no port.
    #[error("can't find a free port: {0}")]
    Unusable(LoopbackPortError),
}

impl Diagnostic for FreePortError {
    fn code(&self) -> &'static str {
        match self {
            Self::Bind { .. } => "ssh.port.bind",
            Self::Unusable(_) => "ssh.port.unusable",
        }
    }

    fn message(&self) -> String {
        self.to_string()
    }

    fn hint(&self) -> Option<String> {
        Some("choose a port with `ssh.port` in the config, or free local ports".to_owned())
    }
}

/// The step of the probe a failure happened in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BannerPhase {
    Connect,
    /// Waiting for the first line.
    Banner,
}

impl fmt::Display for BannerPhase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Connect => f.write_str("connecting"),
            Self::Banner => f.write_str("waiting for the banner"),
        }
    }
}

/// Why sshd did not answer (yet). While the VM boots most of these are expected.
#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum BannerError {
    #[error("can't connect to port {port}: {cause}")]
    Connect { port: LoopbackPort, cause: Cause },
    #[error("can't read from port {port}: {cause}")]
    Read { port: LoopbackPort, cause: Cause },
    #[error("nothing answered on port {port} within {} seconds while {phase}", BANNER_WAIT.as_secs())]
    Timeout {
        port: LoopbackPort,
        phase: BannerPhase,
    },
    /// The connection closed before a byte came.
    #[error("port {port} closed the connection without a banner")]
    Closed { port: LoopbackPort },
    #[error("port {port} didn't answer with an ssh banner: {error}")]
    Invalid {
        port: LoopbackPort,
        error: SshBannerError,
    },
}

impl Diagnostic for BannerError {
    fn code(&self) -> &'static str {
        match self {
            Self::Connect { .. } => "ssh.banner.connect",
            Self::Read { .. } => "ssh.banner.read",
            Self::Timeout { .. } => "ssh.banner.timeout",
            Self::Closed { .. } => "ssh.banner.closed",
            Self::Invalid { .. } => "ssh.banner.invalid",
        }
    }

    fn message(&self) -> String {
        self.to_string()
    }

    fn hint(&self) -> Option<String> {
        Some("see the console log for why sshd isn't up".to_owned())
    }
}

/// Implemented by `adapters::StdSsh`; tests of the use cases use a fake.
pub trait Ssh {
    /// The VM's host public key, generating the key pair first if there is none. The key is made
    /// in `VmPaths::host_key_staging` and its files renamed into `ssh/` (public half first, so the
    /// private key's presence means a complete pair); whatever is left in the staging directory
    /// is cleared, never used. The caller holds the VM lock.
    fn ensure_host_key(
        &self,
        vm: &VmPaths,
    ) -> impl Future<Output = Result<HostPublicKey, KeyError>> + Send;

    /// A loopback port nobody listens on now (the kernel's pick; another process may take it
    /// before it is used).
    fn free_port(&self) -> impl Future<Output = Result<LoopbackPort, FreePortError>> + Send;

    /// The identification line sshd sends to a connection on `port`. qemu accepts on the
    /// forwarded port as soon as it starts, so only a banner proves sshd is up. Each wait is
    /// bounded by [`BANNER_WAIT`]; a banner that arrived in part counts if what came is one.
    fn banner(
        &self,
        port: LoopbackPort,
    ) -> impl Future<Output = Result<SshBanner, BannerError>> + Send;

    /// Whether something accepts a connection on 127.0.0.1:`port` (for qemu: the host forward is
    /// bound, whether or not the guest's sshd is up).
    fn accepts(&self, port: LoopbackPort) -> impl Future<Output = bool> + Send;

    /// Whether 127.0.0.1:`port` can be bound now; false when something else holds it.
    fn port_free(&self, port: LoopbackPort) -> impl Future<Output = bool> + Send;
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) mod tests {
    use std::io;

    use super::*;
    use crate::domain::KeyTypeError;
    use crate::testing::{self, Entry};

    fn path() -> AbsPath {
        AbsPath::parse("/d/proj/ssh/ssh_host_ed25519_key.pub".to_owned()).unwrap()
    }

    fn port() -> LoopbackPort {
        LoopbackPort::parse(2222).unwrap()
    }

    fn cause(kind: io::ErrorKind, errno: i32) -> Cause {
        Cause {
            kind,
            errno: Some(errno),
        }
    }

    fn key_witness(e: &KeyError) -> &'static str {
        match e {
            KeyError::Io { .. } => "Io",
            KeyError::Spawn { .. } => "Spawn",
            KeyError::Failed { .. } => "Failed",
            KeyError::Key { .. } => "Key",
            KeyError::Aborted => "Aborted",
        }
    }

    pub(crate) fn key_errors() -> Vec<KeyError> {
        vec![
            KeyError::Io {
                op: FsOp::Rename,
                path: path(),
                cause: cause(io::ErrorKind::PermissionDenied, 13),
            },
            KeyError::Spawn {
                cause: cause(io::ErrorKind::NotFound, 2),
            },
            KeyError::Failed { code: Some(1) },
            KeyError::Key {
                path: path(),
                error: HostPublicKeyError::Type(KeyTypeError::Unsupported {
                    found: "ssh-rsa".to_owned(),
                }),
            },
            KeyError::Aborted,
        ]
    }

    fn port_witness(e: &FreePortError) -> &'static str {
        match e {
            FreePortError::Bind { .. } => "Bind",
            FreePortError::Unusable(_) => "Unusable",
        }
    }

    pub(crate) fn port_errors() -> Vec<FreePortError> {
        vec![
            FreePortError::Bind {
                cause: cause(io::ErrorKind::AddrNotAvailable, 99),
            },
            FreePortError::Unusable(LoopbackPortError::Zero),
        ]
    }

    fn banner_witness(e: &BannerError) -> &'static str {
        match e {
            BannerError::Connect { .. } => "Connect",
            BannerError::Read { .. } => "Read",
            BannerError::Timeout { .. } => "Timeout",
            BannerError::Closed { .. } => "Closed",
            BannerError::Invalid { .. } => "Invalid",
        }
    }

    pub(crate) fn banner_errors() -> Vec<BannerError> {
        vec![
            BannerError::Connect {
                port: port(),
                cause: cause(io::ErrorKind::ConnectionRefused, 111),
            },
            BannerError::Read {
                port: port(),
                cause: cause(io::ErrorKind::ConnectionReset, 104),
            },
            BannerError::Timeout {
                port: port(),
                phase: BannerPhase::Banner,
            },
            BannerError::Closed { port: port() },
            BannerError::Invalid {
                port: port(),
                error: SshBannerError::NotSsh,
            },
        ]
    }

    /// One catalogue entry per variant.
    pub(crate) fn entries() -> Vec<Entry> {
        key_errors()
            .iter()
            .map(Entry::of)
            .chain(port_errors().iter().map(Entry::of))
            .chain(banner_errors().iter().map(Entry::of))
            .collect()
    }

    #[test]
    fn every_error_variant_has_a_table_case() {
        testing::every_variant(
            &key_errors(),
            key_witness,
            &["Io", "Spawn", "Failed", "Key", "Aborted"],
        );
        testing::every_variant(&port_errors(), port_witness, &["Bind", "Unusable"]);
        testing::every_variant(
            &banner_errors(),
            banner_witness,
            &["Connect", "Read", "Timeout", "Closed", "Invalid"],
        );
    }

    #[test]
    fn messages_name_the_operation_the_path_or_port_and_the_cause() {
        let mut messages: Vec<String> = key_errors().iter().map(Diagnostic::message).collect();
        messages.extend(port_errors().iter().map(Diagnostic::message));
        messages.extend(banner_errors().iter().map(Diagnostic::message));
        assert_eq!(
            messages,
            [
                "can't rename /d/proj/ssh/ssh_host_ed25519_key.pub: permission denied",
                "can't run ssh-keygen: entity not found",
                "ssh-keygen failed: exit status 1",
                "can't use the host key in /d/proj/ssh/ssh_host_ed25519_key.pub: \
                 unsupported host key type \"ssh-rsa\": only ssh-ed25519 is used",
                "can't generate the host key: the operation was aborted",
                "can't find a free port: address not available",
                "can't find a free port: bad port: 0 is not a port number",
                "can't connect to port 2222: connection refused",
                "can't read from port 2222: connection reset",
                "nothing answered on port 2222 within 2 seconds while waiting for the banner",
                "port 2222 closed the connection without a banner",
                "port 2222 didn't answer with an ssh banner: \
                 not an ssh banner: it doesn't start with SSH-",
            ]
        );
    }

    #[test]
    fn a_signalled_keygen_and_each_phase_have_their_own_words() {
        assert_eq!(
            KeyError::Failed { code: None }.message(),
            "ssh-keygen failed: it was ended by a signal"
        );
        assert_eq!(BannerPhase::Connect.to_string(), "connecting");
        assert_eq!(BannerPhase::Banner.to_string(), "waiting for the banner");
    }

    #[test]
    fn hints_are_remedies_for_the_kind_of_failure() {
        let hints: Vec<Option<String>> = key_errors().iter().map(Diagnostic::hint).collect();
        assert_eq!(
            hints,
            [
                "fix the permissions of the path, or run as its owner",
                "install OpenSSH on the host",
                "check the ssh-keygen message above",
                "delete the VM's ssh directory so a new ed25519 key is generated",
                "run the command again",
            ]
            .map(|hint| Some(hint.to_owned()))
        );
    }
}
