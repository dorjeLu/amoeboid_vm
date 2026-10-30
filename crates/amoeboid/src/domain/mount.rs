//! `HostDir`, `MountRequest`, `Mount`: a host directory the operator asked to share, and the same
//! once it has been observed to be a directory.

use crate::diagnostic::Diagnostic;
use crate::domain::abs_path::{AbsDir, HomeDir};
use crate::domain::mode::Mode;
use crate::domain::mount_spec::MountSpec;
use crate::domain::observed::Observation;
use crate::domain::spec_path::ResolveError;

/// A directory that was a directory when observed, with symlinks resolved. The proof is that
/// the observation said so: there is no way in from a bare path. The state may have changed
/// since; effects that rely on it still handle failure.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct HostDir(AbsDir);

#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum HostDirError {
    #[error("{path}: not a directory")]
    NotADirectory { path: AbsDir },
}

impl Diagnostic for HostDirError {
    fn code(&self) -> &'static str {
        match self {
            Self::NotADirectory { .. } => "host-dir.not-a-directory",
        }
    }

    fn message(&self) -> String {
        self.to_string()
    }

    fn hint(&self) -> Option<String> {
        match self {
            Self::NotADirectory { .. } => {
                Some("create the directory, or give the path of an existing one".to_owned())
            }
        }
    }
}

impl HostDir {
    /// Observe `requested` through `probe`.
    ///
    /// # Errors
    ///
    /// The probe found something other than a directory (or nothing).
    pub fn observe(
        requested: &AbsDir,
        probe: impl FnOnce(&AbsDir) -> Observation,
    ) -> Result<Self, HostDirError> {
        match probe(requested) {
            Observation::Directory { resolved } => Ok(Self(resolved)),
            Observation::Missing | Observation::File { .. } | Observation::Other => {
                Err(HostDirError::NotADirectory {
                    path: requested.clone(),
                })
            }
        }
    }

    /// The resolved path.
    #[must_use]
    pub fn path(&self) -> &AbsDir {
        &self.0
    }
}

/// A share as asked for: absolute and normalised, not yet observed.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct MountRequest {
    pub path: AbsDir,
    pub mode: Mode,
}

impl MountRequest {
    /// `cwd` anchors relative paths; `home` is what `~` means.
    ///
    /// # Errors
    ///
    /// The path names another account's home (`~name`).
    pub fn resolve(spec: &MountSpec, cwd: &AbsDir, home: &HomeDir) -> Result<Self, ResolveError> {
        Ok(Self {
            path: spec.path.resolve(cwd, home)?,
            mode: spec.mode,
        })
    }
}

/// An observed directory and the access policy it is shared with.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Mount {
    pub dir: HostDir,
    pub mode: Mode,
}

impl Mount {
    /// `PATH:MODE`, the form `MountSpec::parse` reads back.
    #[must_use]
    pub fn spec(&self) -> String {
        format!("{}:{}", self.dir.path(), self.mode)
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) mod tests {
    use super::*;
    use crate::domain::spec_path::ResolveError;
    use crate::testing::{self, Entry};

    pub(crate) fn abs(text: &str) -> AbsDir {
        AbsDir::parse(text.to_owned()).unwrap()
    }

    fn home(text: &str) -> HomeDir {
        HomeDir::parse(text.to_owned()).unwrap()
    }

    pub(crate) fn dir(path: &str) -> HostDir {
        HostDir::observe(&abs(path), |_| Observation::Directory {
            resolved: abs(path),
        })
        .unwrap()
    }

    fn witness(e: &HostDirError) -> &'static str {
        match e {
            HostDirError::NotADirectory { .. } => "NotADirectory",
        }
    }

    fn errors() -> Vec<HostDirError> {
        vec![HostDirError::NotADirectory {
            path: abs("/srv/x"),
        }]
    }

    /// One catalogue entry per variant.
    pub(crate) fn entries() -> Vec<Entry> {
        errors().iter().map(Entry::of).collect()
    }

    #[test]
    fn every_error_variant_has_a_table_case() {
        testing::every_variant(&errors(), witness, &["NotADirectory"]);
    }

    #[test]
    fn a_host_dir_is_the_resolved_path_of_a_directory_observation() {
        testing::table(
            &[
                (
                    (
                        "/srv/link",
                        Observation::Directory {
                            resolved: abs("/srv/real"),
                        },
                    ),
                    Ok(abs("/srv/real")),
                ),
                (
                    (
                        "/srv/a",
                        Observation::Directory {
                            resolved: abs("/srv/a"),
                        },
                    ),
                    Ok(abs("/srv/a")),
                ),
                (
                    ("/srv/file", Observation::Missing),
                    Err(HostDirError::NotADirectory {
                        path: abs("/srv/file"),
                    }),
                ),
            ],
            |(requested, observed)| {
                let asked = std::cell::RefCell::new(None);
                let result = HostDir::observe(&abs(requested), |path| {
                    *asked.borrow_mut() = Some(path.clone());
                    observed.clone()
                });
                assert_eq!(
                    asked.into_inner(),
                    Some(abs(requested)),
                    "the probe sees the requested path"
                );
                result.map(|d| d.path().clone())
            },
        );
    }

    #[test]
    fn requests_resolve_the_spec_path_and_keep_the_mode() {
        let parse = |text: &str| MountSpec::parse(text).unwrap();
        testing::table(
            &[
                (
                    "~/dev:ro",
                    Ok(MountRequest {
                        path: abs("/home/u/dev"),
                        mode: Mode::Ro,
                    }),
                ),
                (
                    "proj/../lib",
                    Ok(MountRequest {
                        path: abs("/work/lib"),
                        mode: Mode::Rw,
                    }),
                ),
                (
                    "/srv/a:rw",
                    Ok(MountRequest {
                        path: abs("/srv/a"),
                        mode: Mode::Rw,
                    }),
                ),
                (
                    "~root/x",
                    Err(ResolveError::OtherUser {
                        name: "root".to_owned(),
                    }),
                ),
            ],
            |text| MountRequest::resolve(&parse(text), &abs("/work"), &home("/home/u")),
        );
    }

    #[test]
    fn a_mount_spells_itself_as_path_and_mode() {
        testing::table(
            &[
                ((dir("/srv/a"), Mode::Rw), "/srv/a:rw".to_owned()),
                ((dir("/srv/b/c"), Mode::Ro), "/srv/b/c:ro".to_owned()),
                ((dir("/"), Mode::Ro), "/:ro".to_owned()),
            ],
            |(dir, mode)| {
                Mount {
                    dir: dir.clone(),
                    mode: *mode,
                }
                .spec()
            },
        );
    }

    #[test]
    fn a_spelled_mount_parses_back_to_the_same_path_and_mode() {
        for (path, mode) in [("/srv/a", Mode::Rw), ("/odd:name", Mode::Ro)] {
            let mount = Mount {
                dir: dir(path),
                mode,
            };
            let spec = MountSpec::parse(&mount.spec()).unwrap();
            assert_eq!(spec.mode, mode);
            assert_eq!(spec.path.get(), path);
        }
    }
}
