//! `Timeout`: a finite duration, `> 0` and `<= 86400 s`; what `--timeout` and `--boot-timeout`
//! carry. The CLI reads seconds as a float, so [`from_secs_f64`] is the boundary into it.

use std::time::Duration;

use crate::diagnostic::Diagnostic;
use crate::refined::{Refined, Refinement};

/// Longest accepted timeout: one day.
pub const MAX: Duration = Duration::from_hours(24);

/// Marker of the timeout range.
pub struct TimeoutRange;

/// A duration proven to be in [`TimeoutRange`].
pub type Timeout = Refined<Duration, TimeoutRange>;

#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum TimeoutError {
    #[error("bad timeout: it is zero")]
    Zero,
    #[error("bad timeout: {value:?} is more than 86400 seconds")]
    TooLong { value: Duration },
    #[error("bad timeout: it is not a finite number of seconds")]
    NotFinite,
    #[error("bad timeout: it is negative")]
    Negative,
}

impl Diagnostic for TimeoutError {
    fn code(&self) -> &'static str {
        match self {
            Self::Zero => "timeout.zero",
            Self::TooLong { .. } => "timeout.too-long",
            Self::NotFinite => "timeout.not-finite",
            Self::Negative => "timeout.negative",
        }
    }

    fn message(&self) -> String {
        self.to_string()
    }

    fn hint(&self) -> Option<String> {
        Some("use a number of seconds above 0 and at most 86400".to_owned())
    }
}

impl Refinement<Duration> for TimeoutRange {
    type Error = TimeoutError;

    fn check(value: &Duration) -> Result<(), TimeoutError> {
        if value.is_zero() {
            return Err(TimeoutError::Zero);
        }
        if *value > MAX {
            return Err(TimeoutError::TooLong { value: *value });
        }
        Ok(())
    }
}

/// Boundary for a flag given in (fractional) seconds.
///
/// # Errors
/// [`TimeoutError`] naming why `secs` is not a timeout.
pub fn from_secs_f64(secs: f64) -> Result<Timeout, TimeoutError> {
    if !secs.is_finite() {
        return Err(TimeoutError::NotFinite);
    }
    if secs < 0.0 {
        return Err(TimeoutError::Negative);
    }
    // Only values beyond `Duration`'s range fail to convert; they are far above the maximum.
    Timeout::parse(Duration::try_from_secs_f64(secs).unwrap_or(Duration::MAX))
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) mod tests {
    use super::*;
    use crate::testing::{self, Entry};

    fn witness(e: &TimeoutError) -> &'static str {
        match e {
            TimeoutError::Zero => "Zero",
            TimeoutError::TooLong { .. } => "TooLong",
            TimeoutError::NotFinite => "NotFinite",
            TimeoutError::Negative => "Negative",
        }
    }

    fn errors() -> Vec<TimeoutError> {
        vec![
            TimeoutError::Zero,
            TimeoutError::TooLong {
                value: Duration::from_millis(86_400_001),
            },
            TimeoutError::NotFinite,
            TimeoutError::Negative,
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
            &["Zero", "TooLong", "NotFinite", "Negative"],
        );
    }

    #[test]
    fn timeout_parse_accepts_above_zero_up_to_a_day() {
        let over = Duration::from_nanos(86_400_000_000_001);
        let under = Duration::from_nanos(86_399_999_999_999);
        testing::table(
            &[
                (Duration::ZERO, Err(TimeoutError::Zero)),
                (Duration::from_nanos(1), Ok(Duration::from_nanos(1))),
                (Duration::from_secs(1), Ok(Duration::from_secs(1))),
                (under, Ok(under)),
                (MAX, Ok(MAX)),
                (over, Err(TimeoutError::TooLong { value: over })),
                (
                    Duration::MAX,
                    Err(TimeoutError::TooLong {
                        value: Duration::MAX,
                    }),
                ),
            ],
            |value| Timeout::parse(*value).map(Timeout::into_inner),
        );
    }

    #[test]
    fn accepted_timeouts_parse_back_to_themselves() {
        for raw in [Duration::from_millis(1), Duration::from_secs(60), MAX] {
            let timeout = Timeout::parse(raw).unwrap();
            assert_eq!(Timeout::parse(*timeout.get()), Ok(timeout));
        }
    }

    #[test]
    fn timeout_from_float_seconds_refuses_non_finite_negative_zero_and_huge_values() {
        let over = Duration::from_secs_f64(86_400.5);
        testing::table(
            &[
                (f64::NAN, Err(TimeoutError::NotFinite)),
                (f64::INFINITY, Err(TimeoutError::NotFinite)),
                (f64::NEG_INFINITY, Err(TimeoutError::NotFinite)),
                (-1.0, Err(TimeoutError::Negative)),
                (-f64::MIN_POSITIVE, Err(TimeoutError::Negative)),
                (0.0, Err(TimeoutError::Zero)),
                (-0.0, Err(TimeoutError::Zero)),
                (0.5, Ok(Duration::from_millis(500))),
                (60.0, Ok(Duration::from_secs(60))),
                (86_400.0, Ok(MAX)),
                (86_400.5, Err(TimeoutError::TooLong { value: over })),
                (
                    f64::MAX,
                    Err(TimeoutError::TooLong {
                        value: Duration::MAX,
                    }),
                ),
            ],
            |secs| from_secs_f64(*secs).map(Timeout::into_inner),
        );
    }
}
