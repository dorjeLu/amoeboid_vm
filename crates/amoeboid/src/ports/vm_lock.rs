//! `VmLock`: the one lock of a VM (design §One lock). Every process of a run holds the same open
//! file description, so the lock is free exactly when no process of the VM is alive. The file is
//! created on demand and never deleted.

use std::future::Future;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::process::Stdio;

use crate::diagnostic::Diagnostic;
use crate::domain::AbsPath;
use crate::ports::Cause;

/// What the call was doing when it failed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LockOp {
    /// Opening (or creating) the lock file.
    Open,
    /// Taking the lock.
    Lock,
    /// Duplicating the descriptor for a child.
    Duplicate,
}

impl LockOp {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Lock => "lock",
            Self::Duplicate => "hand on",
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum LockError {
    #[error("can't {op} {path}: {cause}", op = .op.as_str())]
    Io {
        op: LockOp,
        path: AbsPath,
        cause: Cause,
    },
    /// Another open file description holds the lock: a process of the VM is alive.
    #[error("can't lock {path}: a process of the VM still holds it")]
    Held { path: AbsPath },
    /// The filesystem has no advisory locks (`flock`).
    #[error("can't lock {path}: the filesystem doesn't support locks")]
    Unsupported { path: AbsPath },
    /// The blocking task did not finish (the runtime was shutting down).
    #[error("can't lock {path}: the operation was aborted")]
    Aborted { path: AbsPath },
}

impl Diagnostic for LockError {
    fn code(&self) -> &'static str {
        match self {
            Self::Io { .. } => "lock.io",
            Self::Held { .. } => "lock.held",
            Self::Unsupported { .. } => "lock.unsupported",
            Self::Aborted { .. } => "lock.aborted",
        }
    }

    fn message(&self) -> String {
        self.to_string()
    }

    fn hint(&self) -> Option<String> {
        Some(
            match self {
                Self::Io { .. } => "check the permissions of the VM's data directory",
                Self::Held { .. } => "stop the VM first, then try again",
                Self::Unsupported { .. } => {
                    "keep the data directory on a filesystem that supports file locks"
                }
                Self::Aborted { .. } => "run the command again",
            }
            .to_owned(),
        )
    }
}

/// Whether a process of the VM holds the lock.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Availability {
    Free,
    Held,
}

/// A descriptor of the held lock that a child process inherits by being spawned while it is open
/// (`FD_CLOEXEC` cleared). Close it in the parent right after the spawn by dropping it.
///
/// Spawns are sequential in our process: a child spawned while one of these is open inherits the
/// lock too, which would keep the VM looking alive after the run ended.
#[derive(Debug)]
pub struct Inheritable(OwnedFd);

impl Inheritable {
    pub(crate) const fn new(fd: OwnedFd) -> Self {
        Self(fd)
    }
}

impl AsFd for Inheritable {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.0.as_fd()
    }
}

/// The lock, held until this value and every handle derived from it are dropped, in this process
/// and in the children that inherited it.
pub trait HeldLock {
    /// `Stdio` for a child's stdin (virtiofsd, which does not read it).
    ///
    /// # Errors
    ///
    /// The descriptor can't be duplicated.
    fn stdin(&self) -> Result<Stdio, LockError>;

    /// A descriptor without `FD_CLOEXEC` for a child that has no stdin to spare (qemu).
    ///
    /// # Errors
    ///
    /// The descriptor can't be duplicated.
    fn inheritable(&self) -> Result<Inheritable, LockError>;
}

pub trait VmLock {
    type Held: HeldLock + Send;

    /// Creates the lock file if needed and takes the lock without waiting
    /// (`LOCK_EX | LOCK_NB`): [`LockError::Held`] when another holder exists.
    fn acquire(&self, path: &AbsPath)
    -> impl Future<Output = Result<Self::Held, LockError>> + Send;

    /// Whether the lock is free, without keeping it and without creating the file (a missing
    /// file has no holder). Never modifies anything.
    fn probe(&self, path: &AbsPath)
    -> impl Future<Output = Result<Availability, LockError>> + Send;
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) mod tests {
    use std::io;

    use super::*;
    use crate::testing::{self, Entry};

    fn path() -> AbsPath {
        AbsPath::parse("/data/proj/lock".to_owned()).unwrap()
    }

    fn witness(e: &LockError) -> &'static str {
        match e {
            LockError::Io { .. } => "Io",
            LockError::Held { .. } => "Held",
            LockError::Unsupported { .. } => "Unsupported",
            LockError::Aborted { .. } => "Aborted",
        }
    }

    pub(crate) fn errors() -> Vec<LockError> {
        vec![
            LockError::Io {
                op: LockOp::Open,
                path: path(),
                cause: Cause {
                    kind: io::ErrorKind::PermissionDenied,
                    errno: Some(13),
                },
            },
            LockError::Held { path: path() },
            LockError::Unsupported { path: path() },
            LockError::Aborted { path: path() },
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
            &["Io", "Held", "Unsupported", "Aborted"],
        );
    }

    #[test]
    fn every_operation_names_what_it_was_doing() {
        testing::table(
            &[
                (LockOp::Open, "open"),
                (LockOp::Lock, "lock"),
                (LockOp::Duplicate, "hand on"),
            ],
            |op| op.as_str(),
        );
    }

    #[test]
    fn messages_name_the_lock_file() {
        let messages: Vec<String> = errors().iter().map(Diagnostic::message).collect();
        assert_eq!(
            messages,
            [
                "can't open /data/proj/lock: permission denied",
                "can't lock /data/proj/lock: a process of the VM still holds it",
                "can't lock /data/proj/lock: the filesystem doesn't support locks",
                "can't lock /data/proj/lock: the operation was aborted",
            ]
        );
    }
}
