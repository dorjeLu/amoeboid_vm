//! `System`: what the host answers about itself before a VM is started (design §Resiliency/
//! Preflight). Each method observes one fact and decides nothing; the decision is
//! `ops::preflight::decide`. Facts the system can't give (a missing file) are values of their
//! own, not errors.

use std::future::Future;

use crate::diagnostic::Diagnostic;
use crate::domain::{
    AbsDir, KernelVersion, KernelVersionError, KvmAccess, Pid, Tool, ToolPresence, UsernsSwitches,
};

/// The kernel's release as [`KernelVersion`], or why it can't be read as one.
pub type KernelProbe = Result<KernelVersion, KernelVersionError>;

#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum SystemError {
    /// The blocking task did not finish (the runtime was shutting down).
    #[error("can't inspect the host: the operation was aborted")]
    Aborted,
}

impl Diagnostic for SystemError {
    fn code(&self) -> &'static str {
        match self {
            Self::Aborted => "system.aborted",
        }
    }

    fn message(&self) -> String {
        self.to_string()
    }

    fn hint(&self) -> Option<String> {
        Some("run the command again".to_owned())
    }
}

pub trait System {
    /// Whether `/dev/kvm` can be opened for reading and writing.
    fn kvm(&self) -> impl Future<Output = Result<KvmAccess, SystemError>> + Send;

    /// Whether an executable regular file named `tool` is in a `PATH` directory.
    fn tool(&self, tool: Tool) -> impl Future<Output = Result<ToolPresence, SystemError>> + Send;

    /// The kernel switches that govern unprivileged user namespaces.
    fn userns(&self) -> impl Future<Output = Result<UsernsSwitches, SystemError>> + Send;

    /// The running kernel's version.
    fn kernel(&self) -> impl Future<Output = Result<KernelProbe, SystemError>> + Send;

    /// The pid of a qemu that a Python-launcher run left in `runtime` (the Python layout's
    /// runtime dir of the VM), if the pid file names a live qemu.
    fn legacy_qemu(
        &self,
        runtime: &AbsDir,
    ) -> impl Future<Output = Result<Option<Pid>, SystemError>> + Send;
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) mod tests {
    use super::*;
    use crate::testing::{self, Entry};

    fn witness(e: &SystemError) -> &'static str {
        match e {
            SystemError::Aborted => "Aborted",
        }
    }

    pub(crate) fn errors() -> Vec<SystemError> {
        vec![SystemError::Aborted]
    }

    /// One catalogue entry per variant.
    pub(crate) fn entries() -> Vec<Entry> {
        errors().iter().map(Entry::of).collect()
    }

    #[test]
    fn every_error_variant_has_a_table_case() {
        testing::every_variant(&errors(), witness, &["Aborted"]);
    }
}
