//! Which of the configured `home-files` can be copied into the VM's home (design §Observed
//! inputs: `ExistingFile`). Pure; `super` looks.

use crate::diagnostic::Diagnostic;
use crate::domain::{AbsPath, ExistingFile, GuestRel, HomeDir, HomeRel, Observation};

/// A file that is not copied; the start goes on.
#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum HomeFileWarning {
    /// Nothing usable at `~/rel`: no such entry, or not a regular file.
    #[error("not copying ~/{rel} into the VM: no such regular file")]
    HomeFileMissing { rel: HomeRel },
}

impl Diagnostic for HomeFileWarning {
    fn code(&self) -> &'static str {
        match self {
            Self::HomeFileMissing { .. } => "home-file.missing",
        }
    }

    fn message(&self) -> String {
        self.to_string()
    }

    fn hint(&self) -> Option<String> {
        Some("create the file, or remove it from `home-files` in the config".to_owned())
    }
}

/// Where `rel` is under `home`.
#[must_use]
pub fn requested(home: &HomeDir, rel: &HomeRel) -> AbsPath {
    home.to_dir().join_rel(&GuestRel::of_home(rel))
}

/// A configured file with what the probe saw at its path.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Present {
    pub rel: HomeRel,
    pub file: ExistingFile,
}

/// The files that exist, in order, and a warning for each that doesn't. `seen` pairs each
/// configured file with the observation of [`requested`]`(home, rel)`.
#[must_use]
pub fn split(
    home: &HomeDir,
    seen: Vec<(HomeRel, Observation)>,
) -> (Vec<Present>, Vec<HomeFileWarning>) {
    let mut present = Vec::new();
    let mut warnings = Vec::new();
    for (rel, observation) in seen {
        match ExistingFile::observe(&requested(home, &rel), |_| observation) {
            Ok(file) => present.push(Present { rel, file }),
            Err(_) => warnings.push(HomeFileWarning::HomeFileMissing { rel }),
        }
    }
    (present, warnings)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) mod tests {
    use super::*;
    use crate::domain::AbsDir;
    use crate::testing::{self, Entry};

    fn rel(text: &str) -> HomeRel {
        HomeRel::parse(text.to_owned()).unwrap()
    }

    fn home() -> HomeDir {
        HomeDir::parse("/home/u".to_owned()).unwrap()
    }

    fn file(text: &str) -> Observation {
        Observation::File {
            resolved: AbsPath::parse(text.to_owned()).unwrap(),
            executable: false,
        }
    }

    fn witness(e: &HomeFileWarning) -> &'static str {
        match e {
            HomeFileWarning::HomeFileMissing { .. } => "HomeFileMissing",
        }
    }

    fn warnings() -> Vec<HomeFileWarning> {
        vec![HomeFileWarning::HomeFileMissing {
            rel: rel(".gitconfig"),
        }]
    }

    /// One catalogue entry per variant.
    pub(crate) fn entries() -> Vec<Entry> {
        warnings().iter().map(Entry::of).collect()
    }

    #[test]
    fn every_warning_variant_has_a_table_case() {
        testing::every_variant(&warnings(), witness, &["HomeFileMissing"]);
    }

    #[test]
    fn the_warning_names_the_file_below_the_home() {
        assert_eq!(
            warnings().first().map(Diagnostic::message),
            Some("not copying ~/.gitconfig into the VM: no such regular file".to_owned())
        );
    }

    #[test]
    fn a_configured_file_is_looked_for_below_the_home() {
        testing::table(
            &[
                (".gitconfig", "/home/u/.gitconfig".to_owned()),
                (
                    ".config/git/config",
                    "/home/u/.config/git/config".to_owned(),
                ),
            ],
            |text| requested(&home(), &rel(text)).get().clone(),
        );
        let root = HomeDir::parse("/".to_owned()).unwrap();
        assert_eq!(requested(&root, &rel("x")).get(), "/x");
    }

    #[test]
    fn files_that_exist_are_copied_and_the_rest_warn_in_order() {
        let dir = |text: &str| AbsDir::parse(text.to_owned()).unwrap();
        let seen = vec![
            (rel("a"), file("/real/a")),
            (rel("missing"), Observation::Missing),
            (rel("b"), file("/real/b")),
            (
                rel("dir"),
                Observation::Directory {
                    resolved: dir("/home/u/dir"),
                },
            ),
            (rel("sock"), Observation::Other),
        ];
        let (present, warned) = split(&home(), seen);
        let names: Vec<(String, String)> = present
            .iter()
            .map(|p| (p.rel.get().clone(), p.file.path().get().clone()))
            .collect();
        assert_eq!(
            names,
            [
                ("a".to_owned(), "/real/a".to_owned()),
                ("b".to_owned(), "/real/b".to_owned())
            ]
        );
        assert_eq!(
            warned,
            vec![
                HomeFileWarning::HomeFileMissing {
                    rel: rel("missing")
                },
                HomeFileWarning::HomeFileMissing { rel: rel("dir") },
                HomeFileWarning::HomeFileMissing { rel: rel("sock") },
            ]
        );
    }

    #[test]
    fn nothing_configured_is_nothing_to_copy_and_nothing_to_warn_about() {
        assert_eq!(split(&home(), Vec::new()), (Vec::new(), Vec::new()));
    }
}
