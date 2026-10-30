//! `FlakePath`, `FlakeDir`: absolute paths that are safe to put in a `path:` flake URL
//! (`nix build path:/dir --override-input host path:/dir/host.nix`).
//!
//! Nix parses that text as a URL: `?` starts a query, `#` a fragment, and `%XX` is decoded, so a
//! path containing one of them silently names something else (or nothing). Every other character
//! outside [`ALLOWED`] (probed with Nix 2.35.4 on `path:` URLs: space, `"`, `<`, `>`, `\`,
//! backtick, `[`, `]`, `^`, `{`, `|`, `}`, DEL, control and non-ASCII characters) makes Nix
//! refuse the flake with a confusing "getting status of" error. All of them are refused when the
//! type is made. A path made by joining a [`FlakeSegment`] onto one needs no second check: those
//! names are proven to use only [`ALLOWED`] characters.
//!
//! Outside this crate the constructor and [`FlakeSegment`] are unreachable, so a path that skips
//! the check cannot be made:
//!
//! ```compile_fail,E0603
//! use amoeboid::domain::{AbsPath, flake_path::FlakeAbs};
//!
//! let path = AbsPath::root();
//! let forged = FlakeAbs(path);
//! ```
//!
//! ```compile_fail,E0277
//! use amoeboid::domain::{FlakeDir, FlakeSegment, Segment};
//!
//! struct Query;
//! impl FlakeSegment for Query {}
//! ```

use std::fmt;

use crate::diagnostic::Diagnostic;
use crate::domain::abs_path::{Abs, Directory, Segment, kind};

/// The ASCII punctuation Nix accepts in a `path:` URL at any position, besides letters, digits
/// and the `/` separator.
pub const ALLOWED: &str = "!$&'()*+,-.:;=@_~";

/// A name whose predicate only admits [`ALLOWED`] characters, letters and digits, so joining it
/// onto a [`FlakeAbs`] keeps the path flake-safe. Sealed through [`Segment`]; implement it only for types whose own predicate
/// implies that, with a test.
pub trait FlakeSegment: Segment {}

/// An absolute path proven to use only letters, digits, `/` and [`ALLOWED`] characters; `K` says what it names, as for [`Abs`].
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FlakeAbs<K>(Abs<K>);

/// A flake-safe absolute path.
pub type FlakePath = FlakeAbs<kind::Entry>;
/// A flake-safe absolute path naming a directory.
pub type FlakeDir = FlakeAbs<kind::Dir>;

#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum FlakePathError {
    /// `at` is a byte offset.
    #[error("bad flake path: '?' at offset {at} would start a URL query")]
    Query { at: usize },
    /// `at` is a byte offset.
    #[error("bad flake path: '#' at offset {at} would start a URL fragment")]
    Fragment { at: usize },
    /// `at` is a byte offset.
    #[error("bad flake path: '%' at offset {at} would be read as a URL escape")]
    Percent { at: usize },
    /// Any other character Nix refuses in a `path:` URL; `at` is a byte offset.
    #[error("bad flake path: {ch:?} at offset {at} is not accepted by nix in a path: URL")]
    BadChar { ch: char, at: usize },
}

impl Diagnostic for FlakePathError {
    fn code(&self) -> &'static str {
        match self {
            Self::Query { .. } => "flake-path.query",
            Self::Fragment { .. } => "flake-path.fragment",
            Self::Percent { .. } => "flake-path.percent",
            Self::BadChar { .. } => "flake-path.bad-char",
        }
    }

    fn message(&self) -> String {
        self.to_string()
    }

    fn hint(&self) -> Option<String> {
        Some(format!(
            "use a path of letters, digits, '/' and any of {ALLOWED}"
        ))
    }
}

impl<K> FlakeAbs<K> {
    /// # Errors
    /// The first character that is not a letter, digit, `/` or [`ALLOWED`].
    pub fn parse(path: Abs<K>) -> Result<Self, FlakePathError> {
        match path
            .get()
            .char_indices()
            .find(|&(_, ch)| !(ch.is_ascii_alphanumeric() || ch == '/' || ALLOWED.contains(ch)))
        {
            Some((at, '?')) => Err(FlakePathError::Query { at }),
            Some((at, '#')) => Err(FlakePathError::Fragment { at }),
            Some((at, '%')) => Err(FlakePathError::Percent { at }),
            Some((at, ch)) => Err(FlakePathError::BadChar { ch, at }),
            None => Ok(Self(path)),
        }
    }

    /// The same path as a plain [`Abs`].
    #[must_use]
    pub const fn abs(&self) -> &Abs<K> {
        &self.0
    }
}

impl<K: Directory> FlakeAbs<K> {
    /// The entry `name` in this directory.
    #[must_use]
    pub fn join(&self, name: &impl FlakeSegment) -> FlakePath {
        FlakeAbs(self.0.join(name))
    }

    /// The directory `name` in this directory.
    #[must_use]
    pub fn join_dir(&self, name: &impl FlakeSegment) -> FlakeDir {
        FlakeAbs(self.0.join_dir(name))
    }
}

