//! `Seconds`: a whole number of seconds in `1..=86400`, as written in `run_config.kdl`
//! (`timeouts`). The CLI's `--timeout` flags use `Timeout` instead.

use crate::diagnostic::Diagnostic;
use crate::refined::{Checked, Refined, Refinement};

/// Largest accepted value: one day.
pub const MAX: u32 = 86_400;

/// Marker of the config seconds range.
pub struct SecondsRange;

/// A duration in whole seconds proven to be in [`SecondsRange`].
pub type Seconds = Refined<u32, SecondsRange>;

#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum SecondsError {
    #[error("bad seconds: 0 is not a duration")]
    Zero,
    #[error("bad seconds: {value} is more than {MAX}")]
    TooLarge { value: u32 },
}

impl Diagnostic for SecondsError {
    fn code(&self) -> &'static str {
        match self {
            Self::Zero => "seconds.zero",
            Self::TooLarge { .. } => "seconds.too-large",
        }
    }

    fn message(&self) -> String {
        self.to_string()
    }

    fn hint(&self) -> Option<String> {
        Some(format!("use a whole number of seconds from 1 to {MAX}"))
    }
}

impl SecondsRange {
    /// Compile-time twin of [`Refinement::check`], for `refined!`.
    #[must_use]
    pub const fn check_const(value: u32) -> Option<Checked<u32, Self>> {
        if value >= 1 && value <= MAX {
            Some(Checked::new(value))
        } else {
            None
        }
    }
}

impl Refinement<u32> for SecondsRange {
    type Error = SecondsError;

    fn check(value: &u32) -> Result<(), SecondsError> {
        match *value {
            0 => Err(SecondsError::Zero),
            1..=MAX => Ok(()),
            value @ 86_401.. => Err(SecondsError::TooLarge { value }),
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) mod tests {
    use super::*;
    use crate::refined;
    use crate::testing::{self, Entry};

    fn witness(e: &SecondsError) -> &'static str {
        match e {
            SecondsError::Zero => "Zero",
            SecondsError::TooLarge { .. } => "TooLarge",
        }
    }

    fn errors() -> Vec<SecondsError> {
        vec![SecondsError::Zero, SecondsError::TooLarge { value: 86_401 }]
    }

    /// One catalogue entry per variant.
    pub(crate) fn entries() -> Vec<Entry> {
        errors().iter().map(Entry::of).collect()
    }

    #[test]
    fn every_error_variant_has_a_table_case() {
        testing::every_variant(&errors(), witness, &["Zero", "TooLarge"]);
    }

    #[test]
    fn seconds_parse_accepts_one_to_a_day_and_names_the_violated_edge() {
        testing::table(
            &[
                (0, Err(SecondsError::Zero)),
                (1, Ok(1)),
                (2, Ok(2)),
                (60, Ok(60)),
                (86_399, Ok(86_399)),
                (86_400, Ok(86_400)),
                (86_401, Err(SecondsError::TooLarge { value: 86_401 })),
                (u32::MAX, Err(SecondsError::TooLarge { value: u32::MAX })),
            ],
            |value| Seconds::parse(*value).map(Seconds::into_inner),
        );
    }

    #[test]
    fn accepted_seconds_parse_back_to_themselves() {
        for raw in [1, 180, 86_400] {
            let seconds = Seconds::parse(raw).unwrap();
            assert_eq!(Seconds::parse(*seconds.get()), Ok(seconds));
        }
    }

    #[test]
    fn check_const_agrees_with_check_at_every_edge() {
        for value in [0, 1, 2, 86_399, 86_400, 86_401, u32::MAX] {
            assert_eq!(
                SecondsRange::check_const(value).is_some(),
                SecondsRange::check(&value).is_ok(),
                "value {value}"
            );
        }
    }

    #[test]
    fn refined_constants_of_seconds_are_the_parsed_values() {
        const BOOT: Seconds = refined!(Seconds, 180);
        const DAY: Seconds = refined!(Seconds, 86_400);
        assert_eq!(Seconds::parse(180), Ok(BOOT));
        assert_eq!(Seconds::parse(86_400), Ok(DAY));
    }
}
