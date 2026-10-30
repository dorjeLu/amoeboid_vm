//! `GuestHome`: the host's home directory, usable as the guest's (same path, so paths paste across).

use crate::diagnostic::Diagnostic;
use crate::domain::abs_path::{AbsDir, HomeDir};
use crate::domain::components::first_control;
use crate::domain::observed::{ExistingDir, ExistingDirError, Observation};
use crate::refined::{Refined, Refinement};

/// Guest paths the home can't be, or be under: the system needs them, or amoeboid mounts there.
pub const RESERVED_HOMES: [&str; 15] = [
    "/nix",
    "/mnt/amoeboid",
    "/etc",
    "/proc",
    "/sys",
    "/dev",
    "/run",
    "/boot",
    "/root",
    "/tmp",
    "/usr",
    "/bin",
    "/sbin",
    "/lib",
    "/lib64",
];

/// Marker of [`GuestHome`].
pub struct GuestHomeRules;

/// A [`HomeDir`] that is also fit to be the guest user's home.
pub type GuestHome = Refined<HomeDir, GuestHomeRules>;

#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum GuestHomeError {
    #[error("bad guest home: it is '/'")]
    Root,
    /// `under` is the entry of [`RESERVED_HOMES`] that equals or contains the home.
    #[error("bad guest home: the guest system uses {under}")]
    Reserved { under: &'static str },
    /// `at` is a char index.
    #[error("bad guest home: control character at position {at}")]
    ControlCharacter { at: usize },
}

impl Diagnostic for GuestHomeError {
    fn code(&self) -> &'static str {
        match self {
            Self::Root => "guest-home.root",
            Self::Reserved { .. } => "guest-home.reserved",
            Self::ControlCharacter { .. } => "guest-home.control-character",
        }
    }

    fn message(&self) -> String {
        self.to_string()
    }

    fn hint(&self) -> Option<String> {
        Some("run amoeboid with $HOME set to your home directory".to_owned())
    }
}

/// Why `$HOME` can't be the guest's home.
#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum GuestHomeObserveError {
    /// `$HOME` is not a directory that exists.
    #[error(transparent)]
    Dir(ExistingDirError),
    /// The resolved home breaks the guest-home rules.
    #[error(transparent)]
    Rules(GuestHomeError),
}

impl Diagnostic for GuestHomeObserveError {
    fn code(&self) -> &'static str {
        match self {
            Self::Dir(_) => "guest-home.dir",
            Self::Rules(_) => "guest-home.rules",
        }
    }

    fn message(&self) -> String {
        self.to_string()
    }

    fn hint(&self) -> Option<String> {
        match self {
            Self::Dir(inner) => inner.hint(),
            Self::Rules(inner) => inner.hint(),
        }
    }
}

impl GuestHome {
    /// The home the guest gets: `requested` (`$HOME`) observed through `probe` with symlinks
    /// resolved, then checked against the guest-home rules. The resolved path is the one home of
    /// the run: the guest's `HOME` and the `protected` path of `Shares::validate` agree.
    ///
    /// # Errors
    /// `$HOME` is not an existing directory, or the resolved path breaks the rules.
    pub fn observe(
        requested: &HomeDir,
        probe: impl FnOnce(&AbsDir) -> Observation,
    ) -> Result<Self, GuestHomeObserveError> {
        let resolved =
            ExistingDir::observe(&requested.to_dir(), probe).map_err(GuestHomeObserveError::Dir)?;
        Self::parse(resolved.path().clone().into_home()).map_err(GuestHomeObserveError::Rules)
    }
}

