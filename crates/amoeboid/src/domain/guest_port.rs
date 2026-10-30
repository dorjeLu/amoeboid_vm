//! `GuestPort`: a TCP port number `1..=65535` inside the guest (the guest's sshd port). Distinct
//! from `LoopbackPort`, a host port bound on 127.0.0.1: the two are never interchangeable.

use crate::diagnostic::Diagnostic;
use crate::refined::{Checked, Refined, Refinement};

/// Marker of the guest port range.
pub struct GuestPortRange;

/// A guest port number proven to be non-zero.
pub type GuestPort = Refined<u16, GuestPortRange>;

#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum GuestPortError {
    #[error("bad port: 0 is not a port number")]
    Zero,
}

impl Diagnostic for GuestPortError {
    fn code(&self) -> &'static str {
        match self {
            Self::Zero => "guest-port.zero",
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

impl GuestPortRange {
    /// Compile-time twin of [`Refinement::check`], for `refined!`.
    #[must_use]
    pub const fn check_const(value: u16) -> Option<Checked<u16, Self>> {
        match value {
            0 => None,
            1.. => Some(Checked::new(value)),
        }
    }
}

impl Refinement<u16> for GuestPortRange {
    type Error = GuestPortError;

    fn check(value: &u16) -> Result<(), GuestPortError> {
        match *value {
            0 => Err(GuestPortError::Zero),
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

    fn witness(e: &GuestPortError) -> &'static str {
        match e {
            GuestPortError::Zero => "Zero",
        }
    }

    fn errors() -> Vec<GuestPortError> {
        vec![GuestPortError::Zero]
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
    fn guest_port_parse_refuses_zero_and_accepts_every_other_u16() {
        testing::table(
            &[
                (0, Err(GuestPortError::Zero)),
                (1, Ok(1)),
                (2, Ok(2)),
                (22, Ok(22)),
                (65_534, Ok(65_534)),
                (u16::MAX, Ok(u16::MAX)),
            ],
            |value| GuestPort::parse(*value).map(GuestPort::into_inner),
        );
    }

    #[test]
    fn check_const_agrees_with_check_at_every_edge() {
        for value in [0, 1, 2, 22, u16::MAX - 1, u16::MAX] {
            assert_eq!(
                GuestPortRange::check_const(value).is_some(),
                GuestPortRange::check(&value).is_ok(),
                "value {value}"
            );
        }
    }

    #[test]
    fn refined_constants_of_ports_are_the_parsed_values() {
        const SSH: GuestPort = refined!(GuestPort, 22);
        assert_eq!(GuestPort::parse(22), Ok(SSH));
    }

    #[test]
    fn accepted_guest_ports_parse_back_to_themselves() {
        for raw in [1, 2222, u16::MAX] {
            let port = GuestPort::parse(raw).unwrap();
            assert_eq!(GuestPort::parse(*port.get()), Ok(port));
        }
    }
}
