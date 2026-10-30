//! Which NixOS configuration a VM is made from (design §Configs are directories): the decisions
//! over what was observed. Pure; `super` looks.
//!
//! - A new VM copies its base: `--base-nix-config`, else the config's `base-nix-config` (a preset
//!   unless it is a path; the config always has one, `default` unless changed).
//! - An existing VM keeps its own copy unless `--base-nix-config` is given, which replaces it.
//! - Nothing is created from inside the binary: a missing default preset is an error that says
//!   where to copy the reference config.

use crate::config::DEFAULT_PRESET;
use crate::diagnostic::Diagnostic;
use crate::domain::{AbsDir, BaseSpec, NixSource, NixSourceError, Observation, PresetName};

/// A directory to look at for a base, and the preset name it came from, if any.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Target {
    pub dir: AbsDir,
    pub preset: Option<PresetName>,
}

/// Where `spec` is: a preset is a directory of the presets directory, a path is itself.
#[must_use]
pub fn target(presets: &AbsDir, spec: &BaseSpec) -> Target {
    match spec {
        BaseSpec::Preset(name) => Target {
            dir: presets.join_dir(name),
            preset: Some(name.clone()),
        },
        BaseSpec::Dir(dir) => Target {
            dir: dir.clone(),
            preset: None,
        },
    }
}

/// Whether the VM already has a config copy of its own (a directory, symlinks followed).
#[must_use]
pub const fn own_exists(own: &Observation) -> bool {
    matches!(own, Observation::Directory { .. })
}

/// What has to be looked at to make the VM's config copy.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Need<'a> {
    /// The VM has its copy and nobody asked for another.
    Keep,
    Create(&'a BaseSpec),
    Rebase(&'a BaseSpec),
}

/// An explicit base always wins (and replaces an existing copy); otherwise only a VM without a
/// copy needs the configured one.
#[must_use]
pub const fn need<'a>(
    own_exists: bool,
    explicit: Option<&'a BaseSpec>,
    configured: &'a BaseSpec,
) -> Need<'a> {
    match (explicit, own_exists) {
        (Some(spec), true) => Need::Rebase(spec),
        (Some(spec), false) => Need::Create(spec),
        (None, false) => Need::Create(configured),
        (None, true) => Need::Keep,
    }
}

/// How the VM's config copy changes in this start.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Change {
    Keep,
    /// A new copy of `source`.
    Create(NixSource),
    /// The existing copy replaced by one of `source`.
    Rebase(NixSource),
}

/// What happened to the copy.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Origin {
    Kept,
    Created,
    Rebased,
}

