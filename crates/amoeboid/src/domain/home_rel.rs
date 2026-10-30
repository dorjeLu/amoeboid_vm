//! `HomeRel`: a path relative to `$HOME`, normalised, that cannot leave it.

use crate::diagnostic::Diagnostic;
use crate::domain::components::{Flaw, first_flaw};
use crate::refined::{CheckedStr, Refined, Refinement};

/// Marker of [`HomeRel`].
pub struct HomeRelShape;

/// A relative path with at least one component and none empty, `.` or `..`; it does not start
/// with `~`, which shells and Python's `expanduser` would expand.
pub type HomeRel = Refined<String, HomeRelShape>;

#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum HomeRelError {
    #[error("bad home file path: it is empty")]
    Empty,
    #[error("bad home file path: it is absolute")]
    Absolute,
    #[error("bad home file path: it starts with '~'")]
    Tilde,
    /// `at` is a byte offset.
    #[error("bad home file path: NUL byte at offset {at}")]
    NulByte { at: usize },
    /// Doubled or trailing `/`; `index` counts components from 0.
    #[error("bad home file path: component {index} is empty (doubled or trailing '/')")]
    EmptyComponent { index: usize },
    #[error("bad home file path: component {index} is '.'")]
    CurrentDir { index: usize },
    #[error("bad home file path: component {index} is '..'")]
    ParentDir { index: usize },
}

impl Diagnostic for HomeRelError {
    fn code(&self) -> &'static str {
        match self {
            Self::Empty => "home-rel.empty",
            Self::Absolute => "home-rel.absolute",
            Self::Tilde => "home-rel.tilde",
            Self::NulByte { .. } => "home-rel.nul-byte",
            Self::EmptyComponent { .. } => "home-rel.empty-component",
            Self::CurrentDir { .. } => "home-rel.current-dir",
            Self::ParentDir { .. } => "home-rel.parent-dir",
        }
    }

    fn message(&self) -> String {
        self.to_string()
    }

    fn hint(&self) -> Option<String> {
        Some("give a path relative to $HOME without '..', e.g. .config/git/config".to_owned())
    }
}

impl Refinement<String> for HomeRelShape {
    type Error = HomeRelError;

    fn check(value: &String) -> Result<(), HomeRelError> {
        if value.is_empty() {
            return Err(HomeRelError::Empty);
        }
        if value.starts_with('/') {
            return Err(HomeRelError::Absolute);
        }
        if value.starts_with('~') {
            return Err(HomeRelError::Tilde);
        }
        if let Some(at) = value.find('\0') {
            return Err(HomeRelError::NulByte { at });
        }
        match first_flaw(value) {
            None => Ok(()),
            Some((Flaw::Empty, index)) => Err(HomeRelError::EmptyComponent { index }),
            Some((Flaw::Current, index)) => Err(HomeRelError::CurrentDir { index }),
            Some((Flaw::Parent, index)) => Err(HomeRelError::ParentDir { index }),
        }
    }
}

impl HomeRelShape {
    /// Compile-time twin of [`Refinement::check`], for `refined!`.
    #[must_use]
    pub const fn check_const(value: &'static str) -> Option<CheckedStr<Self>> {
        if fits(value) {
            Some(CheckedStr::new(value))
        } else {
            None
        }
    }
}

/// The predicate as a `const fn`: a relative, non-`~` text whose components are none empty, `.`
/// or `..`, without NUL.
const fn fits(text: &str) -> bool {
    let mut rest = text.as_bytes();
    if let [] | [b'/' | b'~', ..] = rest {
        return false;
    }
    let mut len = 0_usize;
    let mut dots_only = true;
    while let [byte, tail @ ..] = rest {
        rest = tail;
        match *byte {
            0 => return false,
            b'/' => {
                if !component_fits(len, dots_only) {
                    return false;
                }
                len = 0;
                dots_only = true;
            }
            other => {
                len = len.saturating_add(1);
                dots_only = dots_only && other == b'.';
            }
        }
    }
    component_fits(len, dots_only)
}

/// A component of `len` bytes (`dots_only`: all of them `.`) that is not empty, `.` or `..`.
const fn component_fits(len: usize, dots_only: bool) -> bool {
    len != 0 && !(dots_only && len <= 2)
}

impl HomeRel {
    /// Like [`HomeRel::parse`], but first drops empty and `.` components, as Python's
    /// `PurePosixPath` does when `run_config.kdl` is read: `./a//b/` becomes `a/b`. `..` is
    /// kept (and then refused); a text with nothing left is empty.
    ///
    /// # Errors
    ///
    /// Whatever [`HomeRel::parse`] refuses in the cleaned text, or an absolute or `~` text.
    pub fn normalising(raw: &str) -> Result<Self, HomeRelError> {
        if raw.starts_with('/') || raw.starts_with('~') {
            return Self::parse(raw.to_owned());
        }
        let kept: Vec<&str> = raw
            .split('/')
            .filter(|part| !part.is_empty() && *part != ".")
            .collect();
        Self::parse(kept.join("/"))
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) mod tests {
    use super::*;
    use crate::refined;
    use crate::testing::{self, Entry};

    fn witness(e: &HomeRelError) -> &'static str {
        match e {
            HomeRelError::Empty => "Empty",
            HomeRelError::Absolute => "Absolute",
            HomeRelError::Tilde => "Tilde",
            HomeRelError::NulByte { .. } => "NulByte",
            HomeRelError::EmptyComponent { .. } => "EmptyComponent",
            HomeRelError::CurrentDir { .. } => "CurrentDir",
            HomeRelError::ParentDir { .. } => "ParentDir",
        }
    }

