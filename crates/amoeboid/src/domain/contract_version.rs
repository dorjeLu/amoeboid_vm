//! `ContractVersion`: the version of the Nix/Rust contract (`amoeboid.contract` in `base.nix`),
//! `1..=u32::MAX`.

use crate::diagnostic::Diagnostic;
use crate::refined::{Checked, Refined, Refinement};

/// Marker of the contract version range.
pub struct VersionRange;

/// A contract version proven to be non-zero.
pub type ContractVersion = Refined<u32, VersionRange>;

#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum ContractVersionError {
    #[error("bad contract version: 0 is not a version")]
    Zero,
}

impl Diagnostic for ContractVersionError {
    fn code(&self) -> &'static str {
        match self {
            Self::Zero => "contract-version.zero",
        }
    }

    fn message(&self) -> String {
        self.to_string()
    }

    fn hint(&self) -> Option<String> {
        match self {
            Self::Zero => Some("set amoeboid.contract to a positive number in base.nix".to_owned()),
        }
    }
}

impl VersionRange {
    /// Compile-time twin of [`Refinement::check`], for `refined!`.
    #[must_use]
    pub const fn check_const(value: u32) -> Option<Checked<u32, Self>> {
        if value >= 1 {
            Some(Checked::new(value))
        } else {
            None
        }
    }
}

impl Refinement<u32> for VersionRange {
    type Error = ContractVersionError;

    fn check(value: &u32) -> Result<(), ContractVersionError> {
        match *value {
            0 => Err(ContractVersionError::Zero),
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

    fn witness(e: &ContractVersionError) -> &'static str {
        match e {
            ContractVersionError::Zero => "Zero",
        }
    }

    fn errors() -> Vec<ContractVersionError> {
        vec![ContractVersionError::Zero]
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
    fn contract_version_parse_refuses_zero_and_accepts_every_other_u32() {
        testing::table(
            &[
                (0, Err(ContractVersionError::Zero)),
                (1, Ok(1)),
                (2, Ok(2)),
                (u32::MAX - 1, Ok(u32::MAX - 1)),
                (u32::MAX, Ok(u32::MAX)),
            ],
            |value| ContractVersion::parse(*value).map(ContractVersion::into_inner),
        );
    }

    #[test]
    fn accepted_versions_parse_back_to_themselves() {
        for raw in [1, 7, u32::MAX] {
            let version = ContractVersion::parse(raw).unwrap();
            assert_eq!(ContractVersion::parse(*version.get()), Ok(version));
        }
    }

    #[test]
    fn check_const_agrees_with_check_at_every_edge() {
        for value in [0, 1, 2, u32::MAX - 1, u32::MAX] {
            assert_eq!(
                VersionRange::check_const(value).is_some(),
                VersionRange::check(&value).is_ok(),
                "value {value}"
            );
        }
    }

    #[test]
    fn refined_constants_of_contract_version_are_the_parsed_values() {
        const ONE: ContractVersion = refined!(ContractVersion, 1);
        assert_eq!(ContractVersion::parse(1), Ok(ONE));
    }
}
