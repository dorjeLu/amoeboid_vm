//! What a finished build is worth (design §Contract, §Observed inputs): the observations of the
//! out-link in, a [`BuildOutput`] or the first reason it is unusable out. Pure; `super` looks.

use crate::contract::{BuildError, Contract, OutputProgram, check};
use crate::domain::{Executable, Observation};
use crate::layout::{BuildFile, VmPaths};

/// A build the launcher can use: both programs exist and are executable, and the contract is
/// the one this library implements. The programs are the resolved store paths.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct BuildOutput {
    pub runner: Executable,
    pub virtiofsd: Executable,
    pub contract: Contract,
}

/// What was at the contract's path.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ContractInput {
    Missing,
    /// A directory, socket or the like.
    NotAFile,
    /// The file's content.
    Bytes(Vec<u8>),
}

/// The answers to probing the out-link's entries.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Seen {
    pub runner: Observation,
    pub virtiofsd: Observation,
    pub contract: ContractInput,
}

fn program(
    vm: &VmPaths,
    which: OutputProgram,
    file: BuildFile,
    seen: Observation,
) -> Result<Executable, BuildError> {
    Executable::observe(&vm.build_file(file), |_| seen).map_err(|error| BuildError::Output {
        program: which,
        error,
    })
}

/// The first thing wrong with the build, in the order runner, virtiofsd, contract (present,
/// well formed, and equal to the library's).
///
/// # Errors
/// The [`BuildError`] naming it.
pub fn assemble(vm: &VmPaths, seen: Seen) -> Result<BuildOutput, BuildError> {
    let runner = program(vm, OutputProgram::Runner, BuildFile::Runner, seen.runner)?;
    let virtiofsd = program(
        vm,
        OutputProgram::Virtiofsd,
        BuildFile::Virtiofsd,
        seen.virtiofsd,
    )?;
    let at = vm.build_file(BuildFile::Contract);
    let contract = match seen.contract {
        ContractInput::Missing => return Err(BuildError::ContractMissing { at }),
        ContractInput::NotAFile => return Err(BuildError::ContractNotAFile { at }),
        ContractInput::Bytes(bytes) => {
            Contract::parse(&bytes).map_err(|error| BuildError::ContractInvalid { at, error })?
        }
    };
    check(&contract)?;
    Ok(BuildOutput {
        runner,
        virtiofsd,
        contract,
    })
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::contract::{ContractField, ContractParseError, expected};
    use crate::domain::{AbsDir, AbsPath, ContractVersion, ExecutableError, Uid, VmName};
    use crate::layout::{Env, Paths, Var};
    use crate::testing;

    const EMITTED: &str = include_str!("../../../tests/golden/contract_v1.json");

    fn vm() -> VmPaths {
        let env = Env {
            home: Var::Text("/home/u".to_owned()),
            ..Env::UNSET
        };
        Paths::resolve(&env, Uid::parse(1000).unwrap())
            .unwrap()
            .vm(&VmName::parse("proj".to_owned()).unwrap())
    }

    fn path(text: &str) -> AbsPath {
        AbsPath::parse(text.to_owned()).unwrap()
    }

    fn file(text: &str, executable: bool) -> Observation {
        Observation::File {
            resolved: path(text),
            executable,
        }
    }

    const RUNNER: &str = "/home/u/.local/share/amoeboid/proj/vm/runner/bin/microvm-run";
    const VIRTIOFSD: &str = "/home/u/.local/share/amoeboid/proj/vm/virtiofsd/bin/virtiofsd";
    const CONTRACT: &str = "/home/u/.local/share/amoeboid/proj/vm/contract";

    fn good() -> Seen {
        Seen {
            runner: file("/nix/store/a-runner/bin/microvm-run", true),
            virtiofsd: file("/nix/store/b-virtiofsd/bin/virtiofsd", true),
            contract: ContractInput::Bytes(EMITTED.as_bytes().to_vec()),
        }
    }

    fn outcome(seen: Seen) -> Result<(String, String), BuildError> {
        assemble(&vm(), seen).map(|output| {
            assert_eq!(output.contract, expected());
            (
                output.runner.path().get().clone(),
                output.virtiofsd.path().get().clone(),
            )
        })
    }

    fn not_executable(program: OutputProgram, error: ExecutableError) -> BuildError {
        BuildError::Output { program, error }
    }

    #[test]
    fn a_good_build_yields_the_resolved_programs_and_the_contract() {
        assert_eq!(
            outcome(good()),
            Ok((
                "/nix/store/a-runner/bin/microvm-run".to_owned(),
                "/nix/store/b-virtiofsd/bin/virtiofsd".to_owned()
            ))
        );
    }

    #[test]
    fn each_program_must_be_there_a_file_and_executable() {
        let dir = Observation::Directory {
            resolved: AbsDir::parse("/nix/store/x".to_owned()).unwrap(),
        };
        let runner = |seen: Observation| Seen {
            runner: seen,
            ..good()
        };
        let virtiofsd = |seen: Observation| Seen {
            virtiofsd: seen,
            ..good()
        };
        let bad_runner = |error| Err(not_executable(OutputProgram::Runner, error));
        let bad_virtiofsd = |error| Err(not_executable(OutputProgram::Virtiofsd, error));
        testing::table(
            &[
                (
                    runner(Observation::Missing),
                    bad_runner(ExecutableError::Missing { path: path(RUNNER) }),
                ),
                (
                    runner(file("/nix/store/a/bin/microvm-run", false)),
                    bad_runner(ExecutableError::NotExecutable { path: path(RUNNER) }),
                ),
                (
                    runner(dir.clone()),
                    bad_runner(ExecutableError::NotAFile { path: path(RUNNER) }),
                ),
                (
                    runner(Observation::Other),
                    bad_runner(ExecutableError::NotAFile { path: path(RUNNER) }),
                ),
                (
                    virtiofsd(Observation::Missing),
                    bad_virtiofsd(ExecutableError::Missing {
                        path: path(VIRTIOFSD),
                    }),
                ),
                (
                    virtiofsd(file("/nix/store/b/bin/virtiofsd", false)),
                    bad_virtiofsd(ExecutableError::NotExecutable {
                        path: path(VIRTIOFSD),
                    }),
                ),
                (
                    virtiofsd(dir),
                    bad_virtiofsd(ExecutableError::NotAFile {
                        path: path(VIRTIOFSD),
                    }),
                ),
            ],
            |seen| outcome(seen.clone()),
        );
    }

    #[test]
    fn the_contract_must_be_a_file_a_contract_and_the_librarys() {
        let with = |contract| Seen { contract, ..good() };
        let other_version = EMITTED.replacen("\"version\":1", "\"version\":2", 1);
        let invalid = |bytes: &[u8]| {
            Err(BuildError::ContractInvalid {
                at: path(CONTRACT),
                error: Contract::parse(bytes).unwrap_err(),
            })
        };
        testing::table(
            &[
                (
                    with(ContractInput::Missing),
                    Err(BuildError::ContractMissing { at: path(CONTRACT) }),
                ),
                (
                    with(ContractInput::NotAFile),
                    Err(BuildError::ContractNotAFile { at: path(CONTRACT) }),
                ),
                (with(ContractInput::Bytes(b"{".to_vec())), invalid(b"{")),
                (with(ContractInput::Bytes(vec![])), invalid(b"")),
                (
                    with(ContractInput::Bytes(other_version.into_bytes())),
                    Err(BuildError::ContractMismatch {
                        field: ContractField::Version,
                        expected: expected().value(ContractField::Version),
                        found: crate::contract::ContractValue::Version(
                            ContractVersion::parse(2).unwrap(),
                        ),
                    }),
                ),
            ],
            |seen| outcome(seen.clone()),
        );
        assert!(matches!(
            Contract::parse(b"{"),
            Err(ContractParseError::Invalid { .. })
        ));
    }

    #[test]
    fn the_first_problem_in_order_wins() {
        let seen = Seen {
            runner: Observation::Missing,
            virtiofsd: Observation::Missing,
            contract: ContractInput::Missing,
        };
        assert_eq!(
            outcome(seen),
            Err(not_executable(
                OutputProgram::Runner,
                ExecutableError::Missing { path: path(RUNNER) }
            ))
        );
        let seen = Seen {
            virtiofsd: Observation::Missing,
            contract: ContractInput::Missing,
            ..good()
        };
        assert_eq!(
            outcome(seen),
            Err(not_executable(
                OutputProgram::Virtiofsd,
                ExecutableError::Missing {
                    path: path(VIRTIOFSD)
                }
            ))
        );
    }
}
