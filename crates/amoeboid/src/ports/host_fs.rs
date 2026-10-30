//! `HostFs`: files the host owns (design §Ports and adapters). Every write is atomic: readers see
//! the old content or the new, and a cancelled or failed call leaves no partial file behind.
//! Nothing here is for guest-writable directories; those go through `GuestDelivery`.

use std::fmt;
use std::future::Future;
use std::io;

use crate::diagnostic::Diagnostic;
use crate::domain::{AbsDir, AbsPath, FileMode, Observation, TreeFile};
use crate::ports::Cause;

/// What a failed call was doing, named for the message (`can't <op> <path>`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FsOp {
    /// Make a file or directory.
    Create,
    /// Open a directory to work inside it.
    Open,
    Write,
    /// Flush a file or directory to disk.
    Sync,
    Rename,
    /// Swap two directories (`renameat2` with `RENAME_EXCHANGE`).
    Exchange,
    Remove,
    Read,
    /// List a directory.
    List,
    Stat,
    /// Set permission bits.
    Chmod,
}

impl FsOp {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::Open => "open",
            Self::Write => "write",
            Self::Sync => "sync",
            Self::Rename => "rename",
            Self::Exchange => "swap",
            Self::Remove => "remove",
            Self::Read => "read",
            Self::List => "list",
            Self::Stat => "inspect",
            Self::Chmod => "set permissions on",
        }
    }
}

impl fmt::Display for FsOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum FsError {
    /// `entry` names the failing file below `path` when `path` is a directory being walked
    /// (relative, for display only: it may be lossy).
    #[error("can't {op} {path}{}: {cause}", entry_suffix(.entry.as_ref()))]
    Io {
        op: FsOp,
        path: AbsPath,
        entry: Option<String>,
        cause: Cause,
    },
    /// The directory tree nests deeper than the copy follows.
    #[error("can't copy {path}: it nests deeper than {max} levels")]
    TooDeep { path: AbsDir, max: usize },
    /// A file name in a tree that can't be a path of ours (not UTF-8). `entry` is relative to
    /// `path` and lossy, for display only.
    #[error("can't read {path}/{entry}: it isn't a valid file name (not UTF-8)")]
    BadName { path: AbsDir, entry: String },
    /// The blocking task did not finish (the runtime was shutting down).
    #[error("can't {op} {path}: the operation was aborted")]
    Aborted { op: FsOp, path: AbsPath },
}

fn entry_suffix(entry: Option<&String>) -> String {
    entry.map(|name| format!("/{name}")).unwrap_or_default()
}

impl Diagnostic for FsError {
    fn code(&self) -> &'static str {
        match self {
            Self::Io { .. } => "fs.io",
            Self::TooDeep { .. } => "fs.too-deep",
            Self::BadName { .. } => "fs.bad-name",
            Self::Aborted { .. } => "fs.aborted",
        }
    }

    fn message(&self) -> String {
        self.to_string()
    }

    fn hint(&self) -> Option<String> {
        Some(match self {
            Self::Io { cause, .. } => io_hint(cause.kind).to_owned(),
            Self::TooDeep { .. } => "use a shallower directory tree".to_owned(),
            Self::BadName { .. } => "rename the file to a UTF-8 name".to_owned(),
            Self::Aborted { .. } => "run the command again".to_owned(),
        })
    }
}

pub(crate) fn io_hint(kind: io::ErrorKind) -> &'static str {
    if kind == io::ErrorKind::PermissionDenied {
        "fix the permissions of the path, or run as its owner"
    } else if kind == io::ErrorKind::StorageFull {
        "free disk space and try again"
    } else if kind == io::ErrorKind::NotFound {
        "check that the path exists"
    } else {
        "check the path and its permissions"
    }
}

/// How [`HostFs::replace_dir`] put the new tree in place.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Replacement {
    /// One `renameat2(RENAME_EXCHANGE)` (or a plain rename onto a missing target): a crash leaves
    /// the old tree or the new one.
    Atomic,
    /// The filesystem can't exchange directories, so the old tree was renamed aside, the new one
    /// renamed in and the old one removed: a crash in between leaves no tree at `target` (the old
    /// one is kept beside it as `.NAME.old` and the new as `.NAME.new`). The caller should warn.
    NonAtomic,
}

