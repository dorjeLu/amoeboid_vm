//! Building the VM and reading what the build left (design §Contract, §Observed inputs).

mod decide;

pub use decide::{BuildOutput, ContractInput, Seen, assemble};

use crate::contract::BuildError;
use crate::diagnostic::Diagnostic;
use crate::domain::Observation;
use crate::layout::{BuildFile, VmPaths};
use crate::ports::host_fs::{FsError, HostFs};
use crate::ports::nix::{BuildStatus, Nix, NixError};

/// Why there is no usable build.
#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum BuildFailure {
    #[error(transparent)]
    Build(BuildError),
    #[error(transparent)]
    Nix(NixError),
    #[error(transparent)]
    Probe(FsError),
    #[error(transparent)]
    Read(FsError),
}

impl Diagnostic for BuildFailure {
    fn code(&self) -> &'static str {
        match self {
            Self::Build(error) => error.code(),
            Self::Nix(error) => error.code(),
            Self::Probe(_) => "build.probe",
            Self::Read(_) => "build.read",
        }
    }

    fn message(&self) -> String {
        self.to_string()
    }

    fn hint(&self) -> Option<String> {
        match self {
            Self::Build(error) => error.hint(),
            Self::Nix(error) => error.hint(),
            Self::Probe(error) | Self::Read(error) => error.hint(),
        }
    }
}

/// Runs `nix build` for the VM's own config and reads the result.
///
/// # Errors
/// Nix is missing or fails, or the output is unusable ([`observe_output`]).
pub async fn build(
    nix: &impl Nix,
    fs: &impl HostFs,
    vm: &VmPaths,
) -> Result<BuildOutput, BuildFailure> {
    let status = nix
        .build(&vm.nix_config(), &vm.host_module(), &vm.build_link())
        .await
        .map_err(|error| match error {
            NixError::Spawn { cause } if cause.kind == std::io::ErrorKind::NotFound => {
                BuildFailure::Build(BuildError::NixMissing)
            }
            NixError::Spawn { .. } => BuildFailure::Nix(error),
        })?;
    match status {
        BuildStatus::Built => observe_output(fs, vm).await,
        BuildStatus::Failed { code } => {
            Err(BuildFailure::Build(BuildError::Failed { status: code }))
        }
    }
}

