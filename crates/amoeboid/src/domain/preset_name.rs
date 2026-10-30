//! `PresetName`: `[A-Za-z0-9][A-Za-z0-9._-]*`; a directory name under `nix_presets/`.

use crate::diagnostic::Diagnostic;
use crate::domain::abs_path::{Segment, sealed};
use crate::domain::scan::{Misfit, scan};
use crate::refined::{CheckedStr, Refined, Refinement};

/// Marker of the preset name grammar.
pub struct PresetNameGrammar;

/// A preset name; proven to satisfy [`PresetNameGrammar`]. It is never `.` or `..`, nor does it
/// contain `/`, so it names exactly one entry of the presets directory.
pub type PresetName = Refined<String, PresetNameGrammar>;

#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum PresetNameError {
    #[error("bad preset name: it is empty")]
    Empty,
    #[error("bad preset name: {ch:?} at position {at} isn't a letter, digit, '.', '_' or '-'")]
    BadChar { ch: char, at: usize },
    #[error("bad preset name: it starts with {ch:?}, not a letter or digit")]
    BadStart { ch: char },
}

impl Diagnostic for PresetNameError {
    fn code(&self) -> &'static str {
        match self {
            Self::Empty => "preset-name.empty",
            Self::BadChar { .. } => "preset-name.bad-char",
            Self::BadStart { .. } => "preset-name.bad-start",
        }
    }

    fn message(&self) -> String {
        self.to_string()
    }

    fn hint(&self) -> Option<String> {
        Some(
            "use letters, digits, '.', '_' and '-', starting with a letter or digit; \
             give a path (with '/') to use a directory"
                .to_owned(),
        )
    }
}

impl PresetNameGrammar {
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

/// The grammar as a `const fn`: an ASCII letter or digit, then letters, digits, `.`, `_`, `-`.
/// Any byte of a multi-byte char is outside both classes.
const fn fits(text: &str) -> bool {
    let [first, rest @ ..] = text.as_bytes() else {
        return false;
    };
    if !first.is_ascii_alphanumeric() {
        return false;
    }
    let mut rest = rest;
    while let [byte, tail @ ..] = rest {
        if !(byte.is_ascii_alphanumeric() || matches!(*byte, b'.' | b'_' | b'-')) {
            return false;
        }
        rest = tail;
    }
    true
}

impl Refinement<String> for PresetNameGrammar {
    type Error = PresetNameError;

    fn check(value: &String) -> Result<(), PresetNameError> {
        scan(
            value,
            |c| c.is_ascii_alphanumeric(),
            |c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'),
        )
        .map_err(|misfit| match misfit {
            Misfit::Empty => PresetNameError::Empty,
            Misfit::BadStart { ch } => PresetNameError::BadStart { ch },
            Misfit::BadChar { ch, at } => PresetNameError::BadChar { ch, at },
        })
    }
}

/// A preset name is one component: the grammar has no `/`, NUL, and starts with a letter or digit,
/// so it is neither `.` nor `..`.
impl sealed::Sealed for PresetName {}

impl Segment for PresetName {
    fn segment(&self) -> &str {
        self.get()
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) mod tests {
    use proptest::prelude::*;

    use super::*;
    use crate::refined;
    use crate::testing::{self, Entry};

    fn witness(e: &PresetNameError) -> &'static str {
        match e {
            PresetNameError::Empty => "Empty",
            PresetNameError::BadChar { .. } => "BadChar",
            PresetNameError::BadStart { .. } => "BadStart",
        }
    }

    fn errors() -> Vec<PresetNameError> {
        vec![
            PresetNameError::Empty,
            PresetNameError::BadChar { ch: '/', at: 1 },
            PresetNameError::BadStart { ch: '.' },
        ]
    }

    /// One catalogue entry per variant.
    pub(crate) fn entries() -> Vec<Entry> {
        errors().iter().map(Entry::of).collect()
    }

    #[test]
    fn every_error_variant_has_a_table_case() {
        testing::every_variant(&errors(), witness, &["Empty", "BadChar", "BadStart"]);
    }

