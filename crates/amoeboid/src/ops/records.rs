//! Reading and writing the host-owned records `run` and `mounts` of a VM on a [`HostFs`]. Writes
//! are whole and atomic (readers see the old file or the new); reads are strict, and a missing
//! file is `None`, never an empty record.

use crate::diagnostic::Diagnostic;
use crate::domain::{AbsPath, FileMode, Observation};
use crate::layout::VmPaths;
use crate::plan::MountsTable;
use crate::ports::host_fs::{FsError, HostFs};
use crate::records::{LastShares, LastSharesError, Run, RunError};
use crate::refined;

/// Why a record's text could not be had.
enum Unread {
    Probe(FsError),
    Read(FsError),
    NotUtf8,
}

/// The text of the file at `file`; `None` when nothing is there (anything else is read, so a
/// directory at the path is a read error).
async fn text(fs: &impl HostFs, file: &AbsPath) -> Result<Option<String>, Unread> {
    match fs.observe(file).await.map_err(Unread::Probe)? {
        Observation::Missing => Ok(None),
        Observation::File { .. } | Observation::Directory { .. } | Observation::Other => {
            let bytes = fs.read_file(file).await.map_err(Unread::Read)?;
            String::from_utf8(bytes)
                .map(Some)
                .map_err(|_| Unread::NotUtf8)
        }
    }
}

/// What went wrong with the `run` record (`file` is always in the message).
#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum RunFileError {
    #[error(transparent)]
    Probe(FsError),
    #[error(transparent)]
    Read(FsError),
    #[error("{file}: the file isn't UTF-8 text")]
    NotUtf8 { file: AbsPath },
    #[error("{file}: {error}")]
    Invalid { file: AbsPath, error: Box<RunError> },
    #[error(transparent)]
    Write(FsError),
}

impl Diagnostic for RunFileError {
    fn code(&self) -> &'static str {
        match self {
            Self::Probe(_) => "run-file.probe",
            Self::Read(_) => "run-file.read",
            Self::NotUtf8 { .. } => "run-file.not-utf8",
            Self::Invalid { .. } => "run-file.invalid",
            Self::Write(_) => "run-file.write",
        }
    }

    fn message(&self) -> String {
        self.to_string()
    }

    fn hint(&self) -> Option<String> {
        match self {
            Self::Probe(error) | Self::Read(error) | Self::Write(error) => error.hint(),
            Self::NotUtf8 { .. } => {
                Some("delete the file once no process of the VM is left".to_owned())
            }
            Self::Invalid { error, .. } => error.hint(),
        }
    }
}

/// The run record of the VM, `None` when it has none.
///
/// # Errors
///
/// The file can't be read, isn't text, or isn't a valid record of this VM.
pub async fn load_run(fs: &impl HostFs, vm: &VmPaths) -> Result<Option<Run>, RunFileError> {
    let file = vm.run();
    let Some(text) = text(fs, &file).await.map_err(|unread| match unread {
        Unread::Probe(error) => RunFileError::Probe(error),
        Unread::Read(error) => RunFileError::Read(error),
        Unread::NotUtf8 => RunFileError::NotUtf8 { file: file.clone() },
    })?
    else {
        return Ok(None);
    };
    Run::parse(&text, vm)
        .map(Some)
        .map_err(|error| RunFileError::Invalid {
            file,
            error: Box::new(error),
        })
}

/// Replaces the run record atomically. The VM's state directory must exist.
///
/// # Errors
///
/// The file can't be written.
pub async fn save_run(fs: &impl HostFs, vm: &VmPaths, run: &Run) -> Result<(), RunFileError> {
    fs.write_file(
        &vm.run(),
        run.render().into_bytes(),
        refined!(FileMode, 0o644),
    )
    .await
    .map_err(RunFileError::Write)
}

/// What went wrong with the `mounts` record (`file` is always in the message, `line` in `error`).
#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum MountsFileError {
    #[error(transparent)]
    Probe(FsError),
    #[error(transparent)]
    Read(FsError),
    #[error("{file}: the file isn't UTF-8 text")]
    NotUtf8 { file: AbsPath },
    #[error("{file}: {error}")]
    Invalid {
        file: AbsPath,
        error: LastSharesError,
    },
    #[error(transparent)]
    Write(FsError),
}

