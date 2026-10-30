//! Observed inputs (design §Observed inputs): [`Observation`], what a probe saw at a path, and
//! the types that can only be made from one: [`ExistingDir`], [`ExistingFile`], [`Executable`].
//! (`HostDir` and `GuestHome` are the same pattern, next to the rules specific to them.)
//!
//! The probe is an adapter (canonicalise plus a kind check, `HostFs::observe`); each type holds
//! the *resolved* path, so no plan is built from an unchecked or unresolved path. The state may
//! have changed since; effects that rely on a type still handle failure.

use crate::diagnostic::Diagnostic;
use crate::domain::abs_path::{AbsDir, AbsPath};

/// What an adapter's probe saw at a requested path. The adapter owns the filesystem calls and
/// refines the answer; the domain only decides what it means.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Observation {
    /// Nothing usable is there: no such entry, a dangling symlink, or a path the probe may not
    /// look at.
    Missing,
    /// A directory; `resolved` is the path with symlinks followed.
    Directory { resolved: AbsDir },
    /// A regular file; `executable` is whether the invoking user may execute it.
    File { resolved: AbsPath, executable: bool },
    /// Anything else (socket, pipe, device), or a resolved path that can't be held as an
    /// [`AbsPath`] (not UTF-8).
    Other,
}

/// A directory that was a directory when observed, with symlinks resolved.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct ExistingDir(AbsDir);

#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum ExistingDirError {
    #[error("{path}: no such directory")]
    Missing { path: AbsDir },
    #[error("{path}: not a directory")]
    NotADirectory { path: AbsDir },
}

impl Diagnostic for ExistingDirError {
    fn code(&self) -> &'static str {
        match self {
            Self::Missing { .. } => "existing-dir.missing",
            Self::NotADirectory { .. } => "existing-dir.not-a-directory",
        }
    }

    fn message(&self) -> String {
        self.to_string()
    }

    fn hint(&self) -> Option<String> {
        Some("create the directory, or give the path of an existing one".to_owned())
    }
}

impl ExistingDir {
    /// Observe `requested` through `probe`.
    ///
    /// # Errors
    /// The probe found nothing, or something other than a directory.
    pub fn observe(
        requested: &AbsDir,
        probe: impl FnOnce(&AbsDir) -> Observation,
    ) -> Result<Self, ExistingDirError> {
        match probe(requested) {
            Observation::Directory { resolved } => Ok(Self(resolved)),
            Observation::Missing => Err(ExistingDirError::Missing {
                path: requested.clone(),
            }),
            Observation::File { .. } | Observation::Other => Err(ExistingDirError::NotADirectory {
                path: requested.clone(),
            }),
        }
    }

    /// The resolved path.
    #[must_use]
    pub const fn path(&self) -> &AbsDir {
        &self.0
    }
}

/// A regular file that existed when observed, with symlinks resolved.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct ExistingFile(AbsPath);

#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum ExistingFileError {
    #[error("{path}: no such file")]
    Missing { path: AbsPath },
    #[error("{path}: not a regular file")]
    NotAFile { path: AbsPath },
}

impl Diagnostic for ExistingFileError {
    fn code(&self) -> &'static str {
        match self {
            Self::Missing { .. } => "existing-file.missing",
            Self::NotAFile { .. } => "existing-file.not-a-file",
        }
    }

    fn message(&self) -> String {
        self.to_string()
    }

    fn hint(&self) -> Option<String> {
        Some("create the file, or give the path of an existing one".to_owned())
    }
}

impl ExistingFile {
    /// Observe `requested` through `probe`.
    ///
    /// # Errors
    /// The probe found nothing, or something other than a regular file.
    pub fn observe(
        requested: &AbsPath,
        probe: impl FnOnce(&AbsPath) -> Observation,
    ) -> Result<Self, ExistingFileError> {
        match probe(requested) {
            Observation::File { resolved, .. } => Ok(Self(resolved)),
            Observation::Missing => Err(ExistingFileError::Missing {
                path: requested.clone(),
            }),
            Observation::Directory { .. } | Observation::Other => {
                Err(ExistingFileError::NotAFile {
                    path: requested.clone(),
                })
            }
        }
    }

