//! `Session`: the host user's systemd session (design §Ports and adapters). Without linger the
//! runtime dir goes away at the last logout and takes the VM's sockets with it; the library only
//! reports and, when asked, enables it. Prompts and policy live in the CLI and the ops.

use std::future::Future;

use crate::diagnostic::Diagnostic;
use crate::domain::{Linger, UserName};
use crate::ports::Cause;

#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum SessionError {
    /// `loginctl` could not be started (not installed, not on `PATH`).
    #[error("can't run loginctl: {cause}")]
    Spawn { cause: Cause },
    /// `loginctl enable-linger` ran and refused; `code` is `None` when a signal ended it.
    #[error("loginctl enable-linger failed: {}", describe(*.code))]
    Failed { code: Option<i32> },
    /// The blocking task did not finish (the runtime was shutting down).
    #[error("can't check linger: the operation was aborted")]
    Aborted,
}

fn describe(code: Option<i32>) -> String {
    code.map_or_else(
        || "it was ended by a signal".to_owned(),
        |code| format!("exit status {code}"),
    )
}

impl Diagnostic for SessionError {
    fn code(&self) -> &'static str {
        match self {
            Self::Spawn { .. } => "session.spawn",
            Self::Failed { .. } => "session.failed",
            Self::Aborted => "session.aborted",
        }
    }

    fn message(&self) -> String {
        self.to_string()
    }

    fn hint(&self) -> Option<String> {
        Some(
            match self {
                Self::Spawn { .. } => {
                    "install systemd's loginctl, or run `loginctl enable-linger` as root"
                }
                Self::Failed { .. } => "run `loginctl enable-linger USER` yourself and try again",
                Self::Aborted => "run the command again",
            }
            .to_owned(),
        )
    }
}

pub trait Session {
    /// Whether systemd keeps `user`'s session after the last logout. Never fails for a reason
    /// that says anything about linger: a marker that can't be looked at counts as off.
    fn linger(&self, user: &UserName) -> impl Future<Output = Result<Linger, SessionError>> + Send;

    /// `loginctl enable-linger USER`, without a terminal: where authorisation needs a prompt it
    /// fails ([`SessionError::Failed`]) rather than asking.
    fn enable_linger(
        &self,
        user: &UserName,
    ) -> impl Future<Output = Result<(), SessionError>> + Send;
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) mod tests {
    use std::io;

    use super::*;
    use crate::testing::{self, Entry};

    fn witness(e: &SessionError) -> &'static str {
        match e {
            SessionError::Spawn { .. } => "Spawn",
            SessionError::Failed { .. } => "Failed",
            SessionError::Aborted => "Aborted",
        }
    }

    pub(crate) fn errors() -> Vec<SessionError> {
        vec![
            SessionError::Spawn {
                cause: Cause {
                    kind: io::ErrorKind::NotFound,
                    errno: Some(2),
                },
            },
            SessionError::Failed { code: Some(1) },
            SessionError::Aborted,
        ]
    }

    /// One catalogue entry per variant.
    pub(crate) fn entries() -> Vec<Entry> {
        errors().iter().map(Entry::of).collect()
    }

    #[test]
    fn every_error_variant_has_a_table_case() {
        testing::every_variant(&errors(), witness, &["Spawn", "Failed", "Aborted"]);
    }

    #[test]
    fn messages_say_what_failed() {
        let messages: Vec<String> = errors().iter().map(Diagnostic::message).collect();
        assert_eq!(
            messages,
            [
                "can't run loginctl: entity not found",
                "loginctl enable-linger failed: exit status 1",
                "can't check linger: the operation was aborted",
            ]
        );
    }

    #[test]
    fn a_signalled_loginctl_is_described_without_a_status() {
        assert_eq!(
            SessionError::Failed { code: None }.message(),
            "loginctl enable-linger failed: it was ended by a signal"
        );
    }
}
