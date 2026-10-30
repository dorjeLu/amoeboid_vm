//! `NixSource`: a directory a VM's NixOS configuration can be copied from, observed to hold a
//! `flake.nix` (design §Configs are directories). Nothing is embedded in the binary: presets and
//! `--base-nix-config` paths are always directories.

use crate::diagnostic::Diagnostic;
use crate::domain::file_name::FileName;
use crate::domain::observed::{ExistingDir, ExistingDirError, Observation};
use crate::domain::{AbsDir, AbsPath};
use crate::refined;

/// The file that makes a directory a NixOS configuration.
pub const FLAKE_NIX: FileName = refined!(FileName, "flake.nix");

/// A directory observed to exist, with `flake.nix` in it a regular file; symlinks resolved.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct NixSource(ExistingDir);

#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum NixSourceError {
    #[error(transparent)]
    Dir(ExistingDirError),
    /// `dir` is the resolved directory.
    #[error("{dir}: not a NixOS config dir (no flake.nix)")]
    NoFlake { dir: AbsDir },
}

impl Diagnostic for NixSourceError {
    fn code(&self) -> &'static str {
        match self {
            Self::Dir(_) => "nix-source.dir",
            Self::NoFlake { .. } => "nix-source.no-flake",
        }
    }

    fn message(&self) -> String {
        self.to_string()
    }

    fn hint(&self) -> Option<String> {
        match self {
            Self::Dir(inner) => inner.hint(),
            Self::NoFlake { .. } => Some(
                "give a directory with a flake.nix, or a preset (see `amoeboid presets`)"
                    .to_owned(),
            ),
        }
    }
}

impl NixSource {
    /// The path whose observation tells whether `requested` holds a flake.
    #[must_use]
    pub fn flake_of(requested: &AbsDir) -> AbsPath {
        requested.join(&FLAKE_NIX)
    }

    /// `dir` is what the probe saw at `requested`, `flake` what it saw at
    /// [`NixSource::flake_of`]`(requested)`.
    ///
    /// # Errors
    /// `requested` is not a directory, or its `flake.nix` is not a regular file.
    pub fn observe(
        requested: &AbsDir,
        dir: Observation,
        flake: &Observation,
    ) -> Result<Self, NixSourceError> {
        let dir = ExistingDir::observe(requested, |_| dir).map_err(NixSourceError::Dir)?;
        match flake {
            Observation::File { .. } => Ok(Self(dir)),
            Observation::Missing | Observation::Directory { .. } | Observation::Other => {
                Err(NixSourceError::NoFlake {
                    dir: dir.path().clone(),
                })
            }
        }
    }

    /// The resolved directory.
    #[must_use]
    pub const fn path(&self) -> &AbsDir {
        self.0.path()
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

    fn directory(text: &str) -> Observation {
        Observation::Directory {
            resolved: dir(text),
        }
    }

    fn file() -> Observation {
        Observation::File {
            resolved: AbsPath::parse("/store/x/flake.nix".to_owned()).unwrap(),
            executable: false,
        }
    }

    fn witness(e: &NixSourceError) -> &'static str {
        match e {
            NixSourceError::Dir(_) => "Dir",
            NixSourceError::NoFlake { .. } => "NoFlake",
        }
    }

    fn errors() -> Vec<NixSourceError> {
        vec![
            NixSourceError::Dir(ExistingDirError::Missing { path: dir("/p") }),
            NixSourceError::NoFlake { dir: dir("/p") },
        ]
    }

    /// One catalogue entry per variant.
    pub(crate) fn entries() -> Vec<Entry> {
        errors().iter().map(Entry::of).collect()
    }

    #[test]
    fn every_error_variant_has_a_table_case() {
        testing::every_variant(&errors(), witness, &["Dir", "NoFlake"]);
    }

    #[test]
    fn the_flake_is_looked_for_in_the_requested_directory() {
        assert_eq!(
            NixSource::flake_of(&dir("/c/nix_presets/default")).get(),
            "/c/nix_presets/default/flake.nix"
        );
    }

    #[test]
    fn a_directory_with_a_flake_is_a_source_at_its_resolved_path() {
        let source = NixSource::observe(&dir("/link"), directory("/real"), &file()).unwrap();
        assert_eq!(source.path(), &dir("/real"));
    }

    #[test]
    fn a_source_needs_a_directory_with_a_regular_flake_nix() {
        let no_flake = || Err(NixSourceError::NoFlake { dir: dir("/real") });
        testing::table(
            &[
                (
                    (Observation::Missing, file()),
                    Err(NixSourceError::Dir(ExistingDirError::Missing {
                        path: dir("/req"),
                    })),
                ),
                (
                    (file(), file()),
                    Err(NixSourceError::Dir(ExistingDirError::NotADirectory {
                        path: dir("/req"),
                    })),
                ),
                ((directory("/real"), Observation::Missing), no_flake()),
                (
                    (directory("/real"), directory("/real/flake.nix")),
                    no_flake(),
                ),
                ((directory("/real"), Observation::Other), no_flake()),
                ((directory("/real"), file()), Ok(dir("/real"))),
            ],
            |(seen_dir, seen_flake)| {
                NixSource::observe(&dir("/req"), seen_dir.clone(), seen_flake)
                    .map(|source| source.path().clone())
            },
        );
    }
}
