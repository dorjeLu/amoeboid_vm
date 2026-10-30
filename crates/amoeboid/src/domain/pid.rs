//! `Pid`: a process id, `1..=i32::MAX`. Kernel `pid_t` is signed, so a larger number is no pid
//! (and `kill` would read a wrapped one as a process group).

use crate::diagnostic::Diagnostic;
use crate::refined::{Refined, Refinement};

/// Largest `pid_t`.
pub const MAX: u32 = 0x7FFF_FFFF;

/// Marker of the pid range.
pub struct PidRange;

/// A process id proven to be in [`PidRange`].
pub type Pid = Refined<u32, PidRange>;

#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum PidError {
    #[error("bad pid: 0 is not a process")]
    Zero,
    #[error("bad pid: {value} is more than {MAX}")]
    TooLarge { value: u32 },
}

impl Diagnostic for PidError {
    fn code(&self) -> &'static str {
        match self {
            Self::Zero => "pid.zero",
            Self::TooLarge { .. } => "pid.too-large",
        }
    }

    fn message(&self) -> String {
        self.to_string()
    }

    fn hint(&self) -> Option<String> {
        match self {
            Self::Zero | Self::TooLarge { .. } => None,
        }
    }
}

impl Refinement<u32> for PidRange {
    type Error = PidError;

    fn check(value: &u32) -> Result<(), PidError> {
        match *value {
            0 => Err(PidError::Zero),
            1..=MAX => Ok(()),
            value @ 0x8000_0000.. => Err(PidError::TooLarge { value }),
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) mod tests {
    use super::*;
    use crate::testing::{self, Entry};

    fn witness(e: &PidError) -> &'static str {
        match e {
            PidError::Zero => "Zero",
            PidError::TooLarge { .. } => "TooLarge",
        }
    }

    fn errors() -> Vec<PidError> {
        vec![PidError::Zero, PidError::TooLarge { value: 0x8000_0000 }]
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
    fn pid_parse_accepts_one_to_the_largest_pid_t() {
        testing::table(
            &[
                (0, Err(PidError::Zero)),
                (1, Ok(1)),
                (2, Ok(2)),
                (4_194_304, Ok(4_194_304)),
                (0x7FFF_FFFE, Ok(0x7FFF_FFFE)),
                (0x7FFF_FFFF, Ok(0x7FFF_FFFF)),
                (0x8000_0000, Err(PidError::TooLarge { value: 0x8000_0000 })),
                (u32::MAX, Err(PidError::TooLarge { value: u32::MAX })),
            ],
            |value| Pid::parse(*value).map(Pid::into_inner),
        );
    }

    #[test]
    fn accepted_pids_parse_back_to_themselves() {
        for raw in [1, 4242, 0x7FFF_FFFF] {
            let pid = Pid::parse(raw).unwrap();
            assert_eq!(Pid::parse(*pid.get()), Ok(pid));
        }
    }
}