/// Host-owned files. Implemented by `adapters::StdFs`; tests of the use cases use a fake.
pub trait HostFs {
    /// Replaces `path` (or creates it) with `bytes` and permission bits `mode`, atomically: temp
    /// file in the same directory, `fsync`, rename, `fsync` of the directory. Never writes
    /// through a symlink at `path` (it is replaced). The parent directory must exist.
    fn write_file(
        &self,
        path: &AbsPath,
        bytes: Vec<u8>,
        mode: FileMode,
    ) -> impl Future<Output = Result<(), FsError>> + Send;

    /// What is at `path`, with symlinks resolved: the probe behind the observed types
    /// (`ExistingDir`, `ExistingFile`, `Executable`, `HostDir`, `GuestHome`). Anything the probe
    /// can't look at or hold is an [`Observation`] too (`Missing`, `Other`); only a task that
    /// didn't finish is an error. Run it for every path of an operation before deciding anything,
    /// then hand the answers to the pure `observe` constructors.
    fn observe(&self, path: &AbsPath) -> impl Future<Output = Result<Observation, FsError>> + Send;

    fn read_file(&self, path: &AbsPath) -> impl Future<Output = Result<Vec<u8>, FsError>> + Send;

    /// The permission bits of the file at `path` (symlinks followed).
    fn file_mode(&self, path: &AbsPath) -> impl Future<Output = Result<FileMode, FsError>> + Send;

    /// Every regular file below `root`, read, ordered as a depth-first walk with names sorted at
    /// each level. Symlinks (to files or directories) and special files are left out; nothing is
    /// excluded by name (the tree is a host-owned copy).
    fn read_tree(
        &self,
        root: &AbsDir,
    ) -> impl Future<Output = Result<Vec<TreeFile>, FsError>> + Send;

    /// Removes the file (or symlink, or any non-directory) at `path`; a missing path is fine.
    /// A directory is refused: use [`HostFs::clear_dir`].
    fn remove_file(&self, path: &AbsPath) -> impl Future<Output = Result<(), FsError>> + Send;

    /// Removes `dir` with everything below it (symlinks are removed, never followed); a missing
    /// directory is fine. A path that is not a directory is refused.
    fn clear_dir(&self, dir: &AbsDir) -> impl Future<Output = Result<(), FsError>> + Send;

    /// Makes `dir` and its missing parents with `mode`; succeeds if `dir` is already a directory.
    fn create_dir_all(
        &self,
        dir: &AbsDir,
        mode: FileMode,
    ) -> impl Future<Output = Result<(), FsError>> + Send;

