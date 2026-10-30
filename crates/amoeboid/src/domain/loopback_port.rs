//! `LoopbackPort`: a TCP port number `1..=65535` that was bound on 127.0.0.1. The refinement is the
//! number; the adapter that binds it is the observation.

use crate::diagnostic::Diagnostic;
use crate::refined::{Checked, Refined, Refinement};

/// Marker of the loopback port range.
pub struct LoopbackPortRange;

/// A port number proven to be non-zero.
pub type LoopbackPort = Refined<u16, LoopbackPortRange>;

#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum LoopbackPortError {
    #[error("bad port: 0 is not a port number")]
    Zero,
}

impl Diagnostic for LoopbackPortError {
    fn code(&self) -> &'static str {
        match self {
            Self::Zero => "loopback-port.zero",
        }
    }

    fn message(&self) -> String {
        self.to_string()
    }

    fn hint(&self) -> Option<String> {
        match self {
            Self::Zero => Some("use a port from 1 to 65535".to_owned()),
        }
    }
}

impl LoopbackPortRange {
    /// Compile-time twin of [`Refinement::check`], for `refined!`.
    #[must_use]
    pub const fn check_const(value: u16) -> Option<Checked<u16, Self>> {
        match value {
            0 => None,
            1.. => Some(Checked::new(value)),
        }
    }
}

impl Refinement<u16> for LoopbackPortRange {
    type Error = LoopbackPortError;

    fn check(value: &u16) -> Result<(), LoopbackPortError> {
        match *value {
            0 => Err(LoopbackPortError::Zero),
            1.. => Ok(()),
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) mod tests {
    use super::*;
    use crate::refined;
    use crate::testing::{self, Entry};

    fn witness(e: &LoopbackPortError) -> &'static str {
        match e {
            LoopbackPortError::Zero => "Zero",
        }
    }

    fn errors() -> Vec<LoopbackPortError> {
        vec![LoopbackPortError::Zero]
    }

    /// One catalogue entry per variant.
    pub(crate) fn entries() -> Vec<Entry> {
        errors().iter().map(Entry::of).collect()
    }

    #[test]
    fn every_error_variant_has_a_table_case() {
        testing::every_variant(&errors(), witness, &["Zero"]);
    }

    #[test]
    fn loopback_port_parse_refuses_zero_and_accepts_every_other_u16() {
        testing::table(
            &[
                (0, Err(LoopbackPortError::Zero)),
                (1, Ok(1)),
                (2, Ok(2)),
                (22, Ok(22)),
                (65_534, Ok(65_534)),
                (u16::MAX, Ok(u16::MAX)),
            ],
            |value| LoopbackPort::parse(*value).map(LoopbackPort::into_inner),
        );
    }

    #[test]
    fn check_const_agrees_with_check_at_every_edge() {
        for value in [0, 1, 2, 22, u16::MAX - 1, u16::MAX] {
            assert_eq!(
                LoopbackPortRange::check_const(value).is_some(),
                LoopbackPortRange::check(&value).is_ok(),
                "value {value}"
            );
        }
    }

    #[test]
    fn refined_constants_of_ports_are_the_parsed_values() {
        const SSH: LoopbackPort = refined!(LoopbackPort, 22);
        assert_eq!(LoopbackPort::parse(22), Ok(SSH));
    }

    #[test]
    fn accepted_loopback_ports_parse_back_to_themselves() {
        for raw in [1, 2222, u16::MAX] {
            let port = LoopbackPort::parse(raw).unwrap();
            assert_eq!(LoopbackPort::parse(*port.get()), Ok(port));
        }
    }
}
