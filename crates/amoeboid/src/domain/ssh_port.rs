//! `SshPort`: `Auto` (pick a free port at start) or `Fixed(1024..=65535)`. In `run_config.kdl`
//! `0` means auto, so [`SshPort::parse`] is the boundary for that integer.

use crate::diagnostic::Diagnostic;
use crate::refined::{Refined, Refinement};

/// Lowest port an unprivileged process may bind.
pub const FIRST_UNPRIVILEGED: u16 = 1024;

/// Marker of the fixed port range.
pub struct FixedPortRange;

/// A port number proven to be unprivileged (`1024..=65535`).
pub type FixedPort = Refined<u16, FixedPortRange>;

#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum FixedPortError {
    #[error("bad ssh port: {value} is below {FIRST_UNPRIVILEGED}")]
    Privileged { value: u16 },
}

impl Diagnostic for FixedPortError {
    fn code(&self) -> &'static str {
        match self {
            Self::Privileged { .. } => "ssh-port.privileged",
        }
    }

    fn message(&self) -> String {
        self.to_string()
    }

    fn hint(&self) -> Option<String> {
        match self {
            Self::Privileged { .. } => {
                Some("use 0 (pick a free port) or a port from 1024 to 65535".to_owned())
            }
        }
    }
}

impl Refinement<u16> for FixedPortRange {
    type Error = FixedPortError;

    fn check(value: &u16) -> Result<(), FixedPortError> {
        match *value {
            value @ 0..FIRST_UNPRIVILEGED => Err(FixedPortError::Privileged { value }),
            FIRST_UNPRIVILEGED.. => Ok(()),
        }
    }
}

/// Where the host forwards ssh to the guest.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum SshPort {
    Auto,
    Fixed(FixedPort),
}

impl SshPort {
    /// `0` is [`SshPort::Auto`]; anything else must be a [`FixedPort`].
    ///
    /// # Errors
    /// [`FixedPortError`] for a privileged port other than 0.
    pub fn parse(value: u16) -> Result<Self, FixedPortError> {
        match value {
            0 => Ok(Self::Auto),
            fixed => FixedPort::parse(fixed).map(Self::Fixed),
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) mod tests {
    use super::*;
    use crate::testing::{self, Entry};

    fn witness(e: &FixedPortError) -> &'static str {
        match e {
            FixedPortError::Privileged { .. } => "Privileged",
        }
    }

    fn errors() -> Vec<FixedPortError> {
        vec![FixedPortError::Privileged { value: 1023 }]
    }

    /// One catalogue entry per variant.
    pub(crate) fn entries() -> Vec<Entry> {
        errors().iter().map(Entry::of).collect()
    }

    #[test]
    fn every_error_variant_has_a_table_case() {
        testing::every_variant(&errors(), witness, &["Privileged"]);
    }

    #[test]
    fn fixed_port_parse_refuses_privileged_ports_and_accepts_the_rest() {
        testing::table(
            &[
                (0, Err(FixedPortError::Privileged { value: 0 })),
                (1, Err(FixedPortError::Privileged { value: 1 })),
                (22, Err(FixedPortError::Privileged { value: 22 })),
                (1023, Err(FixedPortError::Privileged { value: 1023 })),
                (1024, Ok(1024)),
                (1025, Ok(1025)),
                (2222, Ok(2222)),
                (65_534, Ok(65_534)),
                (u16::MAX, Ok(u16::MAX)),
            ],
            |value| FixedPort::parse(*value).map(FixedPort::into_inner),
        );
    }

    #[test]
    fn ssh_port_parse_maps_zero_to_auto_and_checks_the_rest() {
        let fixed = |port: u16| SshPort::Fixed(FixedPort::parse(port).unwrap());
        testing::table(
            &[
                (0, Ok(SshPort::Auto)),
                (1, Err(FixedPortError::Privileged { value: 1 })),
                (1023, Err(FixedPortError::Privileged { value: 1023 })),
                (1024, Ok(fixed(1024))),
                (65_535, Ok(fixed(65_535))),
            ],
            |value| SshPort::parse(*value),
        );
    }

    #[test]
    fn accepted_fixed_ports_parse_back_to_themselves() {
        for raw in [1024, 2222, u16::MAX] {
            let port = FixedPort::parse(raw).unwrap();
            assert_eq!(FixedPort::parse(*port.get()), Ok(port));
        }
    }
}
