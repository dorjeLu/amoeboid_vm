//! `GuestRel`: a path relative to the guest-writable share root (`<data>/NAME/guest`), and
//! [`GuestFile`], a file the host puts there.
//!
//! The share is the one directory the guest can write, so every path the host writes into it is
//! one of these: relative, normalised, and unable to leave the root (no `..`, no leading `/`).
//! Not a `Refined`: besides [`GuestRel::parse`] it has total constructors from proven pieces
//! ([`GuestRel::of`], [`GuestRel::join`], [`GuestRel::of_home`]).

use std::fmt;

use crate::diagnostic::Diagnostic;
use crate::domain::abs_path::Segment;
use crate::domain::components::{Flaw, first_flaw};
use crate::domain::file_mode::FileMode;
use crate::domain::home_rel::HomeRel;

/// A relative path with at least one component, none of them empty, `.` or `..`.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GuestRel(String);

#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum GuestRelError {
    #[error("bad guest path: it is empty")]
    Empty,
    #[error("bad guest path: it is absolute")]
    Absolute,
    /// `at` is a byte offset.
    #[error("bad guest path: NUL byte at offset {at}")]
    NulByte { at: usize },
    /// Doubled or trailing `/`; `index` counts components from 0.
    #[error("bad guest path: component {index} is empty (doubled or trailing '/')")]
    EmptyComponent { index: usize },
    #[error("bad guest path: component {index} is '.'")]
    CurrentDir { index: usize },
    #[error("bad guest path: component {index} is '..'")]
    ParentDir { index: usize },
}

impl Diagnostic for GuestRelError {
    fn code(&self) -> &'static str {
        match self {
            Self::Empty => "guest-rel.empty",
            Self::Absolute => "guest-rel.absolute",
            Self::NulByte { .. } => "guest-rel.nul-byte",
            Self::EmptyComponent { .. } => "guest-rel.empty-component",
            Self::CurrentDir { .. } => "guest-rel.current-dir",
            Self::ParentDir { .. } => "guest-rel.parent-dir",
        }
    }

    fn message(&self) -> String {
        self.to_string()
    }

    fn hint(&self) -> Option<String> {
        Some("give a path relative to the share without '..', e.g. home/.gitconfig".to_owned())
    }
}

impl GuestRel {
    /// # Errors
    /// The [`GuestRelError`] naming the first violated rule.
    pub fn parse(value: String) -> Result<Self, GuestRelError> {
        if value.is_empty() {
            return Err(GuestRelError::Empty);
        }
        if value.starts_with('/') {
            return Err(GuestRelError::Absolute);
        }
        if let Some(at) = value.find('\0') {
            return Err(GuestRelError::NulByte { at });
        }
        match first_flaw(&value) {
            None => Ok(Self(value)),
            Some((Flaw::Empty, index)) => Err(GuestRelError::EmptyComponent { index }),
            Some((Flaw::Current, index)) => Err(GuestRelError::CurrentDir { index }),
            Some((Flaw::Parent, index)) => Err(GuestRelError::ParentDir { index }),
        }
    }

    /// The one-component path `name`.
    #[must_use]
    pub fn of(name: &impl Segment) -> Self {
        Self(name.segment().to_owned())
    }

    /// A [`HomeRel`] is such a path already (it only forbids more).
    #[must_use]
    pub fn of_home(rel: &HomeRel) -> Self {
        Self(rel.get().clone())
    }

    /// `self/below`.
    #[must_use]
    pub fn join(&self, below: &Self) -> Self {
        Self(format!("{}/{}", self.0, below.0))
    }

    #[must_use]
    pub const fn get(&self) -> &String {
        &self.0
    }

    /// The components, first to last; at least one.
    pub fn components(&self) -> impl Iterator<Item = &str> {
        self.0.split('/')
    }

    /// The directories above the last component, outermost first (none for a one-component
    /// path), and the last component.
    pub fn dirs_and_name(&self) -> (impl Iterator<Item = &str>, &str) {
        let (dirs, name) = self.0.rsplit_once('/').unwrap_or(("", &self.0));
        (dirs.split('/').filter(|part| !part.is_empty()), name)
    }

    /// The first `count` components (at least one, at most all).
    #[must_use]
    pub fn first(&self, count: usize) -> Self {
        let parts: Vec<&str> = self.components().take(count.max(1)).collect();
        Self(parts.join("/"))
    }
}

