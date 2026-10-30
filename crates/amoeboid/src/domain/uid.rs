//! `Uid`: a host user id other than root's, `1..=2^32-2`; the guest account's uid is taken from it.
//!
//! A gid is never a uid, although the predicates are equal:
//!
//! ```compile_fail,E0308
//! use amoeboid::domain::{Gid, Uid};
//! let uid = Uid::parse(1000).unwrap();
//! let _gid: Gid = uid;
//! ```

use crate::diagnostic::Diagnostic;
use crate::refined::{Refined, Refinement};

/// Marker of the uid range.
pub struct UidRange;

/// A user id proven to be in [`UidRange`].
pub type Uid = Refined<u32, UidRange>;

#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum UidError {
    #[error("bad user id: 0 belongs to root")]
    Root,
    /// `(uid_t)-1`: the kernel's "no user" value.
    #[error("bad user id: 4294967295 means no user")]
    Unset,
}

impl Diagnostic for UidError {
    fn code(&self) -> &'static str {
        match self {
            Self::Root => "uid.root",
            Self::Unset => "uid.unset",
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

impl Refinement<u32> for UidRange {
    type Error = UidError;

    fn check(value: &u32) -> Result<(), UidError> {
        match *value {
            0 => Err(UidError::Root),
            u32::MAX => Err(UidError::Unset),
            1..=0xFFFF_FFFE => Ok(()),
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) mod tests {
    use super::*;
    use crate::testing::{self, Entry};

    fn witness(e: &UidError) -> &'static str {
        match e {
            UidError::Root => "Root",
            UidError::Unset => "Unset",
        }
    }

    fn errors() -> Vec<UidError> {
        vec![UidError::Root, UidError::Unset]
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
    fn uid_parse_refuses_root_and_the_unset_value_and_accepts_everything_between() {
        testing::table(
            &[
                (0, Err(UidError::Root)),
                (1, Ok(1)),
                (2, Ok(2)),
                (1000, Ok(1000)),
                (0xFFFF_FFFD, Ok(0xFFFF_FFFD)),
                (0xFFFF_FFFE, Ok(0xFFFF_FFFE)),
                (u32::MAX, Err(UidError::Unset)),
            ],
            |value| Uid::parse(*value).map(Uid::into_inner),
        );
    }

    #[test]
    fn accepted_uids_parse_back_to_themselves() {
        for raw in [1, 1000, 0xFFFF_FFFE] {
            let uid = Uid::parse(raw).unwrap();
            assert_eq!(Uid::parse(*uid.get()), Ok(uid));
        }
    }
}
