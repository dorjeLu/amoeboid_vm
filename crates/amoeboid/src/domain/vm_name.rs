//! `VmName`: `[A-Za-z0-9][A-Za-z0-9-]{0,39}`; also the guest hostname.

use crate::diagnostic::Diagnostic;
use crate::domain::abs_path::{Segment, sealed};
use crate::domain::flake_path::FlakeSegment;
use crate::domain::scan::{Misfit, scan};
use crate::refined::{Refined, Refinement};

/// Longest VM name; keeps the longest socket path inside `sun_path` (design §Runtime dir).
pub const MAX_LEN: usize = 40;

/// Marker of the VM name grammar.
pub struct VmNameGrammar;

/// A VM name; proven to satisfy [`VmNameGrammar`].
pub type VmName = Refined<String, VmNameGrammar>;

#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum VmNameError {
    #[error("bad VM name: it is empty")]
    Empty,
    #[error("bad VM name: {len} characters, at most {MAX_LEN} allowed")]
    TooLong { len: usize },
    #[error("bad VM name: {ch:?} at position {at} isn't a letter, digit or '-'")]
    BadChar { ch: char, at: usize },
    #[error("bad VM name: it starts with {ch:?}, not a letter or digit")]
    BadStart { ch: char },
}

impl Diagnostic for VmNameError {
    fn code(&self) -> &'static str {
        match self {
            Self::Empty => "vm-name.empty",
            Self::TooLong { .. } => "vm-name.too-long",
            Self::BadChar { .. } => "vm-name.bad-char",
            Self::BadStart { .. } => "vm-name.bad-start",
        }
    }

    fn message(&self) -> String {
        self.to_string()
    }

    fn hint(&self) -> Option<String> {
        match self {
            Self::TooLong { .. } => Some(format!(
                "use at most {MAX_LEN} characters; to keep an existing VM, rename its directory in the data dir"
            )),
            Self::Empty | Self::BadChar { .. } | Self::BadStart { .. } => Some(format!(
                "use letters, digits and '-', starting with a letter or digit (max {MAX_LEN} chars)"
            )),
        }
    }
}

impl Refinement<String> for VmNameGrammar {
    type Error = VmNameError;

    fn check(value: &String) -> Result<(), VmNameError> {
        scan(
            value,
            |c| c.is_ascii_alphanumeric(),
            |c| c.is_ascii_alphanumeric() || c == '-',
        )
        .map_err(|misfit| match misfit {
            Misfit::Empty => VmNameError::Empty,
            Misfit::BadStart { ch } => VmNameError::BadStart { ch },
            Misfit::BadChar { ch, at } => VmNameError::BadChar { ch, at },
        })?;
        // Every char is ASCII by now, so bytes are chars.
        if value.len() > MAX_LEN {
            return Err(VmNameError::TooLong { len: value.len() });
        }
        Ok(())
    }
}

/// A VM name is one component: the grammar has no `/`, NUL or `.`.
impl sealed::Sealed for VmName {}

impl Segment for VmName {
    fn segment(&self) -> &str {
        self.get()
    }
}

/// Letters, digits and `-` only.
impl FlakeSegment for VmName {}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) mod tests {
    use proptest::prelude::*;

    use super::*;
    use crate::testing::{self, Entry};

