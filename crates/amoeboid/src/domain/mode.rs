//! `Mode`: the access policy of a shared directory.

use std::fmt;

/// Read-write or read-only; read-only is enforced by the host's `virtiofsd --readonly`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Mode {
    Rw,
    Ro,
}

impl Mode {
    /// Every spelling [`Mode::from_suffix`] accepts, in declaration order.
    pub const NAMES: &'static [&'static str] = &["rw", "ro"];

    /// The spelling used in mount specs and records.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Rw => "rw",
            Self::Ro => "ro",
        }
    }

    /// The mode spelled exactly `rw` or `ro`.
    #[must_use]
    pub fn from_suffix(text: &str) -> Option<Self> {
        match text {
            "rw" => Some(Self::Rw),
            "ro" => Some(Self::Ro),
            _ => None,
        }
    }
}

impl fmt::Display for Mode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::testing;

    #[test]
    fn modes_are_spelled_rw_and_ro_and_nothing_else_parses() {
        testing::table(
            &[
                ("rw", Some(Mode::Rw)),
                ("ro", Some(Mode::Ro)),
                ("", None),
                ("RW", None),
                ("Ro", None),
                (" ro", None),
                ("rw ", None),
                ("r", None),
                ("rwx", None),
            ],
            |text| Mode::from_suffix(text),
        );
    }

    #[test]
    fn names_list_every_mode_in_order() {
        let names: Vec<&str> = [Mode::Rw, Mode::Ro]
            .iter()
            .map(|mode| mode.as_str())
            .collect();
        assert_eq!(names, Mode::NAMES);
    }

    #[test]
    fn every_mode_displays_as_the_text_that_parses_back_to_it() {
        for mode in [Mode::Rw, Mode::Ro] {
            assert_eq!(mode.to_string(), mode.as_str());
            assert_eq!(Mode::from_suffix(&mode.to_string()), Some(mode));
        }
    }
}
