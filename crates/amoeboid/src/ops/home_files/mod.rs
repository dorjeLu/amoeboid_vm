//! The files of the host user's home that go into the VM's home on every start (config key
//! `home-files`). Each is observed first, in the observe step; one that isn't a regular file is
//! skipped with a warning. Reading and delivering them is the start's.

mod decide;

pub use decide::{HomeFileWarning, Present, requested, split};

#[cfg(test)]
pub(crate) use decide::tests as decide_tests;

#[cfg(test)]
use crate::diagnostic::Diagnostic as _;
use crate::domain::{GuestDest, GuestFile, HomeDir, HomeRel};
use crate::ports::host_fs::{FsError, HostFs};

/// The configured files that exist, and the warnings for those that don't.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Observed {
    pub present: Vec<Present>,
    pub warnings: Vec<HomeFileWarning>,
}

/// Looks at every configured file under `home` (symlinks followed).
///
/// # Errors
/// A path could not be looked at.
pub async fn observe(
    fs: &impl HostFs,
    home: &HomeDir,
    files: &[HomeRel],
) -> Result<Observed, FsError> {
    let mut seen = Vec::with_capacity(files.len());
    for rel in files {
        let observation = fs.observe(&requested(home, rel)).await?;
        seen.push((rel.clone(), observation));
    }
    let (present, warnings) = split(home, seen);
    Ok(Observed { present, warnings })
}

/// Reads the present files with their permission bits, ready to deliver at the same place in the
/// guest's home.
///
/// # Errors
/// A file that was there a moment ago can't be read.
pub async fn load(fs: &impl HostFs, present: &[Present]) -> Result<Vec<GuestFile>, FsError> {
    let mut files = Vec::with_capacity(present.len());
    for Present { rel, file } in present {
        let bytes = fs.read_file(file.path()).await?;
        let mode = fs.file_mode(file.path()).await?;
        files.push(GuestDest::Home(rel.clone()).file(bytes, mode));
    }
    Ok(files)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[allow(
    clippy::disallowed_types,
    reason = "tests plant files on a real filesystem"
)]
mod tests {
    use std::fs;
    use std::os::unix::fs::{PermissionsExt as _, symlink};

    use super::*;
    use crate::adapters::StdFs;
    use crate::domain::{FileMode, GuestRel};
    use crate::testing::TempDir;

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }

    fn rel(text: &str) -> HomeRel {
        HomeRel::parse(text.to_owned()).unwrap()
    }

    fn home_of(temp: &TempDir) -> HomeDir {
        HomeDir::parse(format!("{}/h", temp.path().to_str().unwrap())).unwrap()
    }

    fn setup(label: &str) -> (TempDir, HomeDir) {
        let temp = TempDir::new(label).unwrap();
        let home = home_of(&temp);
        fs::create_dir_all(home.get()).unwrap();
        (temp, home)
    }

    fn put(home: &HomeDir, name: &str, text: &str, mode: u32) {
        let path = format!("{}/{name}", home.get());
        if let Some((parent, _)) = path.rsplit_once('/') {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&path, text).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
    }

    #[test]
    fn existing_files_are_resolved_and_the_others_warn_in_the_configured_order() {
        let (temp, home) = setup("home-observe");
        put(&home, ".gitconfig", "git", 0o644);
        fs::create_dir_all(format!("{}/.ssh", home.get())).unwrap();
        put(&home, "real/target", "t", 0o600);
        symlink(
            format!("{}/real/target", home.get()),
            format!("{}/linked", home.get()),
        )
        .unwrap();
        symlink(
            format!("{}/nowhere", home.get()),
            format!("{}/dangling", home.get()),
        )
        .unwrap();
        let files = [
            ".gitconfig",
            ".config/git/config",
            ".ssh",
            "linked",
            "dangling",
        ]
        .map(rel);
        let seen = runtime().block_on(observe(&StdFs, &home, &files)).unwrap();
        let resolved = |p: &Present| p.file.path().get().clone();
        let canonical = |name: &str| {
            fs::canonicalize(format!("{}/{name}", home.get()))
                .unwrap()
                .to_str()
                .unwrap()
                .to_owned()
        };
        assert_eq!(
            seen.present
                .iter()
                .map(|p| (p.rel.get().clone(), resolved(p)))
                .collect::<Vec<_>>(),
            [
                (".gitconfig".to_owned(), canonical(".gitconfig")),
                ("linked".to_owned(), canonical("real/target")),
            ]
        );
        assert_eq!(
            seen.warnings,
            vec![
                HomeFileWarning::HomeFileMissing {
                    rel: rel(".config/git/config")
                },
                HomeFileWarning::HomeFileMissing { rel: rel(".ssh") },
                HomeFileWarning::HomeFileMissing {
                    rel: rel("dangling")
                },
            ]
        );
        drop(temp);
    }

    #[test]
    fn nothing_configured_is_observed_as_nothing() {
        let (_temp, home) = setup("home-none");
        assert_eq!(
            runtime().block_on(observe(&StdFs, &home, &[])),
            Ok(Observed {
                present: Vec::new(),
                warnings: Vec::new()
            })
        );
    }

    #[test]
    fn loaded_files_carry_their_bytes_and_permission_bits_to_the_same_place_in_the_guest_home() {
        let (_temp, home) = setup("home-load");
        put(&home, ".gitconfig", "git", 0o640);
        put(&home, ".config/tool/x", "x", 0o755);
        let files = [rel(".gitconfig"), rel(".config/tool/x")];
        let rt = runtime();
        let observed = rt.block_on(observe(&StdFs, &home, &files)).unwrap();
        let loaded = rt.block_on(load(&StdFs, &observed.present)).unwrap();
        let guest = |dest: &str, text: &str, mode: u16| GuestFile {
            dest: GuestRel::parse(dest.to_owned()).unwrap(),
            bytes: text.as_bytes().to_vec(),
            mode: FileMode::parse(mode).unwrap(),
        };
        assert_eq!(
            loaded,
            [
                guest("home/.gitconfig", "git", 0o640),
                guest("home/.config/tool/x", "x", 0o755),
            ]
        );
    }

    #[test]
    fn a_file_that_vanishes_between_the_observation_and_the_read_is_a_read_error() {
        let (_temp, home) = setup("home-vanish");
        put(&home, ".gitconfig", "git", 0o644);
        let rt = runtime();
        let observed = rt
            .block_on(observe(&StdFs, &home, &[rel(".gitconfig")]))
            .unwrap();
        fs::remove_file(format!("{}/.gitconfig", home.get())).unwrap();
        let error = rt.block_on(load(&StdFs, &observed.present)).unwrap_err();
        assert_eq!(error.code(), "fs.io");
        assert!(
            error.message().starts_with("can't read "),
            "{}",
            error.message()
        );
    }
}
