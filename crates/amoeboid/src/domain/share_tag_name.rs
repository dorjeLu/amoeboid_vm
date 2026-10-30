//! `ShareTagName`: the virtiofs tag of a share as the build's contract spells it (`amoeboid`,
//! `ro-store`). Not [`ShareTag`](super::ShareTag), which is the closed `dN` family of the shared
//! directories.

use crate::diagnostic::Diagnostic;
use crate::domain::file_name::is_portable;
use crate::refined::{CheckedStr, Refined, Refinement};

/// The virtio-fs specification allows 36 bytes of tag.
pub const MAX_LEN: usize = 36;

/// Marker of a share tag name: 1 to 36 ASCII letters, digits, `.`, `_` and `-`.
pub struct ShareTagNameShape;

/// A share tag proven to be a name the guest can mount by.
pub type ShareTagName = Refined<String, ShareTagNameShape>;

#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum ShareTagNameError {
    #[error("bad share tag: it is empty")]
    Empty,
    #[error("bad share tag: {len} bytes is longer than {MAX_LEN}")]
    TooLong { len: usize },
    /// `at` is a byte offset.
    #[error(
        "bad share tag: character at offset {at} isn't an ASCII letter, digit, '.', '_' or '-'"
    )]
    BadChar { at: usize },
}

impl Diagnostic for ShareTagNameError {
    fn code(&self) -> &'static str {
        match self {
            Self::Empty => "share-tag-name.empty",
            Self::TooLong { .. } => "share-tag-name.too-long",
            Self::BadChar { .. } => "share-tag-name.bad-char",
        }
    }

    fn message(&self) -> String {
        self.to_string()
    }

    fn hint(&self) -> Option<String> {
        Some(format!(
            "use up to {MAX_LEN} ASCII letters, digits, '.', '_' or '-' for the tag"
        ))
    }
}

impl ShareTagNameShape {
    /// Compile-time twin of [`Refinement::check`], for `refined!`.
    #[must_use]
    pub const fn check_const(value: &'static str) -> Option<CheckedStr<Self>> {
        let mut rest = value.as_bytes();
        if rest.is_empty() || rest.len() > MAX_LEN {
            return None;
        }
        while let [byte, tail @ ..] = rest {
            if !is_portable(*byte) {
                return None;
            }
            rest = tail;
        }
        Some(CheckedStr::new(value))
    }
}

impl Refinement<String> for ShareTagNameShape {
    type Error = ShareTagNameError;

    fn check(value: &String) -> Result<(), ShareTagNameError> {
        if value.is_empty() {
            return Err(ShareTagNameError::Empty);
        }
        if let Some(at) = value.bytes().position(|byte| !is_portable(byte)) {
            return Err(ShareTagNameError::BadChar { at });
        }
        if value.len() > MAX_LEN {
            return Err(ShareTagNameError::TooLong { len: value.len() });
        }
        Ok(())
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) mod tests {
    use super::*;
    use crate::testing::{self, Entry};

    fn witness(e: &ShareTagNameError) -> &'static str {
        match e {
            ShareTagNameError::Empty => "Empty",
            ShareTagNameError::TooLong { .. } => "TooLong",
            ShareTagNameError::BadChar { .. } => "BadChar",
        }
    }

    fn errors() -> Vec<ShareTagNameError> {
        vec![
            ShareTagNameError::Empty,
            ShareTagNameError::TooLong { len: 37 },
            ShareTagNameError::BadChar { at: 1 },
        ]
    }

    /// One catalogue entry per variant.
    pub(crate) fn entries() -> Vec<Entry> {
        errors().iter().map(Entry::of).collect()
    }

    #[test]
    fn every_error_variant_has_a_table_case() {
        testing::every_variant(&errors(), witness, &["Empty", "TooLong", "BadChar"]);
    }

    const EDGES: &[&str] = &[
        "",
        "a",
        "amoeboid",
        "ro-store",
        "A.b_c-9",
        "a b",
        "a/b",
        "日本",
        "a\0",
        "a\t",
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    ];

    #[test]
    fn check_const_agrees_with_check_at_every_edge() {
        for text in EDGES {
            assert_eq!(
                ShareTagNameShape::check_const(text).is_some(),
                ShareTagNameShape::check(&(*text).to_owned()).is_ok(),
                "text {text:?}"
            );
        }
    }

    #[test]
    fn refined_constants_of_share_tag_names_are_the_parsed_values() {
        assert_eq!(
            ShareTagName::parse("ro-store".to_owned()),
            Ok(crate::refined!(ShareTagName, "ro-store"))
        );
    }

    #[test]
    fn share_tag_name_parse_accepts_up_to_36_portable_bytes_and_names_the_violated_edge() {
        testing::table(
            &[
                ("", Err(ShareTagNameError::Empty)),
                ("a", Ok(())),
                ("amoeboid", Ok(())),
                ("ro-store", Ok(())),
                ("d1", Ok(())),
                ("A.b_c-9", Ok(())),
                (&"a".repeat(36), Ok(())),
                (&"a".repeat(37), Err(ShareTagNameError::TooLong { len: 37 })),
                ("a b", Err(ShareTagNameError::BadChar { at: 1 })),
                ("a/b", Err(ShareTagNameError::BadChar { at: 1 })),
                ("日本", Err(ShareTagNameError::BadChar { at: 0 })),
                ("a\0", Err(ShareTagNameError::BadChar { at: 1 })),
                ("a\t", Err(ShareTagNameError::BadChar { at: 1 })),
                // A bad character is reported before the length.
                (
                    &format!("{} ", "a".repeat(40)),
                    Err(ShareTagNameError::BadChar { at: 40 }),
                ),
            ],
            |text| ShareTagName::parse((*text).to_owned()).map(|_| ()),
        );
    }
}