    /// Makes `target` a copy of `source` (without `__pycache__`, `result` and `.git`), swapping it
    /// in atomically where the filesystem allows: a crash leaves the old tree or the new one,
    /// never neither. A missing `target` is created. On a filesystem without exchange the
    /// two-rename sequence is used and [`Replacement::NonAtomic`] says so.
    fn replace_dir(
        &self,
        target: &AbsDir,
        source: &AbsDir,
    ) -> impl Future<Output = Result<Replacement, FsError>> + Send;
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) mod tests {
    use super::*;
    use crate::testing::{self, Entry};

    fn path(text: &str) -> AbsPath {
        AbsPath::parse(text.to_owned()).unwrap()
    }

    fn dir(text: &str) -> AbsDir {
        AbsDir::parse(text.to_owned()).unwrap()
    }

    fn io_error(op: FsOp, entry: Option<&str>, kind: io::ErrorKind, errno: Option<i32>) -> FsError {
        FsError::Io {
            op,
            path: path("/data/vm/nix"),
            entry: entry.map(str::to_owned),
            cause: Cause { kind, errno },
        }
    }

    fn witness(e: &FsError) -> &'static str {
        match e {
            FsError::Io { .. } => "Io",
            FsError::TooDeep { .. } => "TooDeep",
            FsError::BadName { .. } => "BadName",
            FsError::Aborted { .. } => "Aborted",
        }
    }

    pub(crate) fn errors() -> Vec<FsError> {
        vec![
            io_error(
                FsOp::Write,
                Some("a/b.nix"),
                io::ErrorKind::StorageFull,
                Some(28),
            ),
            FsError::TooDeep {
                path: dir("/presets/default"),
                max: 64,
            },
            FsError::BadName {
                path: dir("/data/proj/nix"),
                entry: "sub/\u{fffd}.nix".to_owned(),
            },
            FsError::Aborted {
                op: FsOp::Write,
                path: path("/data/run_config.kdl"),
            },
        ]
    }

    /// One catalogue entry per variant.
    pub(crate) fn entries() -> Vec<Entry> {
        errors().iter().map(Entry::of).collect()
    }

    #[test]
    fn every_error_variant_has_a_table_case() {
        testing::every_variant(&errors(), witness, &["Io", "TooDeep", "BadName", "Aborted"]);
    }

    #[test]
    fn messages_name_the_operation_the_path_the_entry_and_the_cause() {
        let cases = [
            (
                io_error(
                    FsOp::Create,
                    None,
                    io::ErrorKind::PermissionDenied,
                    Some(13),
                ),
                "can't create /data/vm/nix: permission denied",
            ),
            (
                io_error(FsOp::Read, Some("x/y"), io::ErrorKind::NotFound, Some(2)),
                "can't read /data/vm/nix/x/y: entity not found",
            ),
            (
                io_error(FsOp::Chmod, None, io::ErrorKind::Other, None),
                "can't set permissions on /data/vm/nix: other error",
            ),
            (
                FsError::TooDeep {
                    path: dir("/p"),
                    max: 64,
                },
                "can't copy /p: it nests deeper than 64 levels",
            ),
            (
                FsError::BadName {
                    path: dir("/p"),
                    entry: "a/\u{fffd}".to_owned(),
                },
                "can't read /p/a/\u{fffd}: it isn't a valid file name (not UTF-8)",
            ),
            (
                FsError::Aborted {
                    op: FsOp::Exchange,
                    path: path("/d"),
                },
                "can't swap /d: the operation was aborted",
            ),
        ];
        for (index, (error, expected)) in cases.iter().enumerate() {
            assert_eq!(error.message(), *expected, "case {index}");
        }
    }

    #[test]
    fn hints_depend_on_why_the_call_failed() {
        let cases = [
            (
                io::ErrorKind::PermissionDenied,
                "fix the permissions of the path, or run as its owner",
            ),
            (io::ErrorKind::StorageFull, "free disk space and try again"),
            (io::ErrorKind::NotFound, "check that the path exists"),
            (io::ErrorKind::Other, "check the path and its permissions"),
        ];
        for (kind, expected) in cases {
            let hint = io_error(FsOp::Write, None, kind, None).hint();
            assert_eq!(hint.as_deref(), Some(expected), "{kind:?}");
        }
        assert_eq!(
            FsError::TooDeep {
                path: dir("/p"),
                max: 1
            }
            .hint()
            .as_deref(),
            Some("use a shallower directory tree")
        );
        assert_eq!(
            FsError::BadName {
                path: dir("/p"),
                entry: "x".to_owned()
            }
            .hint()
            .as_deref(),
            Some("rename the file to a UTF-8 name")
        );
    }

    #[test]
    fn a_cause_keeps_the_kind_and_the_raw_os_error() {
        let os = io::Error::from_raw_os_error(2);
        assert_eq!(
            Cause::of(&os),
            Cause {
                kind: io::ErrorKind::NotFound,
                errno: Some(2)
            }
        );
        let plain = io::Error::from(io::ErrorKind::TimedOut);
        assert_eq!(
            Cause::of(&plain),
            Cause {
                kind: io::ErrorKind::TimedOut,
                errno: None
            }
        );
    }

    #[test]
    fn every_operation_has_a_distinct_verb() {
        let ops = [
            FsOp::Create,
            FsOp::Open,
            FsOp::Write,
            FsOp::Sync,
            FsOp::Rename,
            FsOp::Exchange,
            FsOp::Remove,
            FsOp::Read,
            FsOp::List,
            FsOp::Stat,
            FsOp::Chmod,
        ];
        let verbs: std::collections::BTreeSet<&str> = ops.iter().map(|op| op.as_str()).collect();
        assert_eq!(verbs.len(), ops.len());
        assert_eq!(FsOp::Sync.to_string(), "sync");
    }
}
