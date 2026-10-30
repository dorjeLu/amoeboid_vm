//! The NixOS configuration of a VM: resolving its base, making or replacing its host-side copy,
//! and reading that copy for the guest (design §Configs are directories; the copies the guest gets
//! are `plan::guest_copies`, delivered by the start).
//!
//! ```text
//! preset / path  --new VM, or --base-nix-config-->  <state>/nix/   (host-owned, what is built)
//! ```
//!
//! The host never reads anything back from the VM's own copy in the guest.

mod decide;

pub use decide::{BaseError, Change, Need, Origin, Target, need, own_exists, source, target};

#[cfg(test)]
pub(crate) use decide::tests as decide_tests;

use crate::diagnostic::Diagnostic;
use crate::domain::{AbsDir, BaseSpec, NixSource, TreeFile};
use crate::layout::{Paths, VmPaths};
use crate::ports::host_fs::{FsError, HostFs, Replacement};

/// Why the base could not be settled: it is unusable, or could not be looked at.
#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum ObserveError {
    #[error(transparent)]
    Base(BaseError),
    #[error(transparent)]
    Probe(FsError),
}

impl Diagnostic for ObserveError {
    fn code(&self) -> &'static str {
        match self {
            Self::Base(error) => error.code(),
            Self::Probe(_) => "nix-config.probe",
        }
    }

    fn message(&self) -> String {
        self.to_string()
    }

    fn hint(&self) -> Option<String> {
        match self {
            Self::Base(error) => error.hint(),
            Self::Probe(error) => error.hint(),
        }
    }
}

/// A non-fatal outcome of [`prepare`].
#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum NixConfigWarning {
    /// The filesystem can't swap two directories, so the copy was replaced in two steps; a crash
    /// between them leaves no `nix/` (the old one stays beside it as `.nix.old`).
    #[error("{dir} was replaced in two steps: the filesystem can't swap directories")]
    NonAtomicReplace { dir: AbsDir },
}

impl Diagnostic for NixConfigWarning {
    fn code(&self) -> &'static str {
        match self {
            Self::NonAtomicReplace { .. } => "nix-config.non-atomic-replace",
        }
    }

    fn message(&self) -> String {
        self.to_string()
    }

    fn hint(&self) -> Option<String> {
        Some(
            "keep the data directory on a filesystem that can swap directories (ext4, xfs, \
             btrfs, tmpfs)"
                .to_owned(),
        )
    }
}

/// Settles which base the VM's config copy changes to, looking at the VM's copy and, where one is
/// needed, at the base (the directory and its `flake.nix`; symlinks resolved). `explicit` is
/// `--base-nix-config`, `configured` the config's `base-nix-config`. A VM that keeps its copy
/// needs no base, so a missing default preset is no problem for it.
///
/// # Errors
/// The needed base is unusable ([`BaseError`]), or a path could not be looked at.
pub async fn observe(
    fs: &impl HostFs,
    paths: &Paths,
    vm: &VmPaths,
    explicit: Option<&BaseSpec>,
    configured: &BaseSpec,
) -> Result<Change, ObserveError> {
    let own = fs
        .observe(&vm.nix_config().abs().to_path())
        .await
        .map_err(ObserveError::Probe)?;
    let wanted = need(own_exists(&own), explicit, configured);
    let (Need::Create(spec) | Need::Rebase(spec)) = wanted else {
        return Ok(Change::Keep);
    };
    let target = target(&paths.presets_dir(), spec);
    let dir = fs
        .observe(&target.dir.to_path())
        .await
        .map_err(ObserveError::Probe)?;
    let flake = fs
        .observe(&NixSource::flake_of(&target.dir))
        .await
        .map_err(ObserveError::Probe)?;
    let base = source(&target, dir, &flake).map_err(ObserveError::Base)?;
    Ok(match wanted {
        Need::Rebase(_) => Change::Rebase(base),
        Need::Keep | Need::Create(_) => Change::Create(base),
    })
}

/// The VM's config copy after a start.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Prepared {
    pub origin: Origin,
    /// The regular files of `<state>/nix/`.
    pub tree: Vec<TreeFile>,
    pub warnings: Vec<NixConfigWarning>,
}