/// Shows the text only, like the path types.
impl<K> fmt::Debug for FlakeAbs<K> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl<K> fmt::Display for FlakeAbs<K> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) mod tests {
    use super::*;
    use crate::domain::{AbsDir, AbsPath, FileName, VmName};
    use crate::refined;
    use crate::testing::{self, Entry};

    fn witness(e: &FlakePathError) -> &'static str {
        match e {
            FlakePathError::Query { .. } => "Query",
            FlakePathError::Fragment { .. } => "Fragment",
            FlakePathError::Percent { .. } => "Percent",
            FlakePathError::BadChar { .. } => "BadChar",
        }
    }

    fn errors() -> Vec<FlakePathError> {
        vec![
            FlakePathError::Query { at: 1 },
            FlakePathError::Fragment { at: 2 },
            FlakePathError::Percent { at: 3 },
            FlakePathError::BadChar { ch: ' ', at: 4 },
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
            &["Query", "Fragment", "Percent", "BadChar"],
        );
    }

    #[test]
    fn flake_path_parse_refuses_query_fragment_and_percent_and_names_the_first() {
        testing::table(
            &[
                ("/", Ok(())),
                ("/home/u/.local/share/amoeboid", Ok(())),
                ("/c=d&e:f+g;h,i", Ok(())),
                ("/!$&'()*+,-.:;=@_~/AZaz09", Ok(())),
                ("/a b", Err(FlakePathError::BadChar { ch: ' ', at: 2 })),
                ("/日本", Err(FlakePathError::BadChar { ch: '日', at: 1 })),
                ("/a\"b", Err(FlakePathError::BadChar { ch: '"', at: 2 })),
                ("/a\\b", Err(FlakePathError::BadChar { ch: '\\', at: 2 })),
                ("/a\nb", Err(FlakePathError::BadChar { ch: '\n', at: 2 })),
                (
                    "/a\u{7f}",
                    Err(FlakePathError::BadChar {
                        ch: '\u{7f}',
                        at: 2,
                    }),
                ),
                ("/ x?", Err(FlakePathError::BadChar { ch: ' ', at: 1 })),
                ("/a?b", Err(FlakePathError::Query { at: 2 })),
                ("/a#b", Err(FlakePathError::Fragment { at: 2 })),
                ("/a%41", Err(FlakePathError::Percent { at: 2 })),
                ("/?", Err(FlakePathError::Query { at: 1 })),
                ("/x%#?", Err(FlakePathError::Percent { at: 2 })),
                ("/x#?%", Err(FlakePathError::Fragment { at: 2 })),
                ("/é?", Err(FlakePathError::BadChar { ch: 'é', at: 1 })),
            ],
            |text| {
                let path = AbsPath::parse((*text).to_owned()).unwrap();
                FlakePath::parse(path).map(|flake| {
                    assert_eq!(flake.to_string(), *text);
                    assert_eq!(format!("{flake:?}"), format!("{text:?}"));
                    assert_eq!(flake.abs().get(), text);
                })
            },
        );
    }

    #[test]
    fn joining_flake_segments_keeps_the_path_flake_safe() {
        let base = FlakeDir::parse(AbsDir::parse("/srv/data".to_owned()).unwrap()).unwrap();
        let vm = VmName::parse("my-vm".to_owned()).unwrap();
        let file: FileName = refined!(FileName, "host.nix");
        let dir = base.join_dir(&vm);
        let path = dir.join(&file);
        assert_eq!(path.to_string(), "/srv/data/my-vm/host.nix");
        assert_eq!(base.join(&file).to_string(), "/srv/data/host.nix");
        for flake in [&dir.abs().to_dir(), &AbsDir::root().join_dir(&vm)] {
            assert_eq!(
                FlakeDir::parse(flake.clone()).map(|d| d.to_string()),
                Ok(flake.to_string())
            );
        }
        assert_eq!(FlakePath::parse(path.abs().clone()), Ok(path));
    }

    #[test]
    fn every_ascii_character_nix_refuses_in_a_path_url_is_refused() {
        // Probed with `nix eval path:/tmp/x<c>y#marker` (Nix 2.35.4): these are exactly the ASCII
        // characters that are not accepted at any position.
        let accepted = |ch: char| ch.is_ascii_alphanumeric() || ALLOWED.contains(ch);
        for code in 1_u8..=127 {
            let ch = char::from(code);
            if ch == '/' {
                continue;
            }
            let path = AbsPath::parse(format!("/x{ch}y")).unwrap();
            assert_eq!(FlakePath::parse(path).is_ok(), accepted(ch), "{ch:?}");
        }
    }

    #[test]
    fn every_flake_segment_uses_only_accepted_characters() {
        let files = [
            "lock",
            "run_config.kdl",
            "ssh_host_ed25519_key",
            "A-b_c.d",
            "...",
        ];
        for text in files {
            let name = FileName::parse(text).unwrap();
            assert!(
                FlakePath::parse(AbsDir::root().join(&name)).is_ok(),
                "{text}"
            );
        }
        for text in ["a", "Z9", "a-b", "x".repeat(40).as_str()] {
            let name = VmName::parse(text.to_owned()).unwrap();
            assert!(
                FlakePath::parse(AbsDir::root().join(&name)).is_ok(),
                "{text}"
            );
        }
    }
}
