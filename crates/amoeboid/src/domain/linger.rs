//! `Linger`: whether systemd keeps a user's session (and so `$XDG_RUNTIME_DIR`) alive after the
//! last login ends. Refined from the observation of the marker file systemd keeps per user.

use crate::domain::Observation;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Linger {
    On,
    Off,
}

impl Linger {
    /// A regular file at the marker path means linger is on; anything else (nothing, a
    /// directory, a socket) is not what systemd writes, so it counts as off.
    #[must_use]
    pub const fn observe(marker: &Observation) -> Self {
        match marker {
            Observation::File { .. } => Self::On,
            Observation::Missing | Observation::Directory { .. } | Observation::Other => Self::Off,
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::domain::{AbsDir, AbsPath};
    use crate::testing;

    fn witness(linger: Linger) -> &'static str {
        match linger {
            Linger::On => "On",
            Linger::Off => "Off",
        }
    }

    #[test]
    fn every_state_is_the_answer_to_some_observation() {
        let cases = [Linger::On, Linger::Off];
        testing::every_variant(&cases, |l| witness(*l), &["On", "Off"]);
    }

    #[test]
    fn only_a_regular_file_at_the_marker_path_is_linger_on() {
        let file = |executable| Observation::File {
            resolved: AbsPath::parse("/var/lib/systemd/linger/alice".to_owned()).unwrap(),
            executable,
        };
        testing::table(
            &[
                (Observation::Missing, Linger::Off),
                (
                    Observation::Directory {
                        resolved: AbsDir::parse("/var/lib/systemd/linger/alice".to_owned())
                            .unwrap(),
                    },
                    Linger::Off,
                ),
                (file(false), Linger::On),
                (file(true), Linger::On),
                (Observation::Other, Linger::Off),
            ],
            Linger::observe,
        );
    }
}