/// Makes or replaces the VM's config copy as `change` says (atomically where the filesystem
/// allows), then reads it. The state dir must exist. The caller holds the VM lock.
///
/// # Errors
/// The copy can't be made or read.
pub async fn prepare(fs: &impl HostFs, vm: &VmPaths, change: &Change) -> Result<Prepared, FsError> {
    let own = vm.nix_config();
    let mut warnings = Vec::new();
    match change {
        Change::Keep => {}
        Change::Create(base) | Change::Rebase(base) => {
            match fs.replace_dir(own.abs(), base.path()).await? {
                Replacement::Atomic => {}
                Replacement::NonAtomic => warnings.push(NixConfigWarning::NonAtomicReplace {
                    dir: own.abs().clone(),
                }),
            }
        }
    }
    Ok(Prepared {
        origin: change.origin(),
        tree: fs.read_tree(own.abs()).await?,
        warnings,
    })
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[allow(
    clippy::disallowed_types,
    reason = "tests plant files on a real filesystem"
)]
pub(crate) mod tests {
    use std::fs;
    use std::future::Future;
    use std::os::unix::fs::symlink;

    use super::*;
    use crate::adapters::StdFs;
    use crate::domain::{
        AbsPath, ExistingDirError, FileMode, GuestRel, NixSourceError, Observation, PresetName,
        Uid, VmName,
    };
    use crate::layout::{Env, Var};
    use crate::testing::{self, Entry, TempDir};

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }

    fn setup(label: &str) -> (TempDir, Paths, VmPaths) {
        let temp = TempDir::new(label).unwrap();
        let env = Env {
            home: Var::Text(format!("{}/h", temp.path().to_str().unwrap())),
            ..Env::UNSET
        };
        let paths = Paths::resolve(&env, Uid::parse(1000).unwrap()).unwrap();
        let vm = paths.vm(&VmName::parse("proj".to_owned()).unwrap());
        fs::create_dir_all(vm.state().get()).unwrap();
        (temp, paths, vm)
    }

    fn put(dir: &str, files: &[(&str, &str)]) {
        for (name, text) in files {
            let path = format!("{dir}/{name}");
            if let Some((parent, _)) = path.rsplit_once('/') {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(path, text).unwrap();
        }
    }

    fn preset_dir(paths: &Paths, name: &str) -> String {
        format!("{}/{name}", paths.presets_dir().get())
    }

    fn spec(name: &str) -> BaseSpec {
        BaseSpec::Preset(PresetName::parse(name.to_owned()).unwrap())
    }

    fn dir_spec(path: &str) -> BaseSpec {
        BaseSpec::Dir(AbsDir::parse(path.to_owned()).unwrap())
    }

    fn look(
        paths: &Paths,
        vm: &VmPaths,
        explicit: Option<&BaseSpec>,
        configured: &BaseSpec,
    ) -> Result<Change, ObserveError> {
        runtime().block_on(observe(&StdFs, paths, vm, explicit, configured))
    }

    fn make(vm: &VmPaths, change: &Change) -> Result<Prepared, FsError> {
        runtime().block_on(prepare(&StdFs, vm, change))
    }

    fn files(prepared: &Prepared) -> Vec<(String, String)> {
        prepared
            .tree
            .iter()
            .map(|f| {
                (
                    f.rel.to_string(),
                    String::from_utf8(f.bytes.clone()).unwrap(),
                )
            })
            .collect()
    }

    fn pairs(items: &[(&str, &str)]) -> Vec<(String, String)> {
        items
            .iter()
            .map(|(a, b)| ((*a).to_owned(), (*b).to_owned()))
            .collect()
    }

    fn witness_observe(e: &ObserveError) -> &'static str {
        match e {
            ObserveError::Base(_) => "Base",
            ObserveError::Probe(_) => "Probe",
        }
    }

    fn observe_errors() -> Vec<ObserveError> {
        let path = AbsPath::parse("/c/nix_presets/default".to_owned()).unwrap();
        vec![
            ObserveError::Base(BaseError::DefaultPresetMissing {
                dir: AbsDir::parse("/c/nix_presets/default".to_owned()).unwrap(),
            }),
            ObserveError::Probe(FsError::Aborted {
                op: crate::ports::host_fs::FsOp::Stat,
                path,
            }),
        ]
    }

    fn witness_warning(e: &NixConfigWarning) -> &'static str {
        match e {
            NixConfigWarning::NonAtomicReplace { .. } => "NonAtomicReplace",
        }
    }

    fn warnings() -> Vec<NixConfigWarning> {
        vec![NixConfigWarning::NonAtomicReplace {
            dir: AbsDir::parse("/d/proj/nix".to_owned()).unwrap(),
        }]
    }

    /// One catalogue entry per variant (a wrapped base error keeps the entry of its cause).
    pub(crate) fn entries() -> Vec<Entry> {
        let probe = observe_errors().into_iter().nth(1);
        probe
            .iter()
            .map(Entry::of)
            .chain(warnings().iter().map(Entry::of))
            .collect()
    }

    #[test]
    fn every_error_and_warning_variant_has_a_table_case() {
        testing::every_variant(&observe_errors(), witness_observe, &["Base", "Probe"]);
        testing::every_variant(&warnings(), witness_warning, &["NonAtomicReplace"]);
    }

    #[test]
    fn a_wrapped_error_keeps_the_code_message_and_hint_of_its_cause() {
        let errors = observe_errors();
        let base = errors.first().unwrap();
        let inner = BaseError::DefaultPresetMissing {
            dir: AbsDir::parse("/c/nix_presets/default".to_owned()).unwrap(),
        };
        assert_eq!(
            (base.code(), base.message(), base.hint()),
            (inner.code(), inner.message(), inner.hint())
        );
        let probe = errors.get(1).unwrap();
        assert_eq!(probe.code(), "nix-config.probe");
        assert_eq!(
            probe.message(),
            "can't inspect /c/nix_presets/default: the operation was aborted"
        );
        assert_eq!(probe.hint().as_deref(), Some("run the command again"));
        let warning = warnings().remove(0);
        assert_eq!(
            warning.message(),
            "/d/proj/nix was replaced in two steps: the filesystem can't swap directories"
        );
    }

    #[test]
    fn a_new_vm_is_created_from_the_configured_preset_without_the_ignored_names() {
        let (_temp, paths, vm) = setup("nix-new");
        let preset = preset_dir(&paths, "default");
        put(
            &preset,
            &[
                ("flake.nix", "flake"),
                ("base.nix", "base"),
                ("sub/recipe.nix", "recipe"),
                (".git/config", "git"),
                ("result/x", "link"),
                ("sub/__pycache__/m.pyc", "py"),
            ],
        );
        let change = look(&paths, &vm, None, &spec("default")).unwrap();
        let resolved = fs::canonicalize(&preset).unwrap();
        assert!(
            matches!(&change, Change::Create(base) if base.path().get() == resolved.to_str().unwrap()),
            "{change:?}"
        );
        let prepared = make(&vm, &change).unwrap();
        assert_eq!(prepared.origin, Origin::Created);
        assert_eq!(prepared.warnings, Vec::new());
        assert_eq!(
            files(&prepared),
            pairs(&[
                ("base.nix", "base"),
                ("flake.nix", "flake"),
                ("sub/recipe.nix", "recipe")
            ])
        );
        assert_eq!(
            fs::read_to_string(format!("{}/base.nix", vm.nix_config().abs().get())).unwrap(),
            "base"
        );
    }

    #[test]
    fn an_existing_vm_keeps_its_copy_and_needs_no_preset_at_all() {
        let (_temp, paths, vm) = setup("nix-keep");
        put(
            vm.nix_config().abs().get(),
            &[("flake.nix", "mine"), ("x.nix", "x")],
        );
        // No presets directory exists, and the configured base is a preset that isn't there.
        let change = look(&paths, &vm, None, &spec("default")).unwrap();
        assert_eq!(change, Change::Keep);
        let prepared = make(&vm, &change).unwrap();
        assert_eq!(prepared.origin, Origin::Kept);
        assert_eq!(
            files(&prepared),
            pairs(&[("flake.nix", "mine"), ("x.nix", "x")])
        );
    }

    #[test]
    fn rebasing_replaces_the_copy_with_the_given_directory() {
        let (temp, paths, vm) = setup("nix-rebase");
        put(
            vm.nix_config().abs().get(),
            &[("flake.nix", "old"), ("old.nix", "o")],
        );
        let other = format!("{}/other", temp.path().to_str().unwrap());
        put(&other, &[("flake.nix", "new"), ("new.nix", "n")]);
        let explicit = dir_spec(&other);
        let change = look(&paths, &vm, Some(&explicit), &spec("default")).unwrap();
        assert!(matches!(change, Change::Rebase(_)), "{change:?}");
        let prepared = make(&vm, &change).unwrap();
        assert_eq!(prepared.origin, Origin::Rebased);
        assert_eq!(
            files(&prepared),
            pairs(&[("flake.nix", "new"), ("new.nix", "n")])
        );
    }

    #[test]
    fn an_explicit_base_creates_a_new_vm_whatever_the_configured_preset_is() {
        let (temp, paths, vm) = setup("nix-explicit");
        let other = format!("{}/other", temp.path().to_str().unwrap());
        put(&other, &[("flake.nix", "new")]);
        let explicit = dir_spec(&other);
        let change = look(&paths, &vm, Some(&explicit), &spec("default")).unwrap();
        assert!(matches!(change, Change::Create(_)), "{change:?}");
    }

    #[test]
    fn a_base_behind_a_symlink_is_used_at_its_resolved_path() {
        let (temp, paths, vm) = setup("nix-link");
        let real = format!("{}/real", temp.path().to_str().unwrap());
        put(&real, &[("flake.nix", "f")]);
        let link = format!("{}/link", temp.path().to_str().unwrap());
        symlink(&real, &link).unwrap();
        let explicit = dir_spec(&link);
        let change = look(&paths, &vm, Some(&explicit), &spec("default")).unwrap();
        assert!(
            matches!(&change, Change::Create(base) if base.path().get() == fs::canonicalize(&real).unwrap().to_str().unwrap()),
            "{change:?}"
        );
    }

    #[test]
    fn a_new_vm_without_the_default_preset_is_told_where_to_copy_the_reference_config() {
        let (_temp, paths, vm) = setup("nix-nodefault");
        let dir = AbsDir::parse(preset_dir(&paths, "default")).unwrap();
        let error = BaseError::DefaultPresetMissing { dir: dir.clone() };
        assert_eq!(
            look(&paths, &vm, None, &spec("default")),
            Err(ObserveError::Base(error.clone()))
        );
        assert!(
            error.hint().unwrap().contains(&format!("to {dir}")),
            "{:?}",
            error.hint()
        );
    }

    #[test]
    fn a_bad_base_is_refused_for_the_reason_that_applies() {
        let (temp, paths, vm) = setup("nix-bad");
        let base = temp.path().to_str().unwrap().to_owned();
        put(&preset_dir(&paths, "noflake"), &[("other.nix", "x")]);
        put(&preset_dir(&paths, "flakedir"), &[("flake.nix/inner", "x")]);
        fs::write(format!("{base}/afile"), "x").unwrap();
        let dir = |text: String| AbsDir::parse(text).unwrap();
        let resolved =
            |text: String| dir(fs::canonicalize(text).unwrap().to_str().unwrap().to_owned());
        let cases = [
            (
                spec("work"),
                BaseError::PresetMissing {
                    name: PresetName::parse("work".to_owned()).unwrap(),
                    dir: dir(preset_dir(&paths, "work")),
                },
            ),
            (
                spec("noflake"),
                BaseError::Source(NixSourceError::NoFlake {
                    dir: resolved(preset_dir(&paths, "noflake")),
                }),
            ),
            (
                spec("flakedir"),
                BaseError::Source(NixSourceError::NoFlake {
                    dir: resolved(preset_dir(&paths, "flakedir")),
                }),
            ),
            (
                dir_spec(&format!("{base}/absent")),
                BaseError::Source(NixSourceError::Dir(ExistingDirError::Missing {
                    path: dir(format!("{base}/absent")),
                })),
            ),
            (
                dir_spec(&format!("{base}/afile")),
                BaseError::Source(NixSourceError::Dir(ExistingDirError::NotADirectory {
                    path: dir(format!("{base}/afile")),
                })),
            ),
        ];
        for (index, (explicit, expected)) in cases.into_iter().enumerate() {
            assert_eq!(
                look(&paths, &vm, Some(&explicit), &spec("default")),
                Err(ObserveError::Base(expected)),
                "case {index}"
            );
        }
    }

    /// `StdFs`, except that it never manages to swap directories.
    struct NoSwap;

    impl HostFs for NoSwap {
        fn write_file(
            &self,
            path: &AbsPath,
            bytes: Vec<u8>,
            mode: FileMode,
        ) -> impl Future<Output = Result<(), FsError>> + Send {
            StdFs.write_file(path, bytes, mode)
        }

        fn observe(
            &self,
            path: &AbsPath,
        ) -> impl Future<Output = Result<Observation, FsError>> + Send {
            StdFs.observe(path)
        }

        fn read_file(
            &self,
            path: &AbsPath,
        ) -> impl Future<Output = Result<Vec<u8>, FsError>> + Send {
            StdFs.read_file(path)
        }

        fn file_mode(
            &self,
            path: &AbsPath,
        ) -> impl Future<Output = Result<FileMode, FsError>> + Send {
            StdFs.file_mode(path)
        }

        fn read_tree(
            &self,
            root: &AbsDir,
        ) -> impl Future<Output = Result<Vec<TreeFile>, FsError>> + Send {
            StdFs.read_tree(root)
        }

        fn remove_file(&self, path: &AbsPath) -> impl Future<Output = Result<(), FsError>> + Send {
            StdFs.remove_file(path)
        }

        fn clear_dir(&self, dir: &AbsDir) -> impl Future<Output = Result<(), FsError>> + Send {
            StdFs.clear_dir(dir)
        }

        fn create_dir_all(
            &self,
            dir: &AbsDir,
            mode: FileMode,
        ) -> impl Future<Output = Result<(), FsError>> + Send {
            StdFs.create_dir_all(dir, mode)
        }

        async fn replace_dir(
            &self,
            target: &AbsDir,
            source: &AbsDir,
        ) -> Result<Replacement, FsError> {
            StdFs.replace_dir(target, source).await?;
            Ok(Replacement::NonAtomic)
        }
    }

    #[test]
    fn a_replacement_that_was_not_atomic_is_a_warning_and_the_copy_is_still_made() {
        let (temp, paths, vm) = setup("nix-nonatomic");
        let other = format!("{}/other", temp.path().to_str().unwrap());
        put(&other, &[("flake.nix", "new")]);
        let explicit = dir_spec(&other);
        let change = look(&paths, &vm, Some(&explicit), &spec("default")).unwrap();
        let prepared = runtime().block_on(prepare(&NoSwap, &vm, &change)).unwrap();
        assert_eq!(prepared.warnings, warnings_for(&vm));
        assert_eq!(files(&prepared), pairs(&[("flake.nix", "new")]));
    }

    fn warnings_for(vm: &VmPaths) -> Vec<NixConfigWarning> {
        vec![NixConfigWarning::NonAtomicReplace {
            dir: vm.nix_config().abs().clone(),
        }]
    }

    #[test]
    fn a_copy_that_cannot_be_made_is_a_filesystem_error_and_changes_nothing() {
        let (temp, paths, vm) = setup("nix-fail");
        fs::remove_dir_all(vm.state().get()).unwrap();
        let other = format!("{}/other", temp.path().to_str().unwrap());
        put(&other, &[("flake.nix", "new")]);
        let explicit = dir_spec(&other);
        let change = look(&paths, &vm, Some(&explicit), &spec("default")).unwrap();
        let error = make(&vm, &change).unwrap_err();
        assert_eq!(error.code(), "fs.io");
        assert!(!std::path::Path::new(vm.state().get()).exists());
    }

    #[test]
    fn a_file_where_the_copy_should_be_is_not_a_copy_and_is_refused_not_replaced() {
        let (temp, paths, vm) = setup("nix-file");
        fs::write(vm.nix_config().abs().get(), "oops").unwrap();
        let other = format!("{}/other", temp.path().to_str().unwrap());
        put(&other, &[("flake.nix", "new")]);
        let explicit = dir_spec(&other);
        let change = look(&paths, &vm, Some(&explicit), &spec("default")).unwrap();
        assert!(matches!(change, Change::Create(_)), "{change:?}");
        assert_eq!(make(&vm, &change).unwrap_err().code(), "fs.io");
        assert_eq!(
            fs::read_to_string(vm.nix_config().abs().get()).unwrap(),
            "oops"
        );
    }

    #[test]
    fn the_copy_read_back_is_the_tree_of_regular_files() {
        let (_temp, paths, vm) = setup("nix-tree");
        put(vm.nix_config().abs().get(), &[("flake.nix", "f")]);
        let kept = look(&paths, &vm, None, &spec("default")).unwrap();
        let prepared = make(&vm, &kept).unwrap();
        assert_eq!(
            prepared.tree,
            vec![TreeFile {
                rel: GuestRel::parse("flake.nix".to_owned()).unwrap(),
                bytes: b"f".to_vec()
            }]
        );
    }
}
