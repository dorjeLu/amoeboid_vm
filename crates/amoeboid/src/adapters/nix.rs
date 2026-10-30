//! `StdNix`: [`Nix`] on the `nix` found through `PATH`.

use std::future::Future;
use std::process::Stdio;

use tokio::process::Command;

use crate::domain::{AbsPath, FlakeDir, FlakePath};
use crate::plan::nix_build_argv;
use crate::ports::Cause;
use crate::ports::nix::{BuildStatus, Nix, NixError};

/// Runs `nix build` with the terminal's output and no input.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct StdNix;

impl Nix for StdNix {
    fn build(
        &self,
        flake: &FlakeDir,
        host_module: &FlakePath,
        out_link: &AbsPath,
    ) -> impl Future<Output = Result<BuildStatus, NixError>> + Send {
        let argv = nix_build_argv(flake, host_module, out_link);
        async move { run(argv.program(), argv.args()).await }
    }
}

/// Runs `program` to its end; dropping the future kills it.
async fn run(program: &str, args: &[String]) -> Result<BuildStatus, NixError> {
    let status = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .status()
        .await
        .map_err(|error| NixError::Spawn {
            cause: Cause::of(&error),
        })?;
    Ok(if status.success() {
        BuildStatus::Built
    } else {
        BuildStatus::Failed {
            code: status.code(),
        }
    })
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use std::io;

    use super::*;

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }

    fn args(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| (*item).to_owned()).collect()
    }

    #[test]
    fn the_exit_status_of_the_program_is_the_build_status() {
        let _spawning = crate::testing::one_spawn_at_a_time();
        let seen = |program, items: &[&str]| runtime().block_on(run(program, &args(items)));
        assert_eq!(seen("true", &[]), Ok(BuildStatus::Built));
        assert_eq!(
            seen("sh", &["-c", "exit 3"]),
            Ok(BuildStatus::Failed { code: Some(3) })
        );
        assert_eq!(
            seen("sh", &["-c", "kill -KILL $$"]),
            Ok(BuildStatus::Failed { code: None })
        );
    }

    #[test]
    fn a_program_that_is_not_there_is_a_spawn_failure() {
        let _spawning = crate::testing::one_spawn_at_a_time();
        assert_eq!(
            runtime().block_on(run("/nonexistent/nix", &[])),
            Err(NixError::Spawn {
                cause: Cause {
                    kind: io::ErrorKind::NotFound,
                    errno: Some(2),
                },
            })
        );
    }
}
