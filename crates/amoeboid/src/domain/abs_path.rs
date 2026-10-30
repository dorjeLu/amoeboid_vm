//! `AbsPath`, `AbsDir`, `HomeDir`: absolute, normalised host paths (no `.`, `..` or empty
//! components). Pure text: whether the path exists or is a directory is an observation (`HostDir`).
//!
//! Raw text comes in through `parse`; paths computed from proofs (a directory plus a [`Segment`])
//! come from the total [`Abs::join`], [`Abs::join_dir`] and [`Abs::parent`], never re-parsed
//! (design §Where values come from). That is why these are not `Refined` aliases: the total
//! operations need the private field.
//!
//! Outside this crate neither the constructor nor [`Segment`] is reachable, so a path that skips
//! the predicate cannot be made:
//!
//! ```compile_fail,E0603
//! use std::marker::PhantomData;
//! use amoeboid::domain::abs_path::{Abs, kind};
//!
//! let forged: Abs<kind::Dir> = Abs("relative/..".to_owned(), PhantomData);
//! ```
//!
//! ```compile_fail,E0277
//! use amoeboid::domain::abs_path::{AbsDir, Segment};
//!
//! struct Up;
//! impl Segment for Up {
//!     fn segment(&self) -> &str { ".." }
//! }
//! let forged = AbsDir::root().join_dir(&Up);
//! ```

use std::fmt;
use std::marker::PhantomData;

use crate::diagnostic::Diagnostic;
use crate::domain::components::{Flaw, first_flaw};
use crate::domain::guest_rel::GuestRel;

/// What an absolute path names (by intent); a type parameter of [`Abs`] only.
pub mod kind {
    /// Any entry: a file, socket or directory.
    #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
    pub enum Entry {}
    /// A directory.
    #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
    pub enum Dir {}
    /// The invoking user's home directory, from `$HOME`.
    #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
    pub enum Home {}
    /// A base directory from an `$XDG_*` variable.
    #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
    pub enum Xdg {}
    /// The temporary directory from `$TMPDIR`.
    #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
    pub enum Tmp {}
}

/// Seals [`Segment`] and [`Directory`]: their implementations are proofs this crate vouches for.
pub(crate) mod sealed {
    pub trait Sealed {}
}

/// Text proven to be exactly one path component: non-empty, no `/`, no NUL, not `.` or `..`.
///
/// Sealed: only types of this crate whose own predicate implies that implement it, each with a
/// test.
pub trait Segment: sealed::Sealed {
    fn segment(&self) -> &str;
}

/// The kinds under which entries can be joined.
pub trait Directory: sealed::Sealed {}

impl sealed::Sealed for kind::Dir {}
impl sealed::Sealed for kind::Home {}
impl sealed::Sealed for kind::Xdg {}
impl sealed::Sealed for kind::Tmp {}
impl Directory for kind::Dir {}
impl Directory for kind::Home {}
impl Directory for kind::Xdg {}
impl Directory for kind::Tmp {}

/// An absolute, normalised path naming a `K` (see [`kind`]).
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Abs<K>(String, PhantomData<fn() -> K>);

/// An absolute, normalised path.
pub type AbsPath = Abs<kind::Entry>;
/// An absolute, normalised path that names a directory (by intent), kept apart so a path that is
/// meant to name a directory is never mixed up with one that may name a file.
pub type AbsDir = Abs<kind::Dir>;
/// The invoking user's home directory, from `$HOME`.
pub type HomeDir = Abs<kind::Home>;
/// A base directory named by an `$XDG_*` variable.
pub type XdgDir = Abs<kind::Xdg>;
/// The temporary directory named by `$TMPDIR`.
pub type TmpDir = Abs<kind::Tmp>;

impl<K> Abs<K> {
    /// # Errors
    /// The [`AbsPathError`] naming the first violated rule.
    pub fn parse(value: String) -> Result<Self, AbsPathError> {
        check(&value).map(|()| Self(value, PhantomData))
    }

