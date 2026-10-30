//! `Nix`: builds the VM's config (design §Ports and adapters). The port runs `nix build` and
//! reports how it ended; what the build produced is read from the out-link by `ops::build`.

use std::future::Future;

use crate::diagnostic::Diagnostic;
use crate::domain::{AbsPath, FlakeDir, FlakePath};
use crate::ports::Cause;

/// How `nix build` ended.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BuildStatus {
    Built,
    /// It exited with `code`, or was ended by a signal (`None`).
    Failed {
        code: Option<i32>,
    },
}

#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum NixError {
    /// `nix` could not be started (not on `PATH`, not executable).
    #[error("can't run nix: {cause}")]
    Spawn { cause: Cause },
}

impl Diagnostic for NixError {
    fn code(&self) -> &'static str {
        match self {
            Self::Spawn { .. } => "nix.spawn",
        }
    }

    fn message(&self) -> String {
        self.to_string()
    }

    fn hint(&self) -> Option<String> {
        Some("check that nix is installed and runnable".to_owned())
    }
}

pub trait Nix {
    /// `nix build` of `flake` with its `host` input overridden by `host_module`, leaving the
    /// result at `out_link`. Nix's own output goes to the terminal; a cancelled build kills nix.
    fn build(
        &self,
        flake: &FlakeDir,
        host_module: &FlakePath,
        out_link: &AbsPath,
    ) -> impl Future<Output = Result<BuildStatus, NixError>> + Send;
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) mod tests {
    use std::io;

    use super::*;
    use crate::testing::{self, Entry};

    fn witness(e: &NixError) -> &'static str {
        match e {
            NixError::Spawn { .. } => "Spawn",
        }
    }

    pub(crate) fn errors() -> Vec<NixError> {
        vec![NixError::Spawn {
            cause: Cause {
                kind: io::ErrorKind::PermissionDenied,
                errno: Some(13),
            },
        }]
    }

    /// One catalogue entry per variant.
    pub(crate) fn entries() -> Vec<Entry> {
        errors().iter().map(Entry::of).collect()
    }

    #[test]
    fn every_error_variant_has_a_table_case() {
        testing::every_variant(&errors(), witness, &["Spawn"]);
    }

    #[test]
    fn a_spawn_failure_names_the_cause() {
        assert_eq!(
            errors().first().map(Diagnostic::message),
            Some("can't run nix: permission denied".to_owned())
        );
    }
}