/// Looks at the out-link of the VM's last build: both programs and the contract.
///
/// # Errors
/// A file can't be looked at or read, or the build is unusable (missing or non-executable
/// program, missing, malformed or foreign contract).
pub async fn observe_output(fs: &impl HostFs, vm: &VmPaths) -> Result<BuildOutput, BuildFailure> {
    let probe = |file: BuildFile| async move {
        fs.observe(&vm.build_file(file))
            .await
            .map_err(BuildFailure::Probe)
    };
    let runner = probe(BuildFile::Runner).await?;
    let virtiofsd = probe(BuildFile::Virtiofsd).await?;
    let contract = match probe(BuildFile::Contract).await? {
        Observation::Missing => ContractInput::Missing,
        Observation::Directory { .. } | Observation::Other => ContractInput::NotAFile,
        Observation::File { resolved, .. } => {
            ContractInput::Bytes(fs.read_file(&resolved).await.map_err(BuildFailure::Read)?)
        }
    };
    assemble(
        vm,
        Seen {
            runner,
            virtiofsd,
            contract,
        },
    )
    .map_err(BuildFailure::Build)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[allow(
    clippy::disallowed_types,
    reason = "tests plant files on a real filesystem"
)]
pub(crate) mod tests {
    use std::fs;
    use std::future::{Future, ready};
    use std::io;
    use std::os::unix::fs::{PermissionsExt as _, symlink};
    use std::path::Path;
    use std::sync::Mutex;

    use super::*;
    use crate::adapters::StdFs;
    use crate::contract::{ContractField, OutputProgram, expected};
    use crate::domain::{AbsPath, ExecutableError, FlakeDir, FlakePath, Uid, VmName};
    use crate::layout::{Env, Paths, Var};
    use crate::ports::Cause;
    use crate::ports::host_fs::FsOp;
    use crate::testing::{self, Entry, TempDir};

    const EMITTED: &str = include_str!("../../../tests/golden/contract_v1.json");

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }

    fn setup(label: &str) -> (TempDir, VmPaths) {
        let temp = TempDir::new(label).unwrap();
        let env = Env {
            home: Var::Text(format!("{}/h", temp.path().to_str().unwrap())),
            ..Env::UNSET
        };
        let paths = Paths::resolve(&env, Uid::parse(1000).unwrap()).unwrap();
        let vm = paths.vm(&VmName::parse("proj".to_owned()).unwrap());
        fs::create_dir_all(vm.state().get()).unwrap();
        (temp, vm)
    }

    fn raw(path: &AbsPath) -> &Path {
        Path::new(path.get())
    }

    /// A store-like tree beside the VM with the three outputs, and the out-link `vm` to it.
    fn plant(temp: &TempDir, vm: &VmPaths, mode: u32, contract: &str) {
        let farm = temp.path().join("store");
        let program = |dir: &str, name: &str| {
            let file = farm.join(dir).join("bin").join(name);
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(&file, b"#!/bin/sh\n").unwrap();
            fs::set_permissions(&file, fs::Permissions::from_mode(mode)).unwrap();
        };
        program("runner", "microvm-run");
        program("virtiofsd", "virtiofsd");
        fs::write(farm.join("contract"), contract).unwrap();
        symlink(&farm, raw(&vm.build_link())).unwrap();
    }

    fn seen(vm: &VmPaths) -> Result<BuildOutput, BuildFailure> {
        runtime().block_on(observe_output(&StdFs, vm))
    }

    fn code(result: Result<BuildOutput, BuildFailure>) -> &'static str {
        result.err().map_or("ok", |error| error.code())
    }

    #[test]
    fn a_build_with_executable_programs_and_the_contract_is_usable_and_resolved() {
        let (temp, vm) = setup("build-ok");
        plant(&temp, &vm, 0o755, EMITTED);
        let output = seen(&vm).unwrap();
        let store = fs::canonicalize(temp.path().join("store")).unwrap();
        assert_eq!(
            output.runner.path().get(),
            store.join("runner/bin/microvm-run").to_str().unwrap()
        );
        assert_eq!(
            output.virtiofsd.path().get(),
            store.join("virtiofsd/bin/virtiofsd").to_str().unwrap()
        );
        assert_eq!(output.contract, expected());
    }

    #[test]
    fn a_missing_out_link_is_a_missing_runner() {
        let (_temp, vm) = setup("build-none");
        assert_eq!(
            seen(&vm),
            Err(BuildFailure::Build(BuildError::Output {
                program: OutputProgram::Runner,
                error: ExecutableError::Missing {
                    path: vm.build_file(BuildFile::Runner)
                },
            }))
        );
    }

    #[test]
    fn a_dangling_out_link_is_a_missing_runner() {
        let (_temp, vm) = setup("build-dangling");
        symlink("/nonexistent/store", raw(&vm.build_link())).unwrap();
        assert_eq!(code(seen(&vm)), "build.output");
        let Err(BuildFailure::Build(BuildError::Output { error, .. })) = seen(&vm) else {
            return;
        };
        assert!(
            matches!(error, ExecutableError::Missing { .. }),
            "{error:?}"
        );
    }

    #[test]
    fn programs_without_an_execute_bit_are_not_executable() {
        let (temp, vm) = setup("build-mode");
        plant(&temp, &vm, 0o644, EMITTED);
        assert_eq!(
            seen(&vm),
            Err(BuildFailure::Build(BuildError::Output {
                program: OutputProgram::Runner,
                error: ExecutableError::NotExecutable {
                    path: vm.build_file(BuildFile::Runner)
                },
            }))
        );
    }

    #[test]
    fn a_dangling_program_link_is_missing_and_a_directory_is_not_a_file() {
        let (temp, vm) = setup("build-links");
        plant(&temp, &vm, 0o755, EMITTED);
        let bin = temp.path().join("store/virtiofsd/bin");
        fs::remove_file(bin.join("virtiofsd")).unwrap();
        symlink("/nonexistent/virtiofsd", bin.join("virtiofsd")).unwrap();
        let Err(BuildFailure::Build(BuildError::Output { program, error })) = seen(&vm) else {
            return;
        };
        assert_eq!(program, OutputProgram::Virtiofsd);
        assert!(
            matches!(error, ExecutableError::Missing { .. }),
            "{error:?}"
        );
        fs::remove_file(bin.join("virtiofsd")).unwrap();
        fs::create_dir(bin.join("virtiofsd")).unwrap();
        let Err(BuildFailure::Build(BuildError::Output { error, .. })) = seen(&vm) else {
            return;
        };
        assert!(
            matches!(error, ExecutableError::NotAFile { .. }),
            "{error:?}"
        );
    }

    #[test]
    fn the_contract_is_missing_a_directory_malformed_or_foreign() {
        let (temp, vm) = setup("build-contract");
        plant(&temp, &vm, 0o755, "{}");
        let contract = temp.path().join("store/contract");
        assert_eq!(code(seen(&vm)), "build.contract-invalid");
        fs::write(
            &contract,
            EMITTED.replacen("\"version\":1", "\"version\":9", 1),
        )
        .unwrap();
        let Err(BuildFailure::Build(BuildError::ContractMismatch { field, .. })) = seen(&vm) else {
            return;
        };
        assert_eq!(field, ContractField::Version);
        fs::remove_file(&contract).unwrap();
        assert_eq!(code(seen(&vm)), "build.contract-missing");
        fs::create_dir(&contract).unwrap();
        assert_eq!(code(seen(&vm)), "build.contract-not-a-file");
    }

    #[test]
    fn a_contract_that_cannot_be_read_is_a_read_failure() {
        let (temp, vm) = setup("build-unread");
        plant(&temp, &vm, 0o755, EMITTED);
        fs::set_permissions(
            temp.path().join("store/contract"),
            fs::Permissions::from_mode(0o000),
        )
        .unwrap();
        if rustix::process::geteuid().is_root() {
            return;
        }
        assert_eq!(code(seen(&vm)), "build.read");
    }

    /// A nix that answers fixed and remembers what it was asked.
    struct FakeNix {
        answer: Result<BuildStatus, NixError>,
        asked: Mutex<Vec<(FlakeDir, FlakePath, AbsPath)>>,
    }

    impl Nix for FakeNix {
        fn build(
            &self,
            flake: &FlakeDir,
            host_module: &FlakePath,
            out_link: &AbsPath,
        ) -> impl Future<Output = Result<BuildStatus, NixError>> + Send {
            if let Ok(mut asked) = self.asked.lock() {
                asked.push((flake.clone(), host_module.clone(), out_link.clone()));
            }
            ready(self.answer.clone())
        }
    }

    fn run_build(answer: Result<BuildStatus, NixError>, plant_first: bool) -> (&'static str, bool) {
        let (temp, vm) = setup("build-run");
        if plant_first {
            plant(&temp, &vm, 0o755, EMITTED);
        }
        let nix = FakeNix {
            answer,
            asked: Mutex::new(Vec::new()),
        };
        let result = runtime().block_on(build(&nix, &StdFs, &vm));
        let asked = nix.asked.lock().unwrap().clone();
        let expected_call = vec![(vm.nix_config(), vm.host_module(), vm.build_link())];
        (code(result), asked == expected_call)
    }

    fn cause(kind: io::ErrorKind, errno: i32) -> Cause {
        Cause {
            kind,
            errno: Some(errno),
        }
    }

    #[test]
    fn nix_is_asked_to_build_the_vms_config_into_its_out_link_and_every_ending_is_mapped() {
        let spawn = |kind, errno| {
            Err(NixError::Spawn {
                cause: cause(kind, errno),
            })
        };
        testing::table(
            &[
                ((Ok(BuildStatus::Built), true), ("ok", true)),
                ((Ok(BuildStatus::Built), false), ("build.output", true)),
                (
                    (Ok(BuildStatus::Failed { code: Some(1) }), true),
                    ("build.failed", true),
                ),
                (
                    (Ok(BuildStatus::Failed { code: None }), true),
                    ("build.failed", true),
                ),
                (
                    (spawn(io::ErrorKind::NotFound, 2), true),
                    ("build.nix-missing", true),
                ),
                (
                    (spawn(io::ErrorKind::PermissionDenied, 13), true),
                    ("nix.spawn", true),
                ),
            ],
            |(answer, planted)| run_build(answer.clone(), *planted),
        );
    }

    fn fs_error() -> FsError {
        FsError::Aborted {
            op: FsOp::Stat,
            path: AbsPath::parse("/d/proj/vm/runner/bin/microvm-run".to_owned()).unwrap(),
        }
    }

    fn failures() -> Vec<BuildFailure> {
        vec![
            BuildFailure::Build(BuildError::NixMissing),
            BuildFailure::Nix(NixError::Spawn {
                cause: cause(io::ErrorKind::PermissionDenied, 13),
            }),
            BuildFailure::Probe(fs_error()),
            BuildFailure::Read(fs_error()),
        ]
    }

    fn witness(e: &BuildFailure) -> &'static str {
        match e {
            BuildFailure::Build(_) => "Build",
            BuildFailure::Nix(_) => "Nix",
            BuildFailure::Probe(_) => "Probe",
            BuildFailure::Read(_) => "Read",
        }
    }

    /// The wrapper's own codes (`build.probe`, `build.read`); the others forward.
    pub(crate) fn entries() -> Vec<Entry> {
        failures()
            .iter()
            .filter(|failure| matches!(failure, BuildFailure::Probe(_) | BuildFailure::Read(_)))
            .map(Entry::of)
            .collect()
    }

    #[test]
    fn every_failure_variant_has_a_table_case() {
        testing::every_variant(&failures(), witness, &["Build", "Nix", "Probe", "Read"]);
    }

    #[test]
    fn failures_forward_the_message_and_hint_of_what_failed() {
        for failure in failures() {
            let (message, hint) = match &failure {
                BuildFailure::Build(error) => (error.message(), error.hint()),
                BuildFailure::Nix(error) => (error.message(), error.hint()),
                BuildFailure::Probe(error) | BuildFailure::Read(error) => {
                    (error.message(), error.hint())
                }
            };
            assert_eq!(failure.message(), message);
            assert_eq!(failure.hint(), hint);
        }
    }
}