/// `path` is `reserved` or below it; both are normalised, so a `/` boundary is enough.
fn covers(reserved: &str, path: &str) -> bool {
    path.strip_prefix(reserved)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

impl Refinement<HomeDir> for GuestHomeRules {
    type Error = GuestHomeError;

    fn check(value: &HomeDir) -> Result<(), GuestHomeError> {
        let home = value.get().as_str();
        if home == "/" {
            return Err(GuestHomeError::Root);
        }
        if let Some(under) = RESERVED_HOMES
            .iter()
            .copied()
            .find(|reserved| covers(reserved, home))
        {
            return Err(GuestHomeError::Reserved { under });
        }
        match first_control(home) {
            Some((at, _)) => Err(GuestHomeError::ControlCharacter { at }),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) mod tests {
    use serde_json::Value;

    use super::*;
    use crate::domain::abs_path::AbsPathError;
    use crate::testing::{self, Entry};

    fn witness(e: &GuestHomeError) -> &'static str {
        match e {
            GuestHomeError::Root => "Root",
            GuestHomeError::Reserved { .. } => "Reserved",
            GuestHomeError::ControlCharacter { .. } => "ControlCharacter",
        }
    }

    fn errors() -> Vec<GuestHomeError> {
        vec![
            GuestHomeError::Root,
            GuestHomeError::Reserved { under: "/nix" },
            GuestHomeError::ControlCharacter { at: 7 },
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
            &["Root", "Reserved", "ControlCharacter"],
        );
    }

    fn observe_errors() -> Vec<GuestHomeObserveError> {
        vec![
            GuestHomeObserveError::Dir(ExistingDirError::Missing {
                path: AbsDir::parse("/home/u".to_owned()).unwrap(),
            }),
            GuestHomeObserveError::Rules(GuestHomeError::Root),
        ]
    }

    /// One catalogue entry per observation error variant.
    pub(crate) fn observe_entries() -> Vec<Entry> {
        observe_errors().iter().map(Entry::of).collect()
    }

    #[test]
    fn every_observe_error_variant_has_a_table_case() {
        fn witness(e: &GuestHomeObserveError) -> &'static str {
            match e {
                GuestHomeObserveError::Dir(_) => "Dir",
                GuestHomeObserveError::Rules(_) => "Rules",
            }
        }
        testing::every_variant(&observe_errors(), witness, &["Dir", "Rules"]);
    }

    #[test]
    fn the_guest_home_is_the_resolved_home_when_it_obeys_the_rules() {
        let abs = |text: &str| AbsDir::parse(text.to_owned()).unwrap();
        let seen = |observed: Observation| {
            GuestHome::observe(
                &HomeDir::parse("/home/u".to_owned()).unwrap(),
                |requested| {
                    assert_eq!(requested, &abs("/home/u"));
                    observed.clone()
                },
            )
            .map(|home| home.get().get().clone())
        };
        testing::table(
            &[
                (
                    // /home -> /var/home
                    Observation::Directory {
                        resolved: abs("/var/home/u"),
                    },
                    Ok("/var/home/u".to_owned()),
                ),
                (
                    Observation::Directory {
                        resolved: abs("/home/u"),
                    },
                    Ok("/home/u".to_owned()),
                ),
                (
                    Observation::Directory {
                        resolved: abs("/usr/home/u"),
                    },
                    Err(GuestHomeObserveError::Rules(GuestHomeError::Reserved {
                        under: "/usr",
                    })),
                ),
                (
                    Observation::Directory { resolved: abs("/") },
                    Err(GuestHomeObserveError::Rules(GuestHomeError::Root)),
                ),
                (
                    Observation::Missing,
                    Err(GuestHomeObserveError::Dir(ExistingDirError::Missing {
                        path: abs("/home/u"),
                    })),
                ),
                (
                    Observation::Other,
                    Err(GuestHomeObserveError::Dir(
                        ExistingDirError::NotADirectory {
                            path: abs("/home/u"),
                        },
                    )),
                ),
            ],
            |observed| seen(observed.clone()),
        );
    }

    #[test]
    fn an_observe_error_keeps_the_message_and_hint_of_its_cause() {
        for error in observe_errors() {
            let (message, hint) = match &error {
                GuestHomeObserveError::Dir(inner) => (inner.to_string(), inner.hint()),
                GuestHomeObserveError::Rules(inner) => (inner.to_string(), inner.hint()),
            };
            assert_eq!(error.message(), message);
            assert_eq!(error.hint(), hint);
        }
    }

    fn guest_home(text: &str) -> Result<(), GuestHomeError> {
        let home = HomeDir::parse(text.to_owned()).unwrap();
        GuestHome::parse(home).map(|_| ())
    }

    #[test]
    fn homes_outside_the_reserved_paths_are_accepted() {
        testing::table(
            &[
                ("/home/alice", Ok(())),
                ("/home/a b", Ok(())),
                ("/home/日本", Ok(())),
                ("/home/~x", Ok(())),
                ("/mnt", Ok(())),
                ("/mnt/other", Ok(())),
                ("/homes", Ok(())),
                ("/opt/u", Ok(())),
                ("/var/lib/users/alice", Ok(())),
                ("/", Err(GuestHomeError::Root)),
                (
                    "/home/a\tb",
                    Err(GuestHomeError::ControlCharacter { at: 7 }),
                ),
                (
                    "/home/a\nb",
                    Err(GuestHomeError::ControlCharacter { at: 7 }),
                ),
                (
                    "/日本/\u{1f}",
                    Err(GuestHomeError::ControlCharacter { at: 4 }),
                ),
                (
                    "/home/a\u{7f}b",
                    Err(GuestHomeError::ControlCharacter { at: 7 }),
                ),
                ("/home/\u{80}", Ok(())),
            ],
            |text| guest_home(text),
        );
    }

    #[test]
    fn every_reserved_path_and_everything_below_it_is_refused_but_not_its_prefix_siblings() {
        for reserved in RESERVED_HOMES {
            let below = format!("{reserved}/home");
            let sibling = format!("{reserved}x");
            let expected = Err(GuestHomeError::Reserved { under: reserved });
            assert_eq!(guest_home(reserved), expected, "{reserved}");
            assert_eq!(guest_home(&below), expected, "{below}");
            assert_eq!(guest_home(&sibling), Ok(()), "{sibling}");
        }
    }

    #[test]
    fn a_home_inside_a_reserved_path_names_the_reserved_path() {
        testing::table(
            &[
                ("/mnt/amoeboid/home", "/mnt/amoeboid"),
                ("/lib64/x", "/lib64"),
                ("/lib/x", "/lib"),
                ("/proc/1", "/proc"),
                ("/run/user/1000", "/run"),
            ],
            |text| match guest_home(text) {
                Err(GuestHomeError::Reserved { under }) => under,
                Err(GuestHomeError::Root | GuestHomeError::ControlCharacter { .. }) | Ok(()) => "",
            },
        );
    }

    fn field<'a>(object: &'a Value, key: &str) -> &'a Value {
        object.get(key).unwrap_or(&Value::Null)
    }

    #[test]
    fn reserved_paths_are_those_of_the_python_reference() {
        let constants: Value =
            serde_json::from_str(include_str!("../../tests/golden/constants.json")).unwrap();
        let python: Vec<&str> = field(&constants, "reserved_homes")
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(python, RESERVED_HOMES);
    }

    #[test]
    fn homes_decide_like_the_python_reference_corpus() {
        let corpus: Value =
            serde_json::from_str(include_str!("../../tests/golden/guest_home.json")).unwrap();
        for case in corpus.as_array().unwrap() {
            let name = field(case, "name").as_str().unwrap();
            let home = field(field(case, "input"), "home").as_str().unwrap();
            let kind = field(field(case, "output"), "kind").as_str();
            // Python has one step; Rust splits it between `HomeDir` (host path shape) and here.
            let got = match HomeDir::parse(home.to_owned()) {
                Err(AbsPathError::Empty | AbsPathError::NotAbsolute) => {
                    Some("not_absolute_or_root")
                }
                Err(AbsPathError::NulByte { .. }) => Some("control_characters"),
                Err(
                    AbsPathError::EmptyComponent { .. }
                    | AbsPathError::CurrentDir { .. }
                    | AbsPathError::ParentDir { .. },
                ) => Some("unexpected"),
                Ok(_) => match guest_home(home) {
                    Ok(()) => None,
                    Err(GuestHomeError::Root) => Some("not_absolute_or_root"),
                    Err(GuestHomeError::Reserved { .. }) => Some("reserved"),
                    Err(GuestHomeError::ControlCharacter { .. }) => Some("control_characters"),
                },
            };
            assert_eq!(got, kind, "{name}: {home:?}");
        }
    }
}
