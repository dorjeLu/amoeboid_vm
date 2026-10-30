//! `Identity`: who is running amoeboid, refined at the process-identity boundary (design §Where
//! values come from). It seeds a fresh configuration.

use crate::diagnostic::Diagnostic;
use crate::domain::{Gid, GidError, Uid, UidError, UserName, UserNameError};

/// The invoking account: name, uid and gid, each a proof.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Identity {
    pub user: UserName,
    pub uid: Uid,
    pub gid: Gid,
}

/// Why the invoking account can't be a guest user; the first offender, in the order name, uid, gid.
#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum IdentityError {
    #[error("can't use the account name: {0}")]
    User(UserNameError),
    #[error("can't use the account uid: {0}")]
    Uid(UidError),
    #[error("can't use the account gid: {0}")]
    Gid(GidError),
}

impl Diagnostic for IdentityError {
    fn code(&self) -> &'static str {
        match self {
            Self::User(_) => "identity.user",
            Self::Uid(_) => "identity.uid",
            Self::Gid(_) => "identity.gid",
        }
    }

    fn message(&self) -> String {
        self.to_string()
    }

    fn hint(&self) -> Option<String> {
        Some(
            match self {
                Self::User(_) => "run amoeboid from an account with a plain login name",
                Self::Uid(_) | Self::Gid(_) => "run amoeboid as an unprivileged user",
            }
            .to_owned(),
        )
    }
}

impl Identity {
    /// Refines what the operating system says about the process.
    ///
    /// # Errors
    ///
    /// The first of name, uid and gid outside its domain type (root, or a name that can't be a
    /// guest user).
    pub fn parse(user: String, uid: u32, gid: u32) -> Result<Self, IdentityError> {
        Ok(Self {
            user: UserName::parse(user).map_err(IdentityError::User)?,
            uid: Uid::parse(uid).map_err(IdentityError::Uid)?,
            gid: Gid::parse(gid).map_err(IdentityError::Gid)?,
        })
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) mod tests {
    use super::*;
    use crate::testing::{self, Entry};

    fn witness(e: &IdentityError) -> &'static str {
        match e {
            IdentityError::User(_) => "User",
            IdentityError::Uid(_) => "Uid",
            IdentityError::Gid(_) => "Gid",
        }
    }

    fn errors() -> Vec<IdentityError> {
        vec![
            IdentityError::User(UserNameError::Empty),
            IdentityError::Uid(UidError::Root),
            IdentityError::Gid(GidError::Root),
        ]
    }

    /// One catalogue entry per variant.
    pub(crate) fn entries() -> Vec<Entry> {
        errors().iter().map(Entry::of).collect()
    }

    #[test]
    fn every_error_variant_has_a_table_case() {
        testing::every_variant(&errors(), witness, &["User", "Uid", "Gid"]);
    }

    #[test]
    fn an_account_is_refined_field_by_field_and_the_first_offender_is_reported() {
        let ok = |user: &str, uid: u32, gid: u32| {
            Ok(Identity {
                user: UserName::parse(user.to_owned()).unwrap(),
                uid: Uid::parse(uid).unwrap(),
                gid: Gid::parse(gid).unwrap(),
            })
        };
        testing::table(
            &[
                (("alice", 1000, 100), ok("alice", 1000, 100)),
                (
                    ("", 1000, 100),
                    Err(IdentityError::User(UserNameError::Empty)),
                ),
                (
                    ("root", 0, 0),
                    Err(IdentityError::User(UserNameError::Root)),
                ),
                (("bob", 0, 0), Err(IdentityError::Uid(UidError::Root))),
                (
                    ("bob", u32::MAX, 100),
                    Err(IdentityError::Uid(UidError::Unset)),
                ),
                (("bob", 1000, 0), Err(IdentityError::Gid(GidError::Root))),
            ],
            |&(user, uid, gid)| Identity::parse(user.to_owned(), uid, gid),
        );
    }
}
