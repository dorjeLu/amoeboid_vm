//! `StartTime`: clock ticks since boot at which a process started (`/proc/PID/stat` field 22).
//! Together with the pid it identifies a process across pid reuse. Every `u64` is a possible
//! start time, so the predicate cannot fail; the type exists to keep it apart from other integers.

use std::convert::Infallible;

use crate::refined::{Refined, Refinement};

/// Marker of the start time: no restriction.
pub struct ClockTicks;

/// Ticks since boot proven (trivially) to be a start time.
pub type StartTime = Refined<u64, ClockTicks>;

impl Refinement<u64> for ClockTicks {
    type Error = Infallible;

    fn check(_: &u64) -> Result<(), Infallible> {
        Ok(())
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::testing;

    #[test]
    fn start_time_parse_accepts_every_u64() {
        testing::table(
            &[
                (0, Ok(0)),
                (1, Ok(1)),
                (123_456, Ok(123_456)),
                (u64::MAX - 1, Ok(u64::MAX - 1)),
                (u64::MAX, Ok(u64::MAX)),
            ],
            |value| StartTime::parse(*value).map(StartTime::into_inner),
        );
    }

    #[test]
    fn accepted_start_times_parse_back_to_themselves() {
        for raw in [0, 99, u64::MAX] {
            let start = StartTime::parse(raw).unwrap();
            assert_eq!(StartTime::parse(*start.get()), Ok(start));
        }
    }
}
