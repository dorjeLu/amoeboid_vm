//! The two host-owned records of a VM (design §Run record): `run` (the processes of the running
//! VM, KDL) and `mounts` (the shares of the last start, tab-separated, same format as the
//! guest's table). Pure: text <-> refined values; reading and atomic writing belong to
//! `ops::records`. Both parse strictly: corruption is a typed error, never an empty record.

mod last_shares;
mod run;

pub use last_shares::{LastShares, LastSharesError};
pub use run::{Run, RunError};

#[cfg(test)]
pub(crate) use last_shares::tests as last_shares_tests;
#[cfg(test)]
pub(crate) use run::tests as run_tests;