    /// The resolved path.
    #[must_use]
    pub const fn path(&self) -> &AbsPath {
        &self.0
    }
}

/// A regular file the invoking user could execute when observed, with symlinks resolved.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Executable(AbsPath);

#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum ExecutableError {
    #[error("{path}: no such file")]
    Missing { path: AbsPath },
    #[error("{path}: not a regular file")]
    NotAFile { path: AbsPath },
    #[error("{path}: not executable")]
    NotExecutable { path: AbsPath },
}

impl Diagnostic for ExecutableError {
    fn code(&self) -> &'static str {
        match self {
            Self::Missing { .. } => "executable.missing",
            Self::NotAFile { .. } => "executable.not-a-file",
            Self::NotExecutable { .. } => "executable.not-executable",
        }
    }

    fn message(&self) -> String {
        self.to_string()
    }

    fn hint(&self) -> Option<String> {
        Some(match self {
            Self::Missing { .. } | Self::NotAFile { .. } => {
                "check that the program exists".to_owned()
            }
            Self::NotExecutable { .. } => "make the file executable".to_owned(),
        })
    }
}

impl Executable {
    /// Observe `requested` through `probe`.
    ///
    /// # Errors
    /// The probe found nothing, something other than a regular file, or a file that can't be
    /// executed.
    pub fn observe(
        requested: &AbsPath,
        probe: impl FnOnce(&AbsPath) -> Observation,
    ) -> Result<Self, ExecutableError> {
        match probe(requested) {
            Observation::File {
                resolved,
                executable: true,
            } => Ok(Self(resolved)),
            Observation::File {
                executable: false, ..
            } => Err(ExecutableError::NotExecutable {
                path: requested.clone(),
            }),
            Observation::Missing => Err(ExecutableError::Missing {
                path: requested.clone(),
            }),
            Observation::Directory { .. } | Observation::Other => Err(ExecutableError::NotAFile {
                path: requested.clone(),
            }),
        }
    }

    /// The resolved path.
    #[must_use]
    pub const fn path(&self) -> &AbsPath {
        &self.0
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) mod tests {
    use super::*;
    use crate::testing::{self, Entry};

    fn dir(text: &str) -> AbsDir {
        AbsDir::parse(text.to_owned()).unwrap()
    }

    fn path(text: &str) -> AbsPath {
        AbsPath::parse(text.to_owned()).unwrap()
    }

    fn file(text: &str, executable: bool) -> Observation {
        Observation::File {
            resolved: path(text),
            executable,
        }
    }

    fn directory(text: &str) -> Observation {
        Observation::Directory {
            resolved: dir(text),
        }
    }

    fn dir_witness(e: &ExistingDirError) -> &'static str {
        match e {
            ExistingDirError::Missing { .. } => "Missing",
            ExistingDirError::NotADirectory { .. } => "NotADirectory",
        }
    }

