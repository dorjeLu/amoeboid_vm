//! `Gid`: a host group id other than root's, `1..=2^32-2`; a distinct marker from `Uid`, so a gid
//! is never taken for a uid.

use crate::diagnostic::Diagnostic;
use crate::refined::{Refined, Refinement};

/// Marker of the gid range.
pub struct GidRange;

/// A group id proven to be in [`GidRange`].
pub type Gid = Refined<u32, GidRange>;

#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum GidError {
    #[error("bad group id: 0 belongs to root")]
    Root,
    /// `(gid_t)-1`: the kernel's "no group" value.
    #[error("bad group id: 4294967295 means no group")]
    Unset,
}

impl Diagnostic for GidError {
    fn code(&self) -> &'static str {
        match self {
            Self::Root => "gid.root",
            Self::Unset => "gid.unset",
        }
    }

    fn message(&self) -> String {
        self.to_string()
    }

    fn hint(&self) -> Option<String> {
        match self {
            Self::Root => Some("run amoeboid as an unprivileged user".to_owned()),
            Self::Unset => None,
        }
    }
}

impl Refinement<u32> for GidRange {
    type Error = GidError;

    fn check(value: &u32) -> Result<(), GidError> {
        match *value {
            0 => Err(GidError::Root),
            u32::MAX => Err(GidError::Unset),
            1..=0xFFFF_FFFE => Ok(()),
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) mod tests {
    use super::*;
    use crate::testing::{self, Entry};

    fn witness(e: &GidError) -> &'static str {
        match e {
            GidError::Root => "Root",
            GidError::Unset => "Unset",
        }
    }

    fn errors() -> Vec<GidError> {
        vec![GidError::Root, GidError::Unset]
    }

    /// One catalogue entry per variant.
    pub(crate) fn entries() -> Vec<Entry> {
        errors().iter().map(Entry::of).collect()
    }

    #[test]
    fn every_error_variant_has_a_table_case() {
        testing::every_variant(&errors(), witness, &["Root", "Unset"]);
    }

    #[test]
    fn gid_parse_refuses_root_and_the_unset_value_and_accepts_everything_between() {
        testing::table(
            &[
                (0, Err(GidError::Root)),
                (1, Ok(1)),
                (2, Ok(2)),
                (1000, Ok(1000)),
                (0xFFFF_FFFD, Ok(0xFFFF_FFFD)),
                (0xFFFF_FFFE, Ok(0xFFFF_FFFE)),
                (u32::MAX, Err(GidError::Unset)),
            ],
            |value| Gid::parse(*value).map(Gid::into_inner),
        );
    }

    #[test]
    fn accepted_gids_parse_back_to_themselves() {
        for raw in [1, 1000, 0xFFFF_FFFE] {
            let gid = Gid::parse(raw).unwrap();
            assert_eq!(Gid::parse(*gid.get()), Ok(gid));
        }
    }
}
