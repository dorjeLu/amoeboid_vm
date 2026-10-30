//! `UserName`: `[a-z_][a-z0-9_-]{0,31}`, not `root`; the guest account, taken from the host user.

use crate::diagnostic::Diagnostic;
use crate::domain::abs_path::{Segment, sealed};
use crate::domain::scan::{Misfit, scan};
use crate::refined::{Refined, Refinement};

/// Longest account name (the classic `useradd` limit).
pub const MAX_LEN: usize = 32;

/// Marker of the account name grammar.
pub struct UserNameGrammar;

/// An account name usable in the guest; proven to satisfy [`UserNameGrammar`].
pub type UserName = Refined<String, UserNameGrammar>;

#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum UserNameError {
    #[error("bad account name: it is empty")]
    Empty,
    #[error("bad account name: {len} characters, at most {MAX_LEN} allowed")]
    TooLong { len: usize },
    #[error(
        "bad account name: {ch:?} at position {at} isn't a lowercase letter, digit, '_' or '-'"
    )]
    BadChar { ch: char, at: usize },
    #[error("bad account name: it starts with {ch:?}, not a lowercase letter or '_'")]
    BadStart { ch: char },
    #[error("bad account name: root can't be used in the guest")]
    Root,
}

impl Diagnostic for UserNameError {
    fn code(&self) -> &'static str {
        match self {
            Self::Empty => "user-name.empty",
            Self::TooLong { .. } => "user-name.too-long",
            Self::BadChar { .. } => "user-name.bad-char",
            Self::BadStart { .. } => "user-name.bad-start",
            Self::Root => "user-name.root",
        }
    }

    fn message(&self) -> String {
        self.to_string()
    }

    fn hint(&self) -> Option<String> {
        match self {
            Self::Root => Some("run amoeboid as an unprivileged user".to_owned()),
            Self::Empty | Self::TooLong { .. } | Self::BadChar { .. } | Self::BadStart { .. } => {
                Some(format!(
                    "use lowercase letters, digits, '_' and '-', not starting with a digit or '-' (max {MAX_LEN} chars)"
                ))
            }
        }
    }
}

impl Refinement<String> for UserNameGrammar {
    type Error = UserNameError;

    fn check(value: &String) -> Result<(), UserNameError> {
        scan(
            value,
            |c| c.is_ascii_lowercase() || c == '_',
            |c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-',
        )
        .map_err(|misfit| match misfit {
            Misfit::Empty => UserNameError::Empty,
            Misfit::BadStart { ch } => UserNameError::BadStart { ch },
            Misfit::BadChar { ch, at } => UserNameError::BadChar { ch, at },
        })?;
        // Every char is ASCII by now, so bytes are chars.
        if value.len() > MAX_LEN {
            return Err(UserNameError::TooLong { len: value.len() });
        }
        if value == "root" {
            return Err(UserNameError::Root);
        }
        Ok(())
    }
}

/// An account name is one component: the grammar has no `/`, NUL or `.`.
impl sealed::Sealed for UserName {}

impl Segment for UserName {
    fn segment(&self) -> &str {
        self.get()
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) mod tests {
    use proptest::prelude::*;

    use super::*;
    use crate::testing::{self, Entry};

    fn witness(e: &UserNameError) -> &'static str {
        match e {
            UserNameError::Empty => "Empty",
            UserNameError::TooLong { .. } => "TooLong",
            UserNameError::BadChar { .. } => "BadChar",
            UserNameError::BadStart { .. } => "BadStart",
            UserNameError::Root => "Root",
        }
    }

    fn errors() -> Vec<UserNameError> {
        vec![
            UserNameError::Empty,
            UserNameError::TooLong { len: 33 },
            UserNameError::BadChar { ch: 'A', at: 2 },
            UserNameError::BadStart { ch: '1' },
            UserNameError::Root,
        ]
    }

    /// One catalogue entry per variant.
    pub(crate) fn entries() -> Vec<Entry> {
        errors().iter().map(Entry::of).collect()
    }

    #[test]
    fn every_user_name_is_one_path_component() {
        let longest = "z".repeat(MAX_LEN);
        for raw in ["a", "_", "alice", "svc-1_x", "a--", longest.as_str()] {
            let name = UserName::parse(raw.to_owned()).unwrap();
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
            &["Empty", "TooLong", "BadChar", "BadStart", "Root"],
        );
    }

    #[test]
    fn user_name_parse_accepts_the_grammar_and_names_the_violated_edge() {
        let max = format!("a{}", "b".repeat(31));
        let over = format!("a{}", "b".repeat(32));
        testing::table(
            &[
                ("", Err(UserNameError::Empty)),
                ("a", Ok(())),
                ("_", Ok(())),
                ("magos", Ok(())),
                ("_a-b_9", Ok(())),
                ("a-", Ok(())),
                ("root", Err(UserNameError::Root)),
                ("roots", Ok(())),
                ("Root", Err(UserNameError::BadStart { ch: 'R' })),
                (max.as_str(), Ok(())),
                (over.as_str(), Err(UserNameError::TooLong { len: 33 })),
                ("1a", Err(UserNameError::BadStart { ch: '1' })),
                ("-a", Err(UserNameError::BadStart { ch: '-' })),
                ("A", Err(UserNameError::BadStart { ch: 'A' })),
                ("é", Err(UserNameError::BadStart { ch: 'é' })),
                ("aB", Err(UserNameError::BadChar { ch: 'B', at: 1 })),
                ("a.b", Err(UserNameError::BadChar { ch: '.', at: 1 })),
                ("a b", Err(UserNameError::BadChar { ch: ' ', at: 1 })),
                ("a$", Err(UserNameError::BadChar { ch: '$', at: 1 })),
                ("a\n", Err(UserNameError::BadChar { ch: '\n', at: 1 })),
            ],
            |text| UserName::parse((*text).to_owned()).map(|_| ()),
        );
    }

    #[test]
    fn accepted_names_parse_back_to_themselves() {
        for raw in ["a", "_x", "dev-1"] {
            let name = UserName::parse(raw.to_owned()).unwrap();
            assert_eq!(UserName::parse(name.get().clone()), Ok(name));
        }
    }

    /// The design grammar, written independently of the implementation.
    fn grammar(text: &str) -> bool {
        match text.chars().collect::<Vec<_>>().split_first() {
            Some((first, rest)) => {
                (first.is_ascii_lowercase() || *first == '_')
                    && rest.iter().all(|c| {
                        c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '-')
                    })
                    && rest.len() < MAX_LEN
                    && text != "root"
            }
            None => false,
        }
    }

    proptest! {
        #[test]
        fn strings_of_the_grammar_are_accepted(text in "[a-z_][a-z0-9_-]{0,31}") {
            prop_assert_eq!(UserName::parse(text.clone()).is_ok(), text != "root");
        }

        #[test]
        fn parse_accepts_exactly_the_grammar_among_near_misses(
            text in "[a-zA-Z0-9_. /é-]{0,36}"
        ) {
            prop_assert_eq!(UserName::parse(text.clone()).is_ok(), grammar(&text), "{:?}", text);
        }
    }
}