    fn file_witness(e: &ExistingFileError) -> &'static str {
        match e {
            ExistingFileError::Missing { .. } => "Missing",
            ExistingFileError::NotAFile { .. } => "NotAFile",
        }
    }

    fn exec_witness(e: &ExecutableError) -> &'static str {
        match e {
            ExecutableError::Missing { .. } => "Missing",
            ExecutableError::NotAFile { .. } => "NotAFile",
            ExecutableError::NotExecutable { .. } => "NotExecutable",
        }
    }

    fn dir_errors() -> Vec<ExistingDirError> {
        vec![
            ExistingDirError::Missing { path: dir("/a") },
            ExistingDirError::NotADirectory { path: dir("/b") },
        ]
    }

    fn file_errors() -> Vec<ExistingFileError> {
        vec![
            ExistingFileError::Missing { path: path("/a") },
            ExistingFileError::NotAFile { path: path("/b") },
        ]
    }

    fn exec_errors() -> Vec<ExecutableError> {
        vec![
            ExecutableError::Missing { path: path("/a") },
            ExecutableError::NotAFile { path: path("/b") },
            ExecutableError::NotExecutable { path: path("/c") },
        ]
    }

    /// One catalogue entry per variant.
    pub(crate) fn entries() -> Vec<Entry> {
        let mut all: Vec<Entry> = dir_errors().iter().map(Entry::of).collect();
        all.extend(file_errors().iter().map(Entry::of));
        all.extend(exec_errors().iter().map(Entry::of));
        all
    }

    #[test]
    fn every_error_variant_has_a_table_case() {
        testing::every_variant(&dir_errors(), dir_witness, &["Missing", "NotADirectory"]);
        testing::every_variant(&file_errors(), file_witness, &["Missing", "NotAFile"]);
        testing::every_variant(
            &exec_errors(),
            exec_witness,
            &["Missing", "NotAFile", "NotExecutable"],
        );
    }

    #[test]
    fn an_existing_dir_is_the_resolved_path_of_a_directory_observation() {
        testing::table(
            &[
                (directory("/srv/real"), Ok(dir("/srv/real"))),
                (
                    Observation::Missing,
                    Err(ExistingDirError::Missing {
                        path: dir("/srv/link"),
                    }),
                ),
                (
                    file("/srv/f", true),
                    Err(ExistingDirError::NotADirectory {
                        path: dir("/srv/link"),
                    }),
                ),
                (
                    Observation::Other,
                    Err(ExistingDirError::NotADirectory {
                        path: dir("/srv/link"),
                    }),
                ),
            ],
            |observed| {
                let asked = std::cell::RefCell::new(None);
                let result = ExistingDir::observe(&dir("/srv/link"), |requested| {
                    *asked.borrow_mut() = Some(requested.clone());
                    observed.clone()
                });
                assert_eq!(asked.into_inner(), Some(dir("/srv/link")));
                result.map(|d| d.path().clone())
            },
        );
    }

    #[test]
    fn an_existing_file_is_the_resolved_path_of_a_file_observation_executable_or_not() {
        testing::table(
            &[
                (file("/srv/real", false), Ok(path("/srv/real"))),
                (file("/srv/real", true), Ok(path("/srv/real"))),
                (
                    Observation::Missing,
                    Err(ExistingFileError::Missing {
                        path: path("/srv/link"),
                    }),
                ),
                (
                    directory("/srv/d"),
                    Err(ExistingFileError::NotAFile {
                        path: path("/srv/link"),
                    }),
                ),
                (
                    Observation::Other,
                    Err(ExistingFileError::NotAFile {
                        path: path("/srv/link"),
                    }),
                ),
            ],
            |observed| {
                let asked = std::cell::RefCell::new(None);
                let result = ExistingFile::observe(&path("/srv/link"), |requested| {
                    *asked.borrow_mut() = Some(requested.clone());
                    observed.clone()
                });
                assert_eq!(asked.into_inner(), Some(path("/srv/link")));
                result.map(|f| f.path().clone())
            },
        );
    }

    #[test]
    fn an_executable_is_the_resolved_path_of_an_executable_file_observation() {
        testing::table(
            &[
                (
                    file("/nix/store/x/bin/run", true),
                    Ok(path("/nix/store/x/bin/run")),
                ),
                (
                    file("/nix/store/x/bin/run", false),
                    Err(ExecutableError::NotExecutable {
                        path: path("/h/vm/runner"),
                    }),
                ),
                (
                    Observation::Missing,
                    Err(ExecutableError::Missing {
                        path: path("/h/vm/runner"),
                    }),
                ),
                (
                    directory("/nix/store/x"),
                    Err(ExecutableError::NotAFile {
                        path: path("/h/vm/runner"),
                    }),
                ),
                (
                    Observation::Other,
                    Err(ExecutableError::NotAFile {
                        path: path("/h/vm/runner"),
                    }),
                ),
            ],
            |observed| {
                Executable::observe(&path("/h/vm/runner"), |_| observed.clone())
                    .map(|e| e.path().clone())
            },
        );
    }

    #[test]
    fn hints_tell_a_missing_program_from_a_program_that_cannot_run() {
        let hints: Vec<Option<String>> = exec_errors().iter().map(Diagnostic::hint).collect();
        assert_eq!(
            hints,
            [
                Some("check that the program exists".to_owned()),
                Some("check that the program exists".to_owned()),
                Some("make the file executable".to_owned()),
            ]
        );
    }
}