    /// Like [`Abs::parse`] after dropping empty and `.` components, which name the same entry
    /// to the kernel (`/home/u/` is `/home/u`). `..` is refused, not folded: after a symlink it
    /// names something else than the text suggests.
    ///
    /// # Errors
    /// As [`Abs::parse`], except `EmptyComponent` and `CurrentDir`.
    pub fn normalising(text: &str) -> Result<Self, AbsPathError> {
        match text.strip_prefix('/') {
            Some(rest) => {
                let parts: Vec<&str> = rest
                    .split('/')
                    .filter(|p| !matches!(*p, "" | "."))
                    .collect();
                Self::parse(format!("/{}", parts.join("/")))
            }
            None => Self::parse(text.to_owned()),
        }
    }

    /// `/`.
    #[must_use]
    pub fn root() -> Self {
        Self("/".to_owned(), PhantomData)
    }

    #[must_use]
    pub const fn get(&self) -> &String {
        &self.0
    }

    #[must_use]
    pub fn into_inner(self) -> String {
        self.0
    }

    /// The directory above; `/` is its own parent.
    #[must_use]
    pub fn parent(&self) -> AbsDir {
        match self.0.rsplit_once('/') {
            Some(("", _)) | None => Abs::root(),
            Some((above, _)) => Abs(above.to_owned(), PhantomData),
        }
    }
}

impl AbsDir {
    /// Names this directory the invoking user's home. Crate-private: only an observation of
    /// `$HOME` (`GuestHome::observe`) may make that claim about a resolved path.
    pub(crate) fn into_home(self) -> HomeDir {
        Abs(self.0, PhantomData)
    }
}

impl<K: Directory> Abs<K> {
    /// The entry `name` in this directory.
    #[must_use]
    pub fn join(&self, name: &impl Segment) -> AbsPath {
        Abs(self.below(name), PhantomData)
    }

    /// The directory `name` in this directory.
    #[must_use]
    pub fn join_dir(&self, name: &impl Segment) -> AbsDir {
        Abs(self.below(name), PhantomData)
    }

    /// The entry `rel` below this directory.
    #[must_use]
    pub fn join_rel(&self, rel: &GuestRel) -> AbsPath {
        let sep = if self.0 == "/" { "" } else { "/" };
        Abs(format!("{}{sep}{rel}", self.0), PhantomData)
    }

    /// The same directory as a plain [`AbsDir`] (a home, an XDG base, the temp dir).
    #[must_use]
    pub fn to_dir(&self) -> AbsDir {
        Abs(self.0.clone(), PhantomData)
    }

    /// The same path as an [`AbsPath`], for messages about the directory as an entry.
    #[must_use]
    pub fn to_path(&self) -> AbsPath {
        Abs(self.0.clone(), PhantomData)
    }

    fn below(&self, name: &impl Segment) -> String {
        let sep = if self.0 == "/" { "" } else { "/" };
        format!("{}{sep}{}", self.0, name.segment())
    }
}

impl<K> AsRef<String> for Abs<K> {
    fn as_ref(&self) -> &String {
        &self.0
    }
}

/// Shows the text only, like the refined types.
impl<K> fmt::Debug for Abs<K> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl<K> fmt::Display for Abs<K> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum AbsPathError {
    #[error("bad path: it is empty")]
    Empty,
    #[error("bad path: it isn't absolute")]
    NotAbsolute,
    /// `at` is a byte offset.
    #[error("bad path: NUL byte at offset {at}")]
    NulByte { at: usize },
    /// Doubled or trailing `/`; `index` counts components after the leading `/`, from 0.
    #[error("bad path: component {index} is empty (doubled or trailing '/')")]
    EmptyComponent { index: usize },
    #[error("bad path: component {index} is '.'")]
    CurrentDir { index: usize },
    #[error("bad path: component {index} is '..'")]
    ParentDir { index: usize },
}

impl Diagnostic for AbsPathError {
    fn code(&self) -> &'static str {
        match self {
            Self::Empty => "abs-path.empty",
            Self::NotAbsolute => "abs-path.not-absolute",
            Self::NulByte { .. } => "abs-path.nul-byte",
            Self::EmptyComponent { .. } => "abs-path.empty-component",
            Self::CurrentDir { .. } => "abs-path.current-dir",
            Self::ParentDir { .. } => "abs-path.parent-dir",
        }
    }

    fn message(&self) -> String {
        self.to_string()
    }

    fn hint(&self) -> Option<String> {
        Some(
            "give an absolute path without '.', '..', doubled or trailing '/', \
             e.g. /home/user/dev"
                .to_owned(),
        )
    }
}

