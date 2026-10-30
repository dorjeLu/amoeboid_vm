//! `FileName`: a fixed name of one path component (`lock`, `run_config.kdl`, ...), written as a
//! `refined!` constant and joined onto directories as a [`Segment`].
//!
//! Only ASCII letters, digits, `.`, `_` and `-` are allowed. Names that restricted can't hold
//! `/`, NUL or any character a URL gives a meaning to, so a flake path joined with one stays a
//! valid `path:` URL path (it is a [`FlakeSegment`]).

use crate::diagnostic::Diagnostic;
use crate::domain::abs_path::{Segment, sealed};
use crate::domain::flake_path::FlakeSegment;
use crate::refined::{Checked, Refined, Refinement};

/// Marker of one portable path component: non-empty, ASCII letters, digits, `.`, `_` and `-`,
/// not `.` or `..`.
pub struct FileNameShape;

/// A literal name proven to be one portable path component.
pub type FileName = Refined<&'static str, FileNameShape>;

#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum FileNameError {
    #[error("bad file name: it is empty")]
    Empty,
    /// `at` is a byte offset.
    #[error(
        "bad file name: character at offset {at} isn't an ASCII letter, digit, '.', '_' or '-'"
    )]
    BadChar { at: usize },
    #[error("bad file name: '.' and '..' name no entry of their own")]
    Dots,
}

impl Diagnostic for FileNameError {
    fn code(&self) -> &'static str {
        match self {
            Self::Empty => "file-name.empty",
            Self::BadChar { .. } => "file-name.bad-char",
            Self::Dots => "file-name.dots",
        }
    }

    fn message(&self) -> String {
        self.to_string()
    }

    fn hint(&self) -> Option<String> {
        Some("use ASCII letters, digits, '.', '_' and '-', other than '.' and '..'".to_owned())
    }
}

impl FileNameShape {
    /// Compile-time twin of [`Refinement::check`], for `refined!`.
    #[must_use]
    pub const fn check_const(value: &'static str) -> Option<Checked<&'static str, Self>> {
        let bytes = value.as_bytes();
        if matches!(bytes, [] | [b'.'] | [b'.', b'.']) {
            return None;
        }
        let mut rest = bytes;
        while let [byte, tail @ ..] = rest {
            if !is_portable(*byte) {
                return None;
            }
            rest = tail;
        }
        Some(Checked::new(value))
    }
}

pub(crate) const fn is_portable(byte: u8) -> bool {
    matches!(byte, b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'.' | b'_' | b'-')
}

impl Refinement<&'static str> for FileNameShape {
    type Error = FileNameError;

    fn check(value: &&'static str) -> Result<(), FileNameError> {
        match *value {
            "" => Err(FileNameError::Empty),
            "." | ".." => Err(FileNameError::Dots),
            text => match text.bytes().position(|byte| !is_portable(byte)) {
                Some(at) => Err(FileNameError::BadChar { at }),
                None => Ok(()),
            },
        }
    }
}

impl sealed::Sealed for FileName {}

impl Segment for FileName {
    fn segment(&self) -> &str {
        self.get()
    }
}

/// The alphabet has no `?`, `#` or `%`.
impl FlakeSegment for FileName {}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) mod tests {
    use super::*;
    use crate::domain::abs_path::tests::is_segment;
    use crate::refined;
    use crate::testing::{self, Entry};

    fn witness(e: &FileNameError) -> &'static str {
        match e {
            FileNameError::Empty => "Empty",
            FileNameError::BadChar { .. } => "BadChar",
            FileNameError::Dots => "Dots",
        }
    }

    fn errors() -> Vec<FileNameError> {
        vec![
            FileNameError::Empty,
            FileNameError::BadChar { at: 1 },
            FileNameError::Dots,
        ]
    }

    /// One catalogue entry per variant.
    pub(crate) fn entries() -> Vec<Entry> {
        errors().iter().map(Entry::of).collect()
    }

    #[test]
    fn every_error_variant_has_a_table_case() {
        testing::every_variant(&errors(), witness, &["Empty", "BadChar", "Dots"]);
    }

    const EDGES: &[(&str, Result<(), FileNameError>)] = &[
        ("", Err(FileNameError::Empty)),
        (".", Err(FileNameError::Dots)),
        ("..", Err(FileNameError::Dots)),
        ("...", Ok(())),
        (".x", Ok(())),
        ("x.", Ok(())),
        ("lock", Ok(())),
        ("run_config.kdl", Ok(())),
        ("A-Z_0-9", Ok(())),
        ("a b", Err(FileNameError::BadChar { at: 1 })),
        ("日本", Err(FileNameError::BadChar { at: 0 })),
        ("/", Err(FileNameError::BadChar { at: 0 })),
        ("a/b", Err(FileNameError::BadChar { at: 1 })),
        ("\0", Err(FileNameError::BadChar { at: 0 })),
        ("ab?", Err(FileNameError::BadChar { at: 2 })),
        ("a#", Err(FileNameError::BadChar { at: 1 })),
        ("%41", Err(FileNameError::BadChar { at: 0 })),
        ("ok\n", Err(FileNameError::BadChar { at: 2 })),
    ];

    #[test]
    fn file_name_parse_accepts_portable_components_and_names_the_violated_edge() {
        testing::table(EDGES, |text| FileName::parse(text).map(|_| ()));
    }

    #[test]
    fn check_const_agrees_with_check_at_every_edge() {
        for (text, _) in EDGES {
            assert_eq!(
                FileNameShape::check_const(text).is_some(),
                FileNameShape::check(text).is_ok(),
                "text {text:?}"
            );
        }
    }

    #[test]
    fn accepted_names_are_segments() {
        for (text, expected) in EDGES {
            // The predicate is stricter than one component, so it implies it.
            if expected.is_ok() {
                assert!(is_segment(text), "text {text:?}");
            }
        }
        let lock: FileName = refined!(FileName, "lock");
        assert_eq!(lock.segment(), "lock");
        assert_eq!(FileName::parse("lock"), Ok(lock));
    }
}
