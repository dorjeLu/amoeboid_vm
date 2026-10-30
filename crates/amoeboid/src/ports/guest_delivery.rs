//! `GuestDelivery`: putting files where the guest reads them (design §Ports and adapters).
//!
//! The target is the guest-writable share, so nothing here trusts what is already in it: an
//! existing entry is replaced, never opened or written through; a symlink anywhere on the way
//! makes the call fail with the offending path; the host never reads back what the guest left to
//! decide anything.

use std::future::Future;

use crate::diagnostic::Diagnostic;
use crate::domain::{AbsPath, GuestFile, GuestRel};
use crate::ports::Cause;
use crate::ports::host_fs::{FsOp, io_hint};

#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum DeliveryError {
    /// Something in the share is not what the host expected: a symlink on the way, a file where a
    /// directory should be or the reverse, or an entry planted at a name the host needs. `path`
    /// is the entry that stopped the call.
    #[error("can't {op} {path}: {cause}")]
    Blocked {
        op: FsOp,
        path: AbsPath,
        cause: Cause,
    },
    #[error("can't {op} {path}: {cause}")]
    Io {
        op: FsOp,
        path: AbsPath,
        cause: Cause,
    },
    /// The blocking task did not finish (the runtime was shutting down).
    #[error("can't {op} {path}: the operation was aborted")]
    Aborted { op: FsOp, path: AbsPath },
}

impl Diagnostic for DeliveryError {
    fn code(&self) -> &'static str {
        match self {
            Self::Blocked { .. } => "delivery.blocked",
            Self::Io { .. } => "delivery.io",
            Self::Aborted { .. } => "delivery.aborted",
        }
    }

    fn message(&self) -> String {
        self.to_string()
    }

    fn hint(&self) -> Option<String> {
        Some(match self {
            Self::Blocked { .. } => "something inside the VM may have replaced a directory there \
                 with a symlink; inspect it and remove the offending entry"
                .to_owned(),
            Self::Io { cause, .. } => io_hint(cause.kind).to_owned(),
            Self::Aborted { .. } => "run the command again".to_owned(),
        })
    }
}

/// The guest-writable share. Implemented by `adapters::ShareDelivery`; tests of the use cases use
/// a fake. Paths are relative to the share root, which the implementation holds.
pub trait GuestDelivery {
    /// Makes `dir` and its missing parents, as real directories.
    fn ensure_dir(&self, dir: &GuestRel) -> impl Future<Output = Result<(), DeliveryError>> + Send;

    /// Replaces whatever is at `file.dest` with a new regular file (parents are made). A symlink
    /// there is replaced, not followed; a directory there is an error.
    fn put(&self, file: &GuestFile) -> impl Future<Output = Result<(), DeliveryError>> + Send;

    /// Removes the file or symlink at `rel`; nothing there (or no parent) is fine. A directory
    /// there is an error.
    fn remove(&self, rel: &GuestRel) -> impl Future<Output = Result<(), DeliveryError>> + Send;

    /// Makes `dir` contain exactly `files` (their `dest` relative to `dir`): the old tree is moved
    /// into the VM's trash and deleted there, then the files are put.
    fn replace_tree(
        &self,
        dir: &GuestRel,
        files: &[GuestFile],
    ) -> impl Future<Output = Result<(), DeliveryError>> + Send;
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) mod tests {
    use std::io;

    use super::*;
    use crate::testing::{self, Entry};

    fn path(text: &str) -> AbsPath {
        AbsPath::parse(text.to_owned()).unwrap()
    }

    fn cause(kind: io::ErrorKind, errno: i32) -> Cause {
        Cause {
            kind,
            errno: Some(errno),
        }
    }

    fn witness(e: &DeliveryError) -> &'static str {
        match e {
            DeliveryError::Blocked { .. } => "Blocked",
            DeliveryError::Io { .. } => "Io",
            DeliveryError::Aborted { .. } => "Aborted",
        }
    }

    pub(crate) fn errors() -> Vec<DeliveryError> {
        vec![
            DeliveryError::Blocked {
                op: FsOp::Open,
                path: path("/d/vm/guest/home"),
                cause: cause(io::ErrorKind::Other, 40),
            },
            DeliveryError::Io {
                op: FsOp::Write,
                path: path("/d/vm/guest/mounts"),
                cause: cause(io::ErrorKind::StorageFull, 28),
            },
            DeliveryError::Aborted {
                op: FsOp::Write,
                path: path("/d/vm/guest/mounts"),
            },
        ]
    }

    /// One catalogue entry per variant.
    pub(crate) fn entries() -> Vec<Entry> {
        errors().iter().map(Entry::of).collect()
    }

    #[test]
    fn every_error_variant_has_a_table_case() {
        testing::every_variant(&errors(), witness, &["Blocked", "Io", "Aborted"]);
    }

    #[test]
    fn messages_name_the_operation_the_entry_and_the_cause() {
        let messages: Vec<String> = errors().iter().map(Diagnostic::message).collect();
        assert_eq!(
            messages,
            [
                "can't open /d/vm/guest/home: other error",
                "can't write /d/vm/guest/mounts: no storage space",
                "can't write /d/vm/guest/mounts: the operation was aborted",
            ]
        );
    }

    #[test]
    fn a_blocked_entry_hints_at_the_guest_and_other_errors_at_the_cause() {
        let hints: Vec<Option<String>> = errors().iter().map(Diagnostic::hint).collect();
        assert_eq!(
            hints,
            [
                Some(
                    "something inside the VM may have replaced a directory there with a \
                     symlink; inspect it and remove the offending entry"
                        .to_owned()
                ),
                Some("free disk space and try again".to_owned()),
                Some("run the command again".to_owned()),
            ]
        );
    }
}
