//! `Qmp`: asking qemu over its control socket to press the power button (design §Ports and
//! adapters). Just enough QMP: negotiate capabilities, `system_powerdown`; events are skipped.

use std::fmt;
use std::future::Future;
use std::time::Duration;

use crate::diagnostic::Diagnostic;
use crate::layout::SocketPath;
use crate::plan::{GreetingError, QmpCommand, QmpDesc, QmpErrorClass, ReplyError};
use crate::ports::Cause;

/// How long each step may take: connecting, the greeting, each command's reply.
pub const STEP_TIMEOUT: Duration = Duration::from_secs(5);

/// The longest line read from qemu, in bytes.
pub const MAX_LINE: usize = 65_536;

/// The step of the conversation a failure happened in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum QmpPhase {
    Connect,
    Greeting,
    /// Sending a command and waiting for its reply.
    Command(QmpCommand),
}

impl fmt::Display for QmpPhase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Connect => f.write_str("connecting"),
            Self::Greeting => f.write_str("waiting for the greeting"),
            Self::Command(command) => write!(f, "waiting for the reply to {command}"),
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum QmpError {
    #[error("can't connect to qemu on {socket}: {cause}")]
    Connect { socket: SocketPath, cause: Cause },
    /// Reading from or writing to the socket failed.
    #[error("can't talk to qemu while {phase}: {cause}")]
    Io { phase: QmpPhase, cause: Cause },
    #[error("qemu closed the control socket while {phase}")]
    Closed { phase: QmpPhase },
    #[error("qemu sent a line of more than {MAX_LINE} bytes while {phase}")]
    TooLong { phase: QmpPhase },
    #[error("{0}")]
    Greeting(GreetingError),
    #[error("bad reply to {command}: {error}")]
    Reply {
        command: QmpCommand,
        error: ReplyError,
    },
    /// qemu answered the command with an error.
    #[error("qemu refused {command}: {class}: {desc}")]
    Refused {
        command: QmpCommand,
        class: QmpErrorClass,
        desc: QmpDesc,
    },
    #[error("qemu didn't answer in {} seconds while {phase}", STEP_TIMEOUT.as_secs())]
    Timeout { phase: QmpPhase },
}

impl Diagnostic for QmpError {
    fn code(&self) -> &'static str {
        match self {
            Self::Connect { .. } => "qmp.connect",
            Self::Io { .. } => "qmp.io",
            Self::Closed { .. } => "qmp.closed",
            Self::TooLong { .. } => "qmp.too-long",
            Self::Greeting(_) => "qmp.greeting",
            Self::Reply { .. } => "qmp.reply",
            Self::Refused { .. } => "qmp.refused",
            Self::Timeout { .. } => "qmp.timeout",
        }
    }

    fn message(&self) -> String {
        self.to_string()
    }

    fn hint(&self) -> Option<String> {
        Some(
            match self {
                Self::Connect { .. } => {
                    "check that the VM is running; list VMs with `amoeboid list`"
                }
                Self::Io { .. } | Self::Closed { .. } | Self::Timeout { .. } => {
                    "the VM may be shutting down already; check `amoeboid list`"
                }
                Self::TooLong { .. } | Self::Greeting(_) | Self::Reply { .. } => {
                    "check that the control socket belongs to this VM's qemu"
                }
                Self::Refused { .. } => "check the VM's console for why qemu can't power off",
            }
            .to_owned(),
        )
    }
}

pub trait Qmp {
    /// Presses the VM's virtual power button; the guest shuts down cleanly and qemu exits. Returns
    /// when qemu has accepted the request, not when the guest is off.
    fn powerdown(&self, control: &SocketPath) -> impl Future<Output = Result<(), QmpError>> + Send;
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) mod tests {
    use std::io;

    use super::*;
    use crate::domain::AbsPath;
    use crate::testing::{self, Entry};

    fn socket() -> SocketPath {
        SocketPath::parse(AbsPath::parse("/run/user/1000/amb_vm/control.sock".to_owned()).unwrap())
            .unwrap()
    }

    fn cause() -> Cause {
        Cause {
            kind: io::ErrorKind::ConnectionRefused,
            errno: Some(111),
        }
    }

    fn witness(e: &QmpError) -> &'static str {
        match e {
            QmpError::Connect { .. } => "Connect",
            QmpError::Io { .. } => "Io",
            QmpError::Closed { .. } => "Closed",
            QmpError::TooLong { .. } => "TooLong",
            QmpError::Greeting(_) => "Greeting",
            QmpError::Reply { .. } => "Reply",
            QmpError::Refused { .. } => "Refused",
            QmpError::Timeout { .. } => "Timeout",
        }
    }

    pub(crate) fn errors() -> Vec<QmpError> {
        vec![
            QmpError::Connect {
                socket: socket(),
                cause: cause(),
            },
            QmpError::Io {
                phase: QmpPhase::Greeting,
                cause: cause(),
            },
            QmpError::Closed {
                phase: QmpPhase::Command(QmpCommand::Capabilities),
            },
            QmpError::TooLong {
                phase: QmpPhase::Greeting,
            },
            QmpError::Greeting(GreetingError::NotQmp),
            QmpError::Reply {
                command: QmpCommand::Powerdown,
                error: ReplyError::NotMessage,
            },
            QmpError::Refused {
                command: QmpCommand::Powerdown,
                class: QmpErrorClass::GenericError,
                desc: QmpDesc::of("no power button"),
            },
            QmpError::Timeout {
                phase: QmpPhase::Command(QmpCommand::Powerdown),
            },
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
                "Connect", "Io", "Closed", "TooLong", "Greeting", "Reply", "Refused", "Timeout",
            ],
        );
    }

    #[test]
    fn phases_read_as_what_the_launcher_was_doing() {
        let cases = [
            (QmpPhase::Connect, "connecting"),
            (QmpPhase::Greeting, "waiting for the greeting"),
            (
                QmpPhase::Command(QmpCommand::Powerdown),
                "waiting for the reply to system_powerdown",
            ),
        ];
        for (phase, text) in cases {
            assert_eq!(phase.to_string(), text);
        }
    }

    #[test]
    fn every_variant_has_a_hint_and_a_message_naming_the_failing_step() {
        for error in errors() {
            assert!(error.hint().is_some_and(|h| !h.is_empty()), "{error:?}");
        }
        assert_eq!(
            errors().iter().map(QmpError::message).collect::<Vec<_>>(),
            [
                "can't connect to qemu on /run/user/1000/amb_vm/control.sock: connection refused",
                "can't talk to qemu while waiting for the greeting: connection refused",
                "qemu closed the control socket while waiting for the reply to qmp_capabilities",
                "qemu sent a line of more than 65536 bytes while waiting for the greeting",
                "qemu's greeting isn't a QMP greeting",
                "bad reply to system_powerdown: qemu's reply is neither a return, an error nor an event",
                "qemu refused system_powerdown: GenericError: no power button",
                "qemu didn't answer in 5 seconds while waiting for the reply to system_powerdown",
            ]
        );
    }
}
