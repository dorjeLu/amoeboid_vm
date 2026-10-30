//! `StateFileName`: the name of a file in the state share or in the runtime dir as the build's
//! contract spells it (`mounts`, `amoeboid-qemu-args`). One path component of ASCII letters,
//! digits, `.`, `_` and `-`, like [`FileName`](super::FileName), but read from outside.

use crate::diagnostic::Diagnostic;
use crate::domain::abs_path::{Segment, sealed};
use crate::domain::file_name::is_portable;
use crate::refined::{CheckedStr, Refined, Refinement};

/// Marker of one portable path component, as for `FileName`.
pub struct StateFileNameShape;

/// A file name proven to be one portable path component.
pub type StateFileName = Refined<String, StateFileNameShape>;

#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum StateFileNameError {
    #[error("bad state file name: it is empty")]
    Empty,
    /// `at` is a byte offset.
    #[error(
        "bad state file name: character at offset {at} isn't an ASCII letter, digit, '.', '_' or '-'"
    )]
    BadChar { at: usize },
    #[error("bad state file name: '.' and '..' name no entry of their own")]
    Dots,
}

impl Diagnostic for StateFileNameError {
    fn code(&self) -> &'static str {
        match self {
            Self::Empty => "state-file-name.empty",
            Self::BadChar { .. } => "state-file-name.bad-char",
            Self::Dots => "state-file-name.dots",
        }
    }

    fn message(&self) -> String {
        self.to_string()
    }

    fn hint(&self) -> Option<String> {
        Some("use ASCII letters, digits, '.', '_' and '-', other than '.' and '..'".to_owned())
    }
}

impl StateFileNameShape {
    /// Compile-time twin of [`Refinement::check`], for `refined!`.
    #[must_use]
    pub const fn check_const(value: &'static str) -> Option<CheckedStr<Self>> {
        let mut rest = value.as_bytes();
        if matches!(rest, [] | [b'.'] | [b'.', b'.']) {
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

impl Refinement<String> for StateFileNameShape {
    type Error = StateFileNameError;

    fn check(value: &String) -> Result<(), StateFileNameError> {
        match value.as_str() {
            "" => Err(StateFileNameError::Empty),
            "." | ".." => Err(StateFileNameError::Dots),
            text => match text.bytes().position(|byte| !is_portable(byte)) {
                Some(at) => Err(StateFileNameError::BadChar { at }),
                None => Ok(()),
            },
        }
    }
}

/// Portable characters only, and not `.` or `..`: one path component.
impl sealed::Sealed for StateFileName {}

impl Segment for StateFileName {
    fn segment(&self) -> &str {
        self.get()
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) mod tests {
    use super::*;
    use crate::testing::{self, Entry};

    fn witness(e: &StateFileNameError) -> &'static str {
        match e {
            StateFileNameError::Empty => "Empty",
            StateFileNameError::BadChar { .. } => "BadChar",
            StateFileNameError::Dots => "Dots",
        }
    }

    fn errors() -> Vec<StateFileNameError> {
        vec![
            StateFileNameError::Empty,
            StateFileNameError::BadChar { at: 1 },
            StateFileNameError::Dots,
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

    #[test]
    fn every_state_file_name_is_one_path_component() {
        for raw in ["mounts", "amoeboid-qemu-args", "...", "a.b_c-d"] {
            let name = StateFileName::parse(raw.to_owned()).unwrap();
            assert!(
                crate::domain::abs_path::tests::is_segment(name.segment()),
                "{raw}"
            );
        }
    }

    #[test]
    fn check_const_agrees_with_check_at_every_edge() {
        for text in [
            "", ".", "..", "...", "mounts", "a b", "../x", "a/b", "a\0", "日本", "x.",
        ] {
            assert_eq!(
                StateFileNameShape::check_const(text).is_some(),
                StateFileNameShape::check(&text.to_owned()).is_ok(),
                "text {text:?}"
            );
        }
    }

    #[test]
    fn refined_constants_of_state_file_names_are_the_parsed_values() {
        assert_eq!(
            StateFileName::parse("mounts".to_owned()),
            Ok(crate::refined!(StateFileName, "mounts"))
        );
    }

    #[test]
    fn state_file_name_parse_accepts_portable_components_and_names_the_violated_edge() {
        testing::table(
            &[
                ("", Err(StateFileNameError::Empty)),
                (".", Err(StateFileNameError::Dots)),
                ("..", Err(StateFileNameError::Dots)),
                ("...", Ok(())),
                ("mounts", Ok(())),
                ("amoeboid-qemu-args", Ok(())),
                ("ssh_host_ed25519_key", Ok(())),
                ("a b", Err(StateFileNameError::BadChar { at: 1 })),
                ("../x", Err(StateFileNameError::BadChar { at: 2 })),
                ("a/b", Err(StateFileNameError::BadChar { at: 1 })),
                ("a\0", Err(StateFileNameError::BadChar { at: 1 })),
                ("日本", Err(StateFileNameError::BadChar { at: 0 })),
            ],
            |text| StateFileName::parse((*text).to_owned()).map(|_| ()),
        );
    }
}