    #[test]
    fn preset_name_parse_accepts_the_grammar_and_names_the_violated_edge() {
        let long = "a".repeat(300);
        testing::table(
            &[
                ("", Err(PresetNameError::Empty)),
                ("default", Ok(())),
                ("0", Ok(())),
                ("Z", Ok(())),
                ("a.b_c-d", Ok(())),
                ("v1.2", Ok(())),
                ("a..", Ok(())),
                (long.as_str(), Ok(())),
                (".", Err(PresetNameError::BadStart { ch: '.' })),
                ("..", Err(PresetNameError::BadStart { ch: '.' })),
                (".hidden", Err(PresetNameError::BadStart { ch: '.' })),
                ("_a", Err(PresetNameError::BadStart { ch: '_' })),
                ("-a", Err(PresetNameError::BadStart { ch: '-' })),
                ("~a", Err(PresetNameError::BadStart { ch: '~' })),
                ("é", Err(PresetNameError::BadStart { ch: 'é' })),
                ("a/b", Err(PresetNameError::BadChar { ch: '/', at: 1 })),
                ("a b", Err(PresetNameError::BadChar { ch: ' ', at: 1 })),
                ("ab~", Err(PresetNameError::BadChar { ch: '~', at: 2 })),
                ("aé", Err(PresetNameError::BadChar { ch: 'é', at: 1 })),
                ("a\n", Err(PresetNameError::BadChar { ch: '\n', at: 1 })),
            ],
            |text| PresetName::parse((*text).to_owned()).map(|_| ()),
        );
    }

    #[test]
    fn every_preset_name_is_one_path_component() {
        for raw in ["default", "0", "a.b_c-d", "a..", "v1.2"] {
            let name = PresetName::parse(raw.to_owned()).unwrap();
            assert!(
                crate::domain::abs_path::tests::is_segment(name.segment()),
                "{raw}"
            );
        }
    }

    #[test]
    fn check_const_agrees_with_check_at_every_edge() {
        for text in [
            "", "default", "0", "Z", "a.b_c-d", "a..", ".", "..", ".a", "_a", "-a", "é", "a/b",
            "a b", "aé", "a\n", "a\0",
        ] {
            assert_eq!(
                PresetNameGrammar::check_const(text).is_some(),
                PresetNameGrammar::check(&text.to_owned()).is_ok(),
                "text {text:?}"
            );
        }
    }

    #[test]
    fn refined_constants_of_preset_names_are_the_parsed_values() {
        assert_eq!(
            PresetName::parse("default".to_owned()),
            Ok(refined!(PresetName, "default"))
        );
        assert_eq!(
            PresetName::parse("v1.2_x-y".to_owned()),
            Ok(refined!(PresetName, "v1.2_x-y"))
        );
    }

    #[test]
    fn accepted_names_parse_back_to_themselves() {
        for raw in ["default", "a.b", "X_1-2"] {
            let name = PresetName::parse(raw.to_owned()).unwrap();
            assert_eq!(PresetName::parse(name.get().clone()), Ok(name));
        }
    }

    /// The design grammar, written independently of the implementation.
    fn grammar(text: &str) -> bool {
        match text.chars().collect::<Vec<_>>().split_first() {
            Some((first, rest)) => {
                first.is_ascii_alphanumeric()
                    && rest
                        .iter()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
            }
            None => false,
        }
    }

    proptest! {
        #[test]
        fn strings_of_the_grammar_are_accepted(text in "[A-Za-z0-9][A-Za-z0-9._-]{0,60}") {
            prop_assert!(PresetName::parse(text).is_ok());
        }

        #[test]
        fn the_const_grammar_agrees_with_check_among_near_misses(
            text in "[A-Za-z0-9_. /~é-]{0,12}"
        ) {
            prop_assert_eq!(fits(&text), PresetName::parse(text.clone()).is_ok(), "{:?}", text);
        }

        #[test]
        fn parse_accepts_exactly_the_grammar_among_near_misses(
            text in "[A-Za-z0-9_. /~é-]{0,12}"
        ) {
            prop_assert_eq!(PresetName::parse(text.clone()).is_ok(), grammar(&text), "{:?}", text);
        }
    }
}