/// Shows the text only, like the path types.
impl fmt::Debug for GuestRel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl fmt::Display for GuestRel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// A regular file the host puts in the share: replaced as a whole, never written through.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct GuestFile {
    pub dest: GuestRel,
    pub bytes: Vec<u8>,
    pub mode: FileMode,
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) mod tests {
    use super::*;
    use crate::domain::{AbsDir, AbsPath, FileName, VmName};
    use crate::refined;
    use crate::testing::{self, Entry};

    fn witness(e: &GuestRelError) -> &'static str {
        match e {
            GuestRelError::Empty => "Empty",
            GuestRelError::Absolute => "Absolute",
            GuestRelError::NulByte { .. } => "NulByte",
            GuestRelError::EmptyComponent { .. } => "EmptyComponent",
            GuestRelError::CurrentDir { .. } => "CurrentDir",
            GuestRelError::ParentDir { .. } => "ParentDir",
        }
    }

    fn errors() -> Vec<GuestRelError> {
        vec![
            GuestRelError::Empty,
            GuestRelError::Absolute,
            GuestRelError::NulByte { at: 1 },
            GuestRelError::EmptyComponent { index: 1 },
            GuestRelError::CurrentDir { index: 0 },
            GuestRelError::ParentDir { index: 2 },
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
            &[
                "Empty",
                "Absolute",
                "NulByte",
                "EmptyComponent",
                "CurrentDir",
                "ParentDir",
            ],
        );
    }

    #[test]
    fn a_guest_path_is_relative_normalised_and_cannot_leave_the_share() {
        testing::table(
            &[
                ("", Err(GuestRelError::Empty)),
                ("/", Err(GuestRelError::Absolute)),
                ("/etc/passwd", Err(GuestRelError::Absolute)),
                ("a\0b", Err(GuestRelError::NulByte { at: 1 })),
                ("a//b", Err(GuestRelError::EmptyComponent { index: 1 })),
                ("a/", Err(GuestRelError::EmptyComponent { index: 1 })),
                ("./a", Err(GuestRelError::CurrentDir { index: 0 })),
                ("a/./b", Err(GuestRelError::CurrentDir { index: 1 })),
                ("..", Err(GuestRelError::ParentDir { index: 0 })),
                ("../escape", Err(GuestRelError::ParentDir { index: 0 })),
                ("a/../../escape", Err(GuestRelError::ParentDir { index: 1 })),
                ("a", Ok("a".to_owned())),
                (
                    "home/.config/git/config",
                    Ok("home/.config/git/config".to_owned()),
                ),
                ("~/x", Ok("~/x".to_owned())),
                ("...", Ok("...".to_owned())),
                ("a b/日本", Ok("a b/日本".to_owned())),
            ],
            |text| GuestRel::parse((*text).to_owned()).map(|rel| rel.to_string()),
        );
    }

    #[test]
    fn a_path_splits_into_its_directories_and_its_name_and_into_prefixes() {
        let rel = |text: &str| GuestRel::parse(text.to_owned()).unwrap();
        testing::table(
            &[
                ("a", (vec![], "a".to_owned())),
                ("a/b", (vec!["a".to_owned()], "b".to_owned())),
                (
                    "home/.config/git/config",
                    (
                        vec!["home".to_owned(), ".config".to_owned(), "git".to_owned()],
                        "config".to_owned(),
                    ),
                ),
            ],
            |text| {
                let rel = rel(text);
                let (dirs, name) = rel.dirs_and_name();
                let dirs: Vec<&str> = dirs.collect();
                assert_eq!(
                    dirs.join("/").len() + usize::from(!dirs.is_empty()) + name.len(),
                    rel.get().len()
                );
                (
                    dirs.iter().map(ToString::to_string).collect::<Vec<_>>(),
                    name.to_owned(),
                )
            },
        );
        let long = rel("a/b/c");
        testing::table(
            &[
                (0, "a".to_owned()),
                (1, "a".to_owned()),
                (2, "a/b".to_owned()),
                (3, "a/b/c".to_owned()),
                (9, "a/b/c".to_owned()),
            ],
            |count| long.first(*count).to_string(),
        );
    }

    #[test]
    fn a_parent_component_cannot_be_made_into_a_guest_path() {
        // The dot-dot attack of the Python tests: such a path never reaches the delivery.
        for text in ["../escape", "home/../../escape", ".."] {
            assert!(matches!(
                GuestRel::parse(text.to_owned()),
                Err(GuestRelError::ParentDir { .. })
            ));
        }
    }

    #[test]
    fn a_guest_path_below_a_directory_is_that_directory_and_the_path() {
        let rel = GuestRel::parse("home/.gitconfig".to_owned()).unwrap();
        testing::table(
            &[
                ("/h/vm/guest", "/h/vm/guest/home/.gitconfig".to_owned()),
                ("/", "/home/.gitconfig".to_owned()),
            ],
            |root| {
                let dir = AbsDir::parse((*root).to_owned()).unwrap();
                let path = dir.join_rel(&rel);
                assert_eq!(AbsPath::parse(path.get().clone()), Ok(path.clone()));
                path.into_inner()
            },
        );
    }

    #[test]
    fn total_constructors_make_what_parse_accepts() {
        let file: FileName = refined!(FileName, "hostname");
        let vm = VmName::parse("my-vm".to_owned()).unwrap();
        let home = HomeRel::parse(".config/git/config".to_owned()).unwrap();
        let one = GuestRel::of(&file);
        assert_eq!(one.to_string(), "hostname");
        assert_eq!(GuestRel::of(&vm).to_string(), "my-vm");
        let joined = GuestRel::of(&vm).join(&GuestRel::of_home(&home));
        assert_eq!(joined.to_string(), "my-vm/.config/git/config");
        assert_eq!(
            joined.components().collect::<Vec<_>>(),
            ["my-vm", ".config", "git", "config"]
        );
        for rel in [one, joined] {
            assert_eq!(GuestRel::parse(rel.get().clone()), Ok(rel.clone()));
            assert_eq!(format!("{rel:?}"), format!("{:?}", rel.get()));
        }
    }
}