    fn errors() -> Vec<HomeRelError> {
        vec![
            HomeRelError::Empty,
            HomeRelError::Absolute,
            HomeRelError::Tilde,
            HomeRelError::NulByte { at: 2 },
            HomeRelError::EmptyComponent { index: 1 },
            HomeRelError::CurrentDir { index: 0 },
            HomeRelError::ParentDir { index: 3 },
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
                "Tilde",
                "NulByte",
                "EmptyComponent",
                "CurrentDir",
                "ParentDir",
            ],
        );
    }

    #[test]
    fn home_relative_paths_are_accepted_when_they_stay_inside_home() {
        testing::table(
            &[
                ("", Err(HomeRelError::Empty)),
                ("/", Err(HomeRelError::Absolute)),
                ("/abs", Err(HomeRelError::Absolute)),
                ("~", Err(HomeRelError::Tilde)),
                ("~/x", Err(HomeRelError::Tilde)),
                ("~user/x", Err(HomeRelError::Tilde)),
                (".gitconfig", Ok(())),
                (".config/git/config", Ok(())),
                ("a", Ok(())),
                ("a b/c~", Ok(())),
                ("a/~b", Ok(())),
                ("..x/.y/...", Ok(())),
                ("日本/é", Ok(())),
                ("a\0", Err(HomeRelError::NulByte { at: 1 })),
                ("a/b\0", Err(HomeRelError::NulByte { at: 3 })),
                ("a//b", Err(HomeRelError::EmptyComponent { index: 1 })),
                ("a/", Err(HomeRelError::EmptyComponent { index: 1 })),
                (".", Err(HomeRelError::CurrentDir { index: 0 })),
                ("./a", Err(HomeRelError::CurrentDir { index: 0 })),
                ("a/./b", Err(HomeRelError::CurrentDir { index: 1 })),
                ("..", Err(HomeRelError::ParentDir { index: 0 })),
                ("../escape", Err(HomeRelError::ParentDir { index: 0 })),
                ("a/b/../c", Err(HomeRelError::ParentDir { index: 2 })),
            ],
            |text| HomeRel::parse((*text).to_owned()).map(|_| ()),
        );
    }

    #[test]
    fn check_const_agrees_with_check_at_every_edge() {
        for text in [
            "",
            "/",
            "/abs",
            "~",
            "~/x",
            "~user/x",
            ".gitconfig",
            ".config/git/config",
            "a",
            "a b/c~",
            "a/~b",
            "..x/.y/...",
            "日本/é",
            "a\0",
            "a/b\0",
            "a//b",
            "a/",
            ".",
            "./a",
            "a/./b",
            "..",
            "../escape",
            "a/b/../c",
            "a/..",
            "a/.",
            "...",
            "a/...",
            "//",
        ] {
            assert_eq!(
                HomeRelShape::check_const(text).is_some(),
                HomeRelShape::check(&text.to_owned()).is_ok(),
                "text {text:?}"
            );
        }
    }

    #[test]
    fn refined_constants_of_home_paths_are_the_parsed_values() {
        assert_eq!(
            HomeRel::parse(".config/git/config".to_owned()),
            Ok(refined!(HomeRel, ".config/git/config"))
        );
    }

    #[test]
    fn normalising_drops_empty_and_dot_components_but_never_a_parent_one() {
        let ok = |text: &str| Ok(text.to_owned());
        testing::table(
            &[
                ("", Err(HomeRelError::Empty)),
                (".", Err(HomeRelError::Empty)),
                ("./", Err(HomeRelError::Empty)),
                ("//", Err(HomeRelError::Absolute)),
                ("/abs", Err(HomeRelError::Absolute)),
                ("~/x", Err(HomeRelError::Tilde)),
                (".gitconfig", ok(".gitconfig")),
                ("./.gitconfig", ok(".gitconfig")),
                ("a//b/", ok("a/b")),
                ("./a/./b/.", ok("a/b")),
                ("a/~b", ok("a/~b")),
                ("../escape", Err(HomeRelError::ParentDir { index: 0 })),
                ("./a/../b", Err(HomeRelError::ParentDir { index: 1 })),
                ("a\0b", Err(HomeRelError::NulByte { at: 1 })),
            ],
            |text| HomeRel::normalising(text).map(HomeRel::into_inner),
        );
    }

    #[test]
    fn accepted_paths_parse_back_to_themselves() {
        let rel = HomeRel::parse(".config/git/config".to_owned()).unwrap();
        assert_eq!(HomeRel::parse(rel.get().clone()), Ok(rel));
    }
}