impl Change {
    #[must_use]
    pub const fn origin(&self) -> Origin {
        match self {
            Self::Keep => Origin::Kept,
            Self::Create(_) => Origin::Created,
            Self::Rebase(_) => Origin::Rebased,
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum BaseError {
    #[error(transparent)]
    Source(NixSourceError),
    /// A preset other than `default` that is not in the presets directory.
    #[error("no preset named {name}: {dir} doesn't exist")]
    PresetMissing { name: PresetName, dir: AbsDir },
    #[error("the default preset doesn't exist: {dir}")]
    DefaultPresetMissing { dir: AbsDir },
}

impl Diagnostic for BaseError {
    fn code(&self) -> &'static str {
        match self {
            Self::Source(_) => "base.source",
            Self::PresetMissing { .. } => "base.preset-missing",
            Self::DefaultPresetMissing { .. } => "base.default-preset-missing",
        }
    }

    fn message(&self) -> String {
        self.to_string()
    }

    fn hint(&self) -> Option<String> {
        Some(match self {
            Self::Source(inner) => return inner.hint(),
            Self::PresetMissing { .. } => {
                "list the presets with `amoeboid presets`, or give a directory with \
                 --base-nix-config"
                    .to_owned()
            }
            Self::DefaultPresetMissing { dir } => format!(
                "copy the reference NixOS config (the nix/ directory of the amoeboid sources) \
                 to {dir}, or give a directory with --base-nix-config"
            ),
        })
    }
}

/// The base `target` names, from what the probe saw at its directory and at its `flake.nix`.
///
/// # Errors
/// The directory (or preset) is missing, not a directory, or has no `flake.nix`.
pub fn source(
    target: &Target,
    dir: Observation,
    flake: &Observation,
) -> Result<NixSource, BaseError> {
    if let (Observation::Missing, Some(name)) = (&dir, &target.preset) {
        let dir = target.dir.clone();
        return Err(if name.get() == DEFAULT_PRESET {
            BaseError::DefaultPresetMissing { dir }
        } else {
            BaseError::PresetMissing {
                name: name.clone(),
                dir,
            }
        });
    }
    NixSource::observe(&target.dir, dir, flake).map_err(BaseError::Source)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) mod tests {
    use super::*;
    use crate::domain::{AbsPath, ExistingDirError, HomeDir};
    use crate::testing::{self, Entry};

    fn dir(text: &str) -> AbsDir {
        AbsDir::parse(text.to_owned()).unwrap()
    }

    fn preset(text: &str) -> PresetName {
        PresetName::parse(text.to_owned()).unwrap()
    }

    fn directory(text: &str) -> Observation {
        Observation::Directory {
            resolved: dir(text),
        }
    }

    fn file() -> Observation {
        Observation::File {
            resolved: AbsPath::parse("/real/flake.nix".to_owned()).unwrap(),
            executable: false,
        }
    }

    fn witness(e: &BaseError) -> &'static str {
        match e {
            BaseError::Source(_) => "Source",
            BaseError::PresetMissing { .. } => "PresetMissing",
            BaseError::DefaultPresetMissing { .. } => "DefaultPresetMissing",
        }
    }

    fn errors() -> Vec<BaseError> {
        vec![
            BaseError::Source(NixSourceError::NoFlake { dir: dir("/p") }),
            BaseError::PresetMissing {
                name: preset("work"),
                dir: dir("/c/amoeboid/nix_presets/work"),
            },
            BaseError::DefaultPresetMissing {
                dir: dir("/c/amoeboid/nix_presets/default"),
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
            &["Source", "PresetMissing", "DefaultPresetMissing"],
        );
    }

    #[test]
    fn messages_and_hints_tell_where_the_config_should_come_from() {
        let all = errors();
        let messages: Vec<String> = all.iter().map(Diagnostic::message).collect();
        assert_eq!(
            messages,
            [
                "/p: not a NixOS config dir (no flake.nix)",
                "no preset named work: /c/amoeboid/nix_presets/work doesn't exist",
                "the default preset doesn't exist: /c/amoeboid/nix_presets/default",
            ]
        );
        let hints: Vec<Option<String>> = all.iter().map(Diagnostic::hint).collect();
        assert_eq!(
            hints,
            [
                Some("give a directory with a flake.nix, or a preset (see `amoeboid presets`)".to_owned()),
                Some("list the presets with `amoeboid presets`, or give a directory with --base-nix-config".to_owned()),
                Some("copy the reference NixOS config (the nix/ directory of the amoeboid sources) to /c/amoeboid/nix_presets/default, or give a directory with --base-nix-config".to_owned()),
            ]
        );
    }

    #[test]
    fn a_preset_is_a_directory_of_the_presets_dir_and_a_path_is_itself() {
        let presets = dir("/c/amoeboid/nix_presets");
        let home = HomeDir::parse("/h".to_owned()).unwrap();
        let cwd = dir("/w");
        let spec = |text: &str| BaseSpec::parse(text, &cwd, &home).unwrap();
        testing::table(
            &[
                (
                    "default",
                    Target {
                        dir: dir("/c/amoeboid/nix_presets/default"),
                        preset: Some(preset("default")),
                    },
                ),
                (
                    "my-1.2",
                    Target {
                        dir: dir("/c/amoeboid/nix_presets/my-1.2"),
                        preset: Some(preset("my-1.2")),
                    },
                ),
                (
                    "~/cfg",
                    Target {
                        dir: dir("/h/cfg"),
                        preset: None,
                    },
                ),
                (
                    "./rel",
                    Target {
                        dir: dir("/w/rel"),
                        preset: None,
                    },
                ),
            ],
            |text| target(&presets, &spec(text)),
        );
    }

    #[test]
    fn a_bad_preset_name_is_refused_before_anything_is_looked_at() {
        let home = HomeDir::parse("/h".to_owned()).unwrap();
        let cwd = dir("/w");
        for text in ["", "-x", "a b", "a\n"] {
            let error = BaseSpec::parse(text, &cwd, &home).unwrap_err();
            assert_eq!(error.code(), "base-spec.preset", "{text:?}");
        }
    }

    #[test]
    fn only_a_directory_is_a_config_copy() {
        testing::table(
            &[
                (directory("/d/nix"), true),
                (Observation::Missing, false),
                (file(), false),
                (Observation::Other, false),
            ],
            own_exists,
        );
    }

    #[test]
    fn an_explicit_base_wins_and_only_a_vm_without_a_copy_needs_the_configured_one() {
        let explicit = BaseSpec::Dir(dir("/x"));
        let configured = BaseSpec::Preset(preset("default"));
        testing::table(
            &[
                ((true, Some(&explicit)), Need::Rebase(&explicit)),
                ((false, Some(&explicit)), Need::Create(&explicit)),
                ((false, None), Need::Create(&configured)),
                ((true, None), Need::Keep),
            ],
            |&(exists, given)| need(exists, given, &configured),
        );
    }

    #[test]
    fn a_change_says_what_became_of_the_copy() {
        let source = NixSource::observe(&dir("/s"), directory("/s"), &file()).unwrap();
        testing::table(
            &[
                (Change::Keep, Origin::Kept),
                (Change::Create(source.clone()), Origin::Created),
                (Change::Rebase(source), Origin::Rebased),
            ],
            Change::origin,
        );
    }

    #[test]
    fn a_base_is_a_directory_with_a_flake_and_a_missing_preset_says_so() {
        let default = Target {
            dir: dir("/p/default"),
            preset: Some(preset("default")),
        };
        let work = Target {
            dir: dir("/p/work"),
            preset: Some(preset("work")),
        };
        let path = Target {
            dir: dir("/x/cfg"),
            preset: None,
        };
        let ok = |dir: &str| Ok(dir.to_owned());
        testing::table(
            &[
                // missing: the default preset, another preset, a path
                (
                    (&default, Observation::Missing, &file()),
                    Err(BaseError::DefaultPresetMissing {
                        dir: dir("/p/default"),
                    }),
                ),
                (
                    (&work, Observation::Missing, &Observation::Missing),
                    Err(BaseError::PresetMissing {
                        name: preset("work"),
                        dir: dir("/p/work"),
                    }),
                ),
                (
                    (&path, Observation::Missing, &Observation::Missing),
                    Err(BaseError::Source(NixSourceError::Dir(
                        ExistingDirError::Missing {
                            path: dir("/x/cfg"),
                        },
                    ))),
                ),
                // not a directory, no flake
                (
                    (&default, file(), &file()),
                    Err(BaseError::Source(NixSourceError::Dir(
                        ExistingDirError::NotADirectory {
                            path: dir("/p/default"),
                        },
                    ))),
                ),
                (
                    (&work, directory("/real/work"), &Observation::Missing),
                    Err(BaseError::Source(NixSourceError::NoFlake {
                        dir: dir("/real/work"),
                    })),
                ),
                // usable, symlinks resolved
                (
                    (&default, directory("/real/default"), &file()),
                    ok("/real/default"),
                ),
                ((&path, directory("/real/cfg"), &file()), ok("/real/cfg")),
            ],
            |(target, seen_dir, seen_flake)| {
                source(target, seen_dir.clone(), seen_flake).map(|s| s.path().get().clone())
            },
        );
    }
}