    fn witness(e: &VmNameError) -> &'static str {
        match e {
            VmNameError::Empty => "Empty",
            VmNameError::TooLong { .. } => "TooLong",
            VmNameError::BadChar { .. } => "BadChar",
            VmNameError::BadStart { .. } => "BadStart",
        }
    }

    fn errors() -> Vec<VmNameError> {
        vec![
            VmNameError::Empty,
            VmNameError::TooLong { len: 41 },
            VmNameError::BadChar { ch: '_', at: 3 },
            VmNameError::BadStart { ch: '-' },
        ]
    }

    /// One catalogue entry per variant.
    pub(crate) fn entries() -> Vec<Entry> {
        errors().iter().map(Entry::of).collect()
    }

    #[test]
    fn every_vm_name_is_one_path_component() {
        let longest = "z".repeat(MAX_LEN);
        for raw in ["a", "A9", "proj-1", "a--", longest.as_str()] {
            let name = VmName::parse(raw.to_owned()).unwrap();
            assert!(
                crate::domain::abs_path::tests::is_segment(name.segment()),
                "{raw}"
            );
        }
    }

    #[test]
    fn every_error_variant_has_a_table_case() {
        testing::every_variant(
            &errors(),
            witness,
            &["Empty", "TooLong", "BadChar", "BadStart"],
        );
    }

    #[test]
    fn vm_name_parse_accepts_the_grammar_and_names_the_violated_edge() {
        let max = "a".repeat(MAX_LEN);
        let over = "a".repeat(41);
        testing::table(
            &[
                ("", Err(VmNameError::Empty)),
                ("a", Ok(())),
                ("0", Ok(())),
                ("Z", Ok(())),
                ("a-", Ok(())),
                ("a-b-C-9", Ok(())),
                (max.as_str(), Ok(())),
                (over.as_str(), Err(VmNameError::TooLong { len: 41 })),
                ("-a", Err(VmNameError::BadStart { ch: '-' })),
                ("_a", Err(VmNameError::BadStart { ch: '_' })),
                (".a", Err(VmNameError::BadStart { ch: '.' })),
                (" a", Err(VmNameError::BadStart { ch: ' ' })),
                ("éa", Err(VmNameError::BadStart { ch: 'é' })),
                ("a_b", Err(VmNameError::BadChar { ch: '_', at: 1 })),
                ("ab.c", Err(VmNameError::BadChar { ch: '.', at: 2 })),
                ("a b", Err(VmNameError::BadChar { ch: ' ', at: 1 })),
                ("a/b", Err(VmNameError::BadChar { ch: '/', at: 1 })),
                ("a\n", Err(VmNameError::BadChar { ch: '\n', at: 1 })),
                ("aé", Err(VmNameError::BadChar { ch: 'é', at: 1 })),
                ("a٣", Err(VmNameError::BadChar { ch: '٣', at: 1 })),
            ],
            |text| VmName::parse((*text).to_owned()).map(|_| ()),
        );
    }

    #[test]
    fn a_name_that_is_both_too_long_and_malformed_reports_the_character() {
        let text = format!("{}_", "a".repeat(MAX_LEN));
        assert_eq!(
            VmName::parse(text),
            Err(VmNameError::BadChar { ch: '_', at: 40 })
        );
    }

    #[test]
    fn accepted_names_parse_back_to_themselves() {
        for raw in ["a", "proj-1", "A9"] {
            let name = VmName::parse(raw.to_owned()).unwrap();
            assert_eq!(VmName::parse(name.get().clone()), Ok(name));
        }
    }

    /// The design grammar, written independently of the implementation.
    fn grammar(text: &str) -> bool {
        match text.chars().collect::<Vec<_>>().split_first() {
            Some((first, rest)) => {
                first.is_ascii_alphanumeric()
                    && rest.iter().all(|c| c.is_ascii_alphanumeric() || *c == '-')
                    && rest.len() < MAX_LEN
            }
            None => false,
        }
    }

    proptest! {
        #[test]
        fn strings_of_the_grammar_are_accepted(text in "[A-Za-z0-9][A-Za-z0-9-]{0,39}") {
            prop_assert!(VmName::parse(text).is_ok());
        }

        #[test]
        fn parse_accepts_exactly_the_grammar_among_near_misses(
            text in "[A-Za-z0-9_. /é-]{0,45}"
        ) {
            prop_assert_eq!(VmName::parse(text.clone()).is_ok(), grammar(&text), "{:?}", text);
        }

        #[test]
        fn one_bad_char_or_one_char_too_many_is_refused(
            text in "[A-Za-z0-9][A-Za-z0-9-]{0,39}",
            bad in "[_. /é]",
        ) {
            let (after, before, twice) =
                (format!("{text}{bad}"), format!("{bad}{text}"), text.repeat(2));
            prop_assert!(VmName::parse(after).is_err());
            prop_assert!(VmName::parse(before).is_err());
            prop_assert_eq!(VmName::parse(twice).is_err(), text.len() > 20);
        }
    }
}
