//! `LingerPolicy`: what a detached start does when the user's systemd session would end at logout
//! (config key `linger`).

use std::fmt;

/// `Enable` turns linger on for the user when it is off; `Warn` only reports it.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum LingerPolicy {
    #[default]
    Enable,
    Warn,
}

impl LingerPolicy {
    /// Every spelling [`LingerPolicy::parse`] accepts, in declaration order.
    pub const NAMES: &'static [&'static str] = &["enable", "warn"];

    /// The spelling used in `run_config.kdl`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Enable => "enable",
            Self::Warn => "warn",
        }
    }

    /// The policy spelled exactly as in [`LingerPolicy::NAMES`].
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "enable" => Some(Self::Enable),
            "warn" => Some(Self::Warn),
            _ => None,
        }
    }
}

impl fmt::Display for LingerPolicy {
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
    fn policies_are_spelled_enable_and_warn_and_nothing_else_parses() {
        testing::table(
            &[
                ("enable", Some(LingerPolicy::Enable)),
                ("warn", Some(LingerPolicy::Warn)),
                ("", None),
                ("Enable", None),
                ("warn ", None),
                ("on", None),
            ],
            |text| LingerPolicy::parse(text),
        );
    }

    #[test]
    fn names_list_every_policy_in_order_and_parse_back_to_it() {
        let all = [LingerPolicy::Enable, LingerPolicy::Warn];
        let names: Vec<&str> = all.iter().map(|policy| policy.as_str()).collect();
        assert_eq!(names, LingerPolicy::NAMES);
        for policy in all {
            assert_eq!(policy.to_string(), policy.as_str());
            assert_eq!(LingerPolicy::parse(&policy.to_string()), Some(policy));
        }
    }

    #[test]
    fn the_default_policy_enables_linger() {
        assert_eq!(LingerPolicy::default(), LingerPolicy::Enable);
    }
}