impl Diagnostic for MountsFileError {
    fn code(&self) -> &'static str {
        match self {
            Self::Probe(_) => "mounts-file.probe",
            Self::Read(_) => "mounts-file.read",
            Self::NotUtf8 { .. } => "mounts-file.not-utf8",
            Self::Invalid { .. } => "mounts-file.invalid",
            Self::Write(_) => "mounts-file.write",
        }
    }

    fn message(&self) -> String {
        self.to_string()
    }

    fn hint(&self) -> Option<String> {
        match self {
            Self::Probe(error) | Self::Read(error) | Self::Write(error) => error.hint(),
            Self::NotUtf8 { .. } => {
                Some("fix the file, or delete it to forget the last shares".to_owned())
            }
            Self::Invalid { error, .. } => error.hint(),
        }
    }
}

/// The shares of the VM's last start, `None` when none were recorded.
///
/// # Errors
///
/// The file can't be read, isn't text, or has a malformed line.
pub async fn load_last_shares(
    fs: &impl HostFs,
    vm: &VmPaths,
) -> Result<Option<LastShares>, MountsFileError> {
    let file = vm.last_mounts();
    let Some(text) = text(fs, &file).await.map_err(|unread| match unread {
        Unread::Probe(error) => MountsFileError::Probe(error),
        Unread::Read(error) => MountsFileError::Read(error),
        Unread::NotUtf8 => MountsFileError::NotUtf8 { file: file.clone() },
    })?
    else {
        return Ok(None);
    };
    LastShares::parse(&text)
        .map(Some)
        .map_err(|error| MountsFileError::Invalid { file, error })
}