/// The predicate behind all three kinds.
fn check(value: &str) -> Result<(), AbsPathError> {
    let Some(rest) = value.strip_prefix('/') else {
        return Err(if value.is_empty() {
            AbsPathError::Empty
        } else {
            AbsPathError::NotAbsolute
        });
    };
    if let Some(at) = value.find('\0') {
        return Err(AbsPathError::NulByte { at });
    }
    if rest.is_empty() {
        return Ok(());
    }
    match first_flaw(rest) {
        None => Ok(()),
        Some((Flaw::Empty, index)) => Err(AbsPathError::EmptyComponent { index }),
        Some((Flaw::Current, index)) => Err(AbsPathError::CurrentDir { index }),
        Some((Flaw::Parent, index)) => Err(AbsPathError::ParentDir { index }),
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) mod tests {
    use super::*;
    use crate::testing::{self, Entry};

    fn witness(e: &AbsPathError) -> &'static str {
        match e {
            AbsPathError::Empty => "Empty",
            AbsPathError::NotAbsolute => "NotAbsolute",
            AbsPathError::NulByte { .. } => "NulByte",
            AbsPathError::EmptyComponent { .. } => "EmptyComponent",
            AbsPathError::CurrentDir { .. } => "CurrentDir",
            AbsPathError::ParentDir { .. } => "ParentDir",
        }
    }

    fn errors() -> Vec<AbsPathError> {
        vec![
            AbsPathError::Empty,
            AbsPathError::NotAbsolute,
            AbsPathError::NulByte { at: 3 },
            AbsPathError::EmptyComponent { index: 1 },
            AbsPathError::CurrentDir { index: 0 },
            AbsPathError::ParentDir { index: 2 },
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
                "NotAbsolute",
                "NulByte",
                "EmptyComponent",
                "CurrentDir",
                "ParentDir",
            ],
        );
    }

    #[test]
    fn absolute_paths_are_accepted_when_normalised_and_the_violated_edge_is_named() {
        testing::table(
            &[
                ("", Err(AbsPathError::Empty)),
                ("a", Err(AbsPathError::NotAbsolute)),
                ("./a", Err(AbsPathError::NotAbsolute)),
                ("~", Err(AbsPathError::NotAbsolute)),
                ("~/a", Err(AbsPathError::NotAbsolute)),
                (" /a", Err(AbsPathError::NotAbsolute)),
                ("a\0", Err(AbsPathError::NotAbsolute)),
                ("/", Ok(())),
                ("/a", Ok(())),
                ("/a/b", Ok(())),
                ("/a b", Ok(())),
                ("/日本", Ok(())),
                ("/a:b", Ok(())),
                ("/~x", Ok(())),
                ("/.x", Ok(())),
                ("/..x", Ok(())),
                ("/...", Ok(())),
                ("/\n", Ok(())),
                ("/\0", Err(AbsPathError::NulByte { at: 1 })),
                ("/a\0b", Err(AbsPathError::NulByte { at: 2 })),
                ("/é\0", Err(AbsPathError::NulByte { at: 3 })),
                ("//", Err(AbsPathError::EmptyComponent { index: 0 })),
                ("/a/", Err(AbsPathError::EmptyComponent { index: 1 })),
                ("/a//b", Err(AbsPathError::EmptyComponent { index: 1 })),
                ("/.", Err(AbsPathError::CurrentDir { index: 0 })),
                ("/a/./b", Err(AbsPathError::CurrentDir { index: 1 })),
                ("/..", Err(AbsPathError::ParentDir { index: 0 })),
                ("/a/b/..", Err(AbsPathError::ParentDir { index: 2 })),
                ("/a/../b", Err(AbsPathError::ParentDir { index: 1 })),
            ],
            |text| {
                let all = (
                    AbsPath::parse((*text).to_owned()).map(|_| ()),
                    AbsDir::parse((*text).to_owned()).map(|_| ()),
                    HomeDir::parse((*text).to_owned()).map(|_| ()),
                );
                // The three kinds share one predicate.
                assert_eq!(all.0, all.1, "AbsPath and AbsDir disagree on {text:?}");
                assert_eq!(all.0, all.2, "AbsPath and HomeDir disagree on {text:?}");
                all.0
            },
        );
    }

    /// A test segment; the texts used are valid components.
    struct Name(&'static str);

    impl sealed::Sealed for Name {}

    impl Segment for Name {
        fn segment(&self) -> &str {
            self.0
        }
    }

    fn dir(text: &str) -> AbsDir {
        AbsDir::parse(text.to_owned()).unwrap()
    }

    #[test]
    fn join_appends_one_component_and_yields_a_valid_path() {
        testing::table(
            &[
                (("/", "a"), "/a".to_owned()),
                (("/a", "b"), "/a/b".to_owned()),
                (("/a/b", ".x"), "/a/b/.x".to_owned()),
                (("/a", "c d"), "/a/c d".to_owned()),
            ],
            |(base, name)| {
                let path = dir(base).join(&Name(name));
                let sub = dir(base).join_dir(&Name(name));
                assert_eq!(path.get(), sub.get());
                assert_eq!(AbsPath::parse(path.get().clone()), Ok(path.clone()));
                path.into_inner()
            },
        );
    }

    #[test]
    fn a_home_is_joined_like_a_directory_and_converts_to_one() {
        let home = HomeDir::parse("/home/u".to_owned()).unwrap();
        assert_eq!(home.join_dir(&Name("dev")), dir("/home/u/dev"));
        assert_eq!(home.to_dir(), dir("/home/u"));
        assert_eq!(home.to_path().get(), "/home/u");
    }

    #[test]
    fn parent_drops_the_last_component_and_stops_at_the_root() {
        testing::table(
            &[
                ("/", "/".to_owned()),
                ("/a", "/".to_owned()),
                ("/a/b", "/a".to_owned()),
                ("/a/b/c", "/a/b".to_owned()),
            ],
            |text| {
                let parent = AbsPath::parse((*text).to_owned()).unwrap().parent();
                assert_eq!(AbsDir::parse(parent.get().clone()), Ok(parent.clone()));
                parent.into_inner()
            },
        );
        assert_eq!(AbsDir::root(), dir("/"));
    }

    #[test]
    fn paths_show_compare_and_expose_their_text() {
        let (a, b) = (dir("/a"), dir("/b"));
        assert!(a < b);
        assert_eq!(format!("{a:?} {a}"), "\"/a\" /a");
        assert_eq!(a.as_ref(), "/a");
    }

    /// `text` is one component: what [`Segment`] implementations must guarantee.
    pub(crate) fn is_segment(text: &str) -> bool {
        !text.contains(['/', '\0']) && first_flaw(text).is_none()
    }

    #[test]
    fn normalising_drops_empty_and_dot_components_but_refuses_dot_dot() {
        testing::table(
            &[
                ("/", Ok("/".to_owned())),
                ("//", Ok("/".to_owned())),
                ("/home/u/", Ok("/home/u".to_owned())),
                ("/home//u/./", Ok("/home/u".to_owned())),
                ("/./a", Ok("/a".to_owned())),
                ("/a/..b", Ok("/a/..b".to_owned())),
                ("/a/../b", Err(AbsPathError::ParentDir { index: 1 })),
                ("//a/..", Err(AbsPathError::ParentDir { index: 1 })),
                ("/a\0", Err(AbsPathError::NulByte { at: 2 })),
                ("", Err(AbsPathError::Empty)),
                ("a/", Err(AbsPathError::NotAbsolute)),
            ],
            |text| AbsDir::normalising(text).map(AbsDir::into_inner),
        );
    }

    #[test]
    fn accepted_paths_parse_back_to_themselves() {
        for raw in ["/", "/a", "/home/u/dev"] {
            let path = AbsPath::parse(raw.to_owned()).unwrap();
            assert_eq!(AbsPath::parse(path.get().clone()), Ok(path));
        }
    }
}