/// Records `table` (the one the guest gets) as the VM's last shares, atomically. The VM's state
/// directory must exist.
///
/// # Errors
///
/// The file can't be written.
pub async fn save_last_shares(
    fs: &impl HostFs,
    vm: &VmPaths,
    table: &MountsTable,
) -> Result<(), MountsFileError> {
    fs.write_file(
        &vm.last_mounts(),
        table.as_str().as_bytes().to_vec(),
        refined!(FileMode, 0o644),
    )
    .await
    .map_err(MountsFileError::Write)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[allow(
    clippy::disallowed_types,
    reason = "tests plant files on a real filesystem"
)]
pub(crate) mod tests {
    use std::fs;
    use std::os::unix::fs::{MetadataExt as _, symlink};

    use super::*;
    use crate::adapters::StdFs;
    use crate::domain::{
        ContractVersion, LoopbackPort, Mode, MountRequest, Pid, ProcIdentity, ProgramName,
        StartTime, Uid, VmName,
    };
    use crate::layout::{Env, Paths, Var};
    use crate::plan::ShareRole;
    use crate::plan::fixtures::{abs, list_of, shares};
    use crate::ports::Cause;
    use crate::ports::host_fs::FsOp;
    use crate::testing::{self, Entry, TempDir};

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
    }

    /// The VM `proj` with its state directory made inside `temp`.
    fn vm(temp: &TempDir) -> VmPaths {
        let root = temp.path().to_str().unwrap();
        let var = |dir: &str| Var::Text(format!("{root}/{dir}"));
        let paths = Paths::resolve(
            &Env {
                home: var("home"),
                xdg_data_home: var("data"),
                xdg_runtime_dir: Var::Text("/run/user/1000".to_owned()),
                ..Env::UNSET
            },
            Uid::parse(1000).unwrap(),
        )
        .unwrap();
        let vm = paths.vm(&VmName::parse("proj".to_owned()).unwrap());
        fs::create_dir_all(vm.state().get()).unwrap();
        vm
    }

    fn raw(path: &AbsPath) -> &std::path::Path {
        std::path::Path::new(path.get())
    }

    fn proc(pid: u32) -> ProcIdentity {
        ProcIdentity {
            pid: Pid::parse(pid).unwrap(),
            start: StartTime::parse(77).unwrap(),
            program: ProgramName::parse("qemu".to_owned()).unwrap(),
        }
    }

    fn run_of(vm: &VmPaths) -> Run {
        Run::new(
            vm,
            proc(10),
            vec![(ShareRole::State, proc(11))],
            LoopbackPort::parse(2222).unwrap(),
            ContractVersion::parse(1).unwrap(),
            Some(vm.console_log()),
        )
        .unwrap()
    }

    fn fs_error() -> FsError {
        FsError::Io {
            op: FsOp::Read,
            path: AbsPath::parse("/d/proj/run".to_owned()).unwrap(),
            entry: None,
            cause: Cause {
                kind: std::io::ErrorKind::PermissionDenied,
                errno: Some(13),
            },
        }
    }

    fn run_witness(e: &RunFileError) -> &'static str {
        match e {
            RunFileError::Probe(_) => "Probe",
            RunFileError::Read(_) => "Read",
            RunFileError::NotUtf8 { .. } => "NotUtf8",
            RunFileError::Invalid { .. } => "Invalid",
            RunFileError::Write(_) => "Write",
        }
    }

    fn mounts_witness(e: &MountsFileError) -> &'static str {
        match e {
            MountsFileError::Probe(_) => "Probe",
            MountsFileError::Read(_) => "Read",
            MountsFileError::NotUtf8 { .. } => "NotUtf8",
            MountsFileError::Invalid { .. } => "Invalid",
            MountsFileError::Write(_) => "Write",
        }
    }

    fn file(text: &str) -> AbsPath {
        AbsPath::parse(text.to_owned()).unwrap()
    }

    fn run_errors() -> Vec<RunFileError> {
        vec![
            RunFileError::Probe(fs_error()),
            RunFileError::Read(fs_error()),
            RunFileError::NotUtf8 {
                file: file("/d/proj/run"),
            },
            RunFileError::Invalid {
                file: file("/d/proj/run"),
                error: Box::new(RunError::Missing {
                    at: "vm".to_owned(),
                }),
            },
            RunFileError::Write(fs_error()),
        ]
    }

    fn mounts_errors() -> Vec<MountsFileError> {
        vec![
            MountsFileError::Probe(fs_error()),
            MountsFileError::Read(fs_error()),
            MountsFileError::NotUtf8 {
                file: file("/d/proj/mounts"),
            },
            MountsFileError::Invalid {
                file: file("/d/proj/mounts"),
                error: LastSharesError::Fields { line: 2, found: 1 },
            },
            MountsFileError::Write(fs_error()),
        ]
    }

    /// One catalogue entry per variant.
    pub(crate) fn entries() -> Vec<Entry> {
        let run = run_errors().iter().map(Entry::of).collect::<Vec<_>>();
        let mounts = mounts_errors().iter().map(Entry::of).collect::<Vec<_>>();
        [run, mounts].concat()
    }

    #[test]
    fn every_error_variant_has_a_table_case() {
        let names = &["Probe", "Read", "NotUtf8", "Invalid", "Write"];
        testing::every_variant(&run_errors(), run_witness, names);
        testing::every_variant(&mounts_errors(), mounts_witness, names);
    }

    #[test]
    fn errors_name_the_file_and_the_line() {
        let mounts = MountsFileError::Invalid {
            file: file("/d/proj/mounts"),
            error: LastSharesError::Fields { line: 2, found: 1 },
        };
        assert_eq!(
            mounts.message(),
            "/d/proj/mounts: line 2: expected 3 tab-separated fields, found 1"
        );
        let run = RunFileError::Invalid {
            file: file("/d/proj/run"),
            error: Box::new(RunError::Missing {
                at: "vm".to_owned(),
            }),
        };
        assert_eq!(run.message(), "/d/proj/run: vm: missing");
    }

    #[test]
    fn a_saved_run_loads_back_equal_and_a_missing_one_is_none() {
        let temp = TempDir::new("rec-run").unwrap();
        let vm = vm(&temp);
        let run = run_of(&vm);
        runtime().block_on(async {
            assert_eq!(load_run(&StdFs, &vm).await, Ok(None));
            save_run(&StdFs, &vm, &run).await.unwrap();
            assert_eq!(load_run(&StdFs, &vm).await, Ok(Some(run.clone())));
        });
        assert_eq!(fs::read_to_string(raw(&vm.run())).unwrap(), run.render());
        assert_eq!(fs::metadata(raw(&vm.run())).unwrap().mode() & 0o7777, 0o644);
    }

    #[test]
    fn saving_a_run_replaces_the_file_whole_even_through_a_planted_symlink() {
        let temp = TempDir::new("rec-run-replace").unwrap();
        let vm = vm(&temp);
        let victim = temp.path().join("victim");
        fs::write(&victim, b"keep").unwrap();
        symlink(&victim, raw(&vm.run())).unwrap();
        let run = run_of(&vm);
        runtime().block_on(async {
            save_run(&StdFs, &vm, &run).await.unwrap();
            assert_eq!(load_run(&StdFs, &vm).await, Ok(Some(run.clone())));
        });
        assert_eq!(fs::read(&victim).unwrap(), b"keep");
        let leftovers: Vec<String> = fs::read_dir(raw(&vm.run()).parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .filter(|name| name.contains(".tmp"))
            .collect();
        assert_eq!(leftovers, Vec::<String>::new());
    }

    #[test]
    fn a_corrupt_run_record_is_an_error_naming_the_file() {
        let temp = TempDir::new("rec-run-bad").unwrap();
        let vm = vm(&temp);
        let load = || runtime().block_on(load_run(&StdFs, &vm));
        fs::write(raw(&vm.run()), "vm other\n").unwrap();
        assert!(matches!(
            load(),
            Err(RunFileError::Invalid { file, .. }) if file == vm.run()
        ));
        fs::write(raw(&vm.run()), [0xff, 0xfe]).unwrap();
        assert_eq!(load(), Err(RunFileError::NotUtf8 { file: vm.run() }));
        fs::remove_file(raw(&vm.run())).unwrap();
        fs::create_dir(raw(&vm.run())).unwrap();
        assert!(matches!(load(), Err(RunFileError::Read(_))));
    }

    #[test]
    fn saved_last_shares_load_back_as_the_requests_and_a_missing_file_is_none() {
        let temp = TempDir::new("rec-mounts").unwrap();
        let vm = vm(&temp);
        let table =
            list_of(&shares(&[("/srv/a b", Mode::Rw), ("/srv/c", Mode::Ro)])).mounts_table();
        runtime().block_on(async {
            assert_eq!(load_last_shares(&StdFs, &vm).await, Ok(None));
            save_last_shares(&StdFs, &vm, &table).await.unwrap();
            let loaded = load_last_shares(&StdFs, &vm).await.unwrap().unwrap();
            let expected = vec![
                MountRequest {
                    path: abs("/srv/a b"),
                    mode: Mode::Rw,
                },
                MountRequest {
                    path: abs("/srv/c"),
                    mode: Mode::Ro,
                },
            ];
            assert_eq!(loaded.as_slice(), expected.as_slice());
        });
        assert_eq!(
            fs::read_to_string(raw(&vm.last_mounts())).unwrap(),
            "d1\trw\t/srv/a b\nd2\tro\t/srv/c\n"
        );
        assert_eq!(
            fs::metadata(raw(&vm.last_mounts())).unwrap().mode() & 0o7777,
            0o644
        );
    }

    #[test]
    fn a_python_written_mounts_file_loads() {
        let temp = TempDir::new("rec-mounts-py").unwrap();
        let vm = vm(&temp);
        fs::write(
            raw(&vm.last_mounts()),
            "d1\trw\t/srv/a:b\nd2\tro\t/srv/k=v\n",
        )
        .unwrap();
        let loaded = runtime()
            .block_on(load_last_shares(&StdFs, &vm))
            .unwrap()
            .unwrap();
        assert_eq!(loaded.len(), 2);
    }

    #[test]
    fn a_corrupt_mounts_file_is_an_error_naming_the_file_and_line() {
        let temp = TempDir::new("rec-mounts-bad").unwrap();
        let vm = vm(&temp);
        let load = || runtime().block_on(load_last_shares(&StdFs, &vm));
        fs::write(raw(&vm.last_mounts()), "d1\trw\t/a\nd2\tbogus\t/b\n").unwrap();
        assert_eq!(
            load(),
            Err(MountsFileError::Invalid {
                file: vm.last_mounts(),
                error: LastSharesError::Mode {
                    line: 2,
                    found: "bogus".to_owned()
                },
            })
        );
        fs::write(raw(&vm.last_mounts()), [b'd', 0xff]).unwrap();
        assert_eq!(
            load(),
            Err(MountsFileError::NotUtf8 {
                file: vm.last_mounts()
            })
        );
        fs::remove_file(raw(&vm.last_mounts())).unwrap();
        fs::create_dir(raw(&vm.last_mounts())).unwrap();
        assert!(matches!(load(), Err(MountsFileError::Read(_))));
    }

    #[test]
    fn writing_into_a_missing_state_dir_is_a_write_error() {
        let temp = TempDir::new("rec-nodir").unwrap();
        let vm = vm(&temp);
        fs::remove_dir_all(raw(&vm.run()).parent().unwrap()).unwrap();
        let run = run_of(&vm);
        let table = list_of(&shares(&[])).mounts_table();
        runtime().block_on(async {
            assert!(matches!(
                save_run(&StdFs, &vm, &run).await,
                Err(RunFileError::Write(_))
            ));
            assert!(matches!(
                save_last_shares(&StdFs, &vm, &table).await,
                Err(MountsFileError::Write(_))
            ));
        });
    }
}
