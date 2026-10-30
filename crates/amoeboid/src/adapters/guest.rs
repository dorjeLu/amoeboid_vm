//! `ShareDelivery`: [`GuestDelivery`] on the guest-writable share, the directory the VM can
//! write anything into. Everything the guest left there is hostile input:
//!
//! * every directory is entered with `openat2(RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS |
//!   RESOLVE_NO_MAGICLINKS)` from the descriptor of the one before, starting at the share root
//!   (itself opened with `O_NOFOLLOW`), so a symlink swapped in at any depth is refused by the
//!   kernel, not by a check that could race;
//! * a file is written to a fresh `O_EXCL | O_NOFOLLOW` temp name in its directory and renamed
//!   over its name, so the existing entry (a symlink, a hardlink to a host file) is replaced and
//!   never opened;
//! * an old tree is renamed into the VM's trash (outside the share, same filesystem) and deleted
//!   there with std's symlink-safe `remove_dir_all`;
//! * nothing that is read from the share is used to decide anything.

use std::fs::{self, File};
use std::future::Future;
use std::io::Write as _;
use std::os::fd::OwnedFd;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use rustix::fs::{
    AtFlags, CWD, FileType, Mode, OFlags, ResolveFlags, fchmod, mkdirat, open, openat, openat2,
    renameat, statat, unlinkat,
};
use rustix::io::Errno;

use crate::domain::{AbsDir, AbsPath, GuestFile, GuestRel};
use crate::layout::VmPaths;
use crate::ports::Cause;
use crate::ports::guest_delivery::{DeliveryError, GuestDelivery};
use crate::ports::host_fs::FsOp;

/// The real share of one VM.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ShareDelivery {
    root: AbsDir,
    trash: AbsDir,
}

impl ShareDelivery {
    /// `root` is the share (`VmPaths::guest`); `trash` a host-owned directory on the same
    /// filesystem (`VmPaths::trash`), made when first needed.
    #[must_use]
    pub const fn new(root: AbsDir, trash: AbsDir) -> Self {
        Self { root, trash }
    }

    /// The share and trash of `vm`.
    #[must_use]
    pub fn of(vm: &VmPaths) -> Self {
        Self::new(vm.guest(), vm.trash())
    }
}

impl GuestDelivery for ShareDelivery {
    fn ensure_dir(&self, dir: &GuestRel) -> impl Future<Output = Result<(), DeliveryError>> + Send {
        let (share, dir) = (self.clone(), dir.clone());
        blocking(FsOp::Create, self.root.join_rel(&dir), move || {
            share.make_dir(&dir)
        })
    }

    fn put(&self, file: &GuestFile) -> impl Future<Output = Result<(), DeliveryError>> + Send {
        let (share, file) = (self.clone(), file.clone());
        blocking(FsOp::Write, self.root.join_rel(&file.dest), move || {
            share.write(&file)
        })
    }

    fn remove(&self, rel: &GuestRel) -> impl Future<Output = Result<(), DeliveryError>> + Send {
        let (share, rel) = (self.clone(), rel.clone());
        blocking(FsOp::Remove, self.root.join_rel(&rel), move || {
            share.unlink(&rel)
        })
    }

    fn replace_tree(
        &self,
        dir: &GuestRel,
        files: &[GuestFile],
    ) -> impl Future<Output = Result<(), DeliveryError>> + Send {
        let (share, dir, files) = (self.clone(), dir.clone(), files.to_vec());
        blocking(FsOp::Remove, self.root.join_rel(&dir), move || {
            share.swap_tree(&dir, &files)
        })
    }
}

async fn blocking(
    op: FsOp,
    path: AbsPath,
    work: impl FnOnce() -> Result<(), DeliveryError> + Send + 'static,
) -> Result<(), DeliveryError> {
    tokio::task::spawn_blocking(work)
        .await
        .unwrap_or(Err(DeliveryError::Aborted { op, path }))
}

/// A failed step: what was being done and the errno.
type Step = (FsOp, Errno);

/// How a path is entered: below a directory descriptor, never through a symlink.
const ENTER: ResolveFlags = ResolveFlags::BENEATH
    .union(ResolveFlags::NO_SYMLINKS)
    .union(ResolveFlags::NO_MAGICLINKS);

const DIR_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);

/// Errors that say the share holds something the host didn't put there (or can't enter).
fn is_blocked(errno: Errno) -> bool {
    [
        Errno::LOOP,
        Errno::XDEV,
        Errno::NOTDIR,
        Errno::ISDIR,
        Errno::EXIST,
        Errno::NOTEMPTY,
    ]
    .contains(&errno)
}

/// A walk that failed: the number of components entered when it did.
struct WalkFailure {
    depth: usize,
    step: Step,
}

impl ShareDelivery {
    fn fail(&self, rel: &GuestRel, depth: usize, (op, errno): Step) -> DeliveryError {
        let path = self.root.join_rel(&rel.first(depth));
        let cause = Cause::of(&std::io::Error::from(errno));
        if is_blocked(errno) {
            DeliveryError::Blocked { op, path, cause }
        } else {
            DeliveryError::Io { op, path, cause }
        }
    }

    fn open_root(&self) -> Result<OwnedFd, Step> {
        open(self.root.get().as_str(), DIR_FLAGS, Mode::empty()).map_err(|e| (FsOp::Open, e))
    }

    /// Enters `dirs` one by one from the root, making missing directories if `create`.
    fn walk<'a>(
        &self,
        dirs: impl Iterator<Item = &'a str>,
        create: bool,
    ) -> Result<OwnedFd, WalkFailure> {
        let mut fd = self
            .open_root()
            .map_err(|step| WalkFailure { depth: 0, step })?;
        for (index, name) in dirs.enumerate() {
            let depth = index.saturating_add(1);
            let fail = |step| WalkFailure { depth, step };
            if create {
                match mkdirat(&fd, name, Mode::from_raw_mode(0o755)) {
                    Ok(()) | Err(Errno::EXIST) => {}
                    Err(errno) => return Err(fail((FsOp::Create, errno))),
                }
            }
            fd = openat2(&fd, name, DIR_FLAGS, Mode::empty(), ENTER)
                .map_err(|errno| fail((FsOp::Open, errno)))?;
        }
        Ok(fd)
    }

    fn make_dir(&self, dir: &GuestRel) -> Result<(), DeliveryError> {
        self.walk(dir.components(), true)
            .map(|_| ())
            .map_err(|f| self.fail(dir, f.depth, f.step))
    }

    fn write(&self, file: &GuestFile) -> Result<(), DeliveryError> {
        let rel = &file.dest;
        let (dirs, name) = rel.dirs_and_name();
        let depth = rel.components().count();
        let dir = self
            .walk(dirs, true)
            .map_err(|f| self.fail(rel, f.depth, f.step))?;
        let temp = temp_name(name);
        write_replacing(&dir, name, &temp, &file.bytes, file.mode)
            .map_err(|step| self.fail(rel, depth, step))
    }

    fn unlink(&self, rel: &GuestRel) -> Result<(), DeliveryError> {
        let (dirs, name) = rel.dirs_and_name();
        let depth = rel.components().count();
        let dir = match self.walk(dirs, false) {
            Ok(dir) => dir,
            Err(WalkFailure {
                step: (_, Errno::NOENT),
                ..
            }) => return Ok(()),
            Err(f) => return Err(self.fail(rel, f.depth, f.step)),
        };
        match unlinkat(&dir, name, AtFlags::empty()) {
            Ok(()) | Err(Errno::NOENT) => Ok(()),
            Err(errno) => Err(self.fail(rel, depth, (FsOp::Remove, errno))),
        }
    }

    fn swap_tree(&self, dir: &GuestRel, files: &[GuestFile]) -> Result<(), DeliveryError> {
        self.discard(dir)?;
        self.make_dir(dir)?;
        for file in files {
            self.write(&GuestFile {
                dest: dir.join(&file.dest),
                bytes: file.bytes.clone(),
                mode: file.mode,
            })?;
        }
        Ok(())
    }

    /// Gets whatever is at `rel` out of the share: a directory is renamed into the trash and
    /// deleted there, anything else unlinked; nothing there is fine.
    fn discard(&self, rel: &GuestRel) -> Result<(), DeliveryError> {
        let (dirs, name) = rel.dirs_and_name();
        let depth = rel.components().count();
        let at = |step| self.fail(rel, depth, step);
        let parent = match self.walk(dirs, true) {
            Ok(parent) => parent,
            Err(f) => return Err(self.fail(rel, f.depth, f.step)),
        };
        let is_dir = match statat(&parent, name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(stat) => FileType::from_raw_mode(stat.st_mode) == FileType::Directory,
            Err(Errno::NOENT) => return Ok(()),
            Err(errno) => return Err(at((FsOp::Stat, errno))),
        };
        if !is_dir {
            return unlinkat(&parent, name, AtFlags::empty())
                .map_err(|errno| at((FsOp::Remove, errno)));
        }
        let trash = self.open_trash().map_err(at)?;
        let entry = trash_entry();
        renameat(&parent, name, &trash, &entry).map_err(|errno| at((FsOp::Rename, errno)))?;
        let moved = PathBuf::from(self.trash.get()).join(&entry);
        fs::remove_dir_all(&moved).map_err(|error| DeliveryError::Io {
            op: FsOp::Remove,
            path: self.trash.to_path(),
            cause: Cause::of(&error),
        })
    }

    /// The trash directory, made (mode 0700) if missing and emptied of what an earlier crash
    /// left, best effort.
    fn open_trash(&self) -> Result<OwnedFd, Step> {
        match mkdirat(CWD, self.trash.get().as_str(), Mode::from_raw_mode(0o700)) {
            Ok(()) | Err(Errno::EXIST) => {}
            Err(errno) => return Err((FsOp::Create, errno)),
        }
        let trash = open(self.trash.get().as_str(), DIR_FLAGS, Mode::empty())
            .map_err(|e| (FsOp::Open, e))?;
        if let Ok(entries) = fs::read_dir(self.trash.get()) {
            for entry in entries.flatten() {
                let path = entry.path();
                let _ = if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                    fs::remove_dir_all(&path)
                } else {
                    fs::remove_file(&path)
                };
            }
        }
        Ok(trash)
    }
}

fn unique() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    format!(
        "{}.{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

fn temp_name(name: &str) -> String {
    format!(".{name}.{}.tmp", unique())
}

fn trash_entry() -> String {
    unique()
}

/// Removes the temp entry unless it was renamed away.
struct TempEntry<'a> {
    dir: &'a OwnedFd,
    name: &'a str,
    committed: bool,
}

impl Drop for TempEntry<'_> {
    fn drop(&mut self) {
        if !self.committed {
            let _ = unlinkat(self.dir, self.name, AtFlags::empty());
        }
    }
}

/// Writes `bytes` to the fresh entry `temp` in `dir` (`O_EXCL | O_NOFOLLOW`: a planted entry at
/// that name is an error, never written through) and renames it over `name`.
fn write_replacing(
    dir: &OwnedFd,
    name: &str,
    temp: &str,
    bytes: &[u8],
    mode: crate::domain::FileMode,
) -> Result<(), Step> {
    let flags = OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let fd = openat(dir, temp, flags, Mode::RUSR | Mode::WUSR).map_err(|e| (FsOp::Create, e))?;
    let mut entry = TempEntry {
        dir,
        name: temp,
        committed: false,
    };
    // Not subject to the umask.
    fchmod(&fd, Mode::from_raw_mode(u32::from(*mode.get()))).map_err(|e| (FsOp::Chmod, e))?;
    let mut file = File::from(fd);
    file.write_all(bytes).map_err(|error| {
        (
            FsOp::Write,
            error
                .raw_os_error()
                .map_or(Errno::IO, Errno::from_raw_os_error),
        )
    })?;
    renameat(dir, temp, dir, name).map_err(|e| (FsOp::Rename, e))?;
    entry.committed = true;
    Ok(())
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _, symlink};

    use super::*;
    use crate::diagnostic::Diagnostic as _;
    use crate::domain::{FileMode, FileName};
    use crate::refined;
    use crate::testing::TempDir;

    struct Fixture {
        temp: TempDir,
        share: ShareDelivery,
        root: PathBuf,
        outside: PathBuf,
    }

    /// `<tmp>/guest` (the share), `<tmp>/.trash` and `<tmp>/outside` with a file `precious`.
    fn fixture(label: &str) -> Fixture {
        let temp = TempDir::new(label).unwrap();
        let base = temp.path().to_path_buf();
        let (root, outside) = (base.join("guest"), base.join("outside"));
        fs::create_dir(&root).unwrap();
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("precious"), b"precious").unwrap();
        let abs = |path: PathBuf| AbsDir::normalising(path.to_str().unwrap()).unwrap();
        let share = ShareDelivery::new(abs(root.clone()), abs(base.join(".trash")));
        Fixture {
            temp,
            share,
            root,
            outside,
        }
    }

    impl Fixture {
        fn precious_is_intact(&self) -> bool {
            fs::read(self.outside.join("precious")).is_ok_and(|bytes| bytes == b"precious")
                && fs::read_dir(&self.outside).unwrap().count() == 1
        }

        fn trash_is_empty(&self) -> bool {
            let trash = self.temp.path().join(".trash");
            !trash.exists() || fs::read_dir(trash).unwrap().count() == 0
        }
    }

    fn rel(text: &str) -> GuestRel {
        GuestRel::parse(text.to_owned()).unwrap()
    }

    fn mode(bits: u16) -> FileMode {
        FileMode::parse(bits).unwrap()
    }

    fn file(dest: &str, bytes: &[u8], bits: u16) -> GuestFile {
        GuestFile {
            dest: rel(dest),
            bytes: bytes.to_vec(),
            mode: mode(bits),
        }
    }

    fn names(dir: &std::path::Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    }

    /// The kind and path of a `Blocked` error, `None` for anything else.
    fn blocked(error: &DeliveryError) -> Option<(FsOp, String, i32)> {
        match error {
            DeliveryError::Blocked { op, path, cause } => {
                Some((*op, path.to_string(), cause.errno.unwrap_or_default()))
            }
            DeliveryError::Io { .. } | DeliveryError::Aborted { .. } => None,
        }
    }

    #[test]
    fn a_put_creates_parents_and_sets_exactly_the_mode() {
        let f = fixture("guest-mode");
        f.share
            .write(&file("home/.config/git/config", b"x", 0o600))
            .unwrap();
        let target = f.root.join("home/.config/git/config");
        assert_eq!(fs::read(&target).unwrap(), b"x");
        assert_eq!(fs::metadata(&target).unwrap().mode() & 0o7777, 0o600);
        assert_eq!(
            fs::metadata(f.root.join("home/.config")).unwrap().mode() & 0o777,
            0o755
        );
        assert_eq!(names(&f.root.join("home/.config/git")), ["config"]);
    }

    #[test]
    fn a_hardlinked_file_is_replaced_not_written_through() {
        let f = fixture("guest-hardlink");
        let share_file = f.root.join("file");
        fs::hard_link(f.outside.join("precious"), &share_file).unwrap();
        f.share.write(&file("file", b"new", 0o644)).unwrap();
        assert_eq!(fs::read(&share_file).unwrap(), b"new");
        assert!(f.precious_is_intact());
        assert_eq!(fs::metadata(&share_file).unwrap().nlink(), 1);
    }

    #[test]
    fn a_symlink_at_the_file_is_replaced_and_its_target_untouched() {
        let f = fixture("guest-file-symlink");
        symlink(f.outside.join("precious"), f.root.join("file")).unwrap();
        f.share.write(&file("file", b"new", 0o644)).unwrap();
        assert!(f.precious_is_intact());
        let meta = fs::symlink_metadata(f.root.join("file")).unwrap();
        assert!(!meta.is_symlink());
        assert_eq!(fs::read(f.root.join("file")).unwrap(), b"new");
    }

    #[test]
    fn a_dangling_symlink_at_the_file_is_replaced_and_creates_nothing_outside() {
        let f = fixture("guest-dangling");
        symlink(f.outside.join("new-file"), f.root.join("file")).unwrap();
        f.share.write(&file("file", b"new", 0o644)).unwrap();
        assert!(!f.outside.join("new-file").exists());
        assert!(
            !fs::symlink_metadata(f.root.join("file"))
                .unwrap()
                .is_symlink()
        );
    }

    #[test]
    fn a_symlinked_directory_on_the_way_is_refused_and_nothing_is_written_outside() {
        let f = fixture("guest-dir-symlink");
        symlink(&f.outside, f.root.join("home")).unwrap();
        let error = f
            .share
            .write(&file("home/.gitconfig", b"x", 0o644))
            .unwrap_err();
        assert_eq!(
            blocked(&error),
            Some((
                FsOp::Open,
                format!("{}/home", f.root.display()),
                // O_DIRECTORY | O_NOFOLLOW on a symlink: ENOTDIR (ELOOP without O_DIRECTORY).
                Errno::NOTDIR.raw_os_error()
            ))
        );
        assert!(f.precious_is_intact());
        assert!(error.hint().is_some_and(|h| h.contains("symlink")));
    }

    #[test]
    fn a_symlinked_directory_deeper_on_the_way_is_refused_at_that_component() {
        let f = fixture("guest-deep-symlink");
        fs::create_dir_all(f.root.join("home/.config")).unwrap();
        symlink(&f.outside, f.root.join("home/.config/git")).unwrap();
        let error = f
            .share
            .write(&file("home/.config/git/config", b"x", 0o644))
            .unwrap_err();
        assert_eq!(
            blocked(&error).map(|(_, path, _)| path),
            Some(format!("{}/home/.config/git", f.root.display()))
        );
        assert!(f.precious_is_intact());
    }

    #[test]
    fn a_relative_symlink_climbing_out_of_the_share_is_refused() {
        let f = fixture("guest-climbing-symlink");
        symlink("../outside", f.root.join("home")).unwrap();
        let error = f.share.write(&file("home/x", b"x", 0o644)).unwrap_err();
        assert!(blocked(&error).is_some(), "{error:?}");
        assert!(f.precious_is_intact());
    }

    #[test]
    fn a_symlink_loop_on_the_way_is_refused() {
        let f = fixture("guest-loop");
        symlink("loop", f.root.join("loop")).unwrap();
        let error = f.share.write(&file("loop/x", b"x", 0o644)).unwrap_err();
        assert_eq!(
            blocked(&error).map(|(_, _, errno)| errno),
            Some(Errno::NOTDIR.raw_os_error())
        );
    }

    #[test]
    fn a_file_where_a_directory_is_needed_is_refused() {
        let f = fixture("guest-file-for-dir");
        fs::write(f.root.join("home"), b"file").unwrap();
        let error = f.share.write(&file("home/x", b"x", 0o644)).unwrap_err();
        assert_eq!(
            blocked(&error).map(|(op, _, errno)| (op, errno)),
            Some((FsOp::Open, Errno::NOTDIR.raw_os_error()))
        );
        assert_eq!(fs::read(f.root.join("home")).unwrap(), b"file");
    }

    #[test]
    fn a_directory_where_a_file_goes_is_refused_and_leaves_no_temp_file() {
        let f = fixture("guest-dir-for-file");
        fs::create_dir(f.root.join("mounts")).unwrap();
        fs::write(f.root.join("mounts/inner"), b"kept").unwrap();
        let error = f.share.write(&file("mounts", b"x", 0o644)).unwrap_err();
        assert_eq!(
            blocked(&error).map(|(op, _, errno)| (op, errno)),
            Some((FsOp::Rename, Errno::ISDIR.raw_os_error()))
        );
        assert_eq!(fs::read(f.root.join("mounts/inner")).unwrap(), b"kept");
        assert_eq!(names(&f.root), ["mounts"]);
    }

    #[test]
    fn a_symlink_planted_at_the_temp_name_is_not_written_through() {
        let f = fixture("guest-temp-symlink");
        symlink(f.outside.join("precious"), f.root.join(".file.tmp")).unwrap();
        let dir = f.share.walk(std::iter::empty(), true).ok().unwrap();
        let step = write_replacing(&dir, "file", ".file.tmp", b"new", mode(0o644)).unwrap_err();
        assert_eq!(step, (FsOp::Create, Errno::EXIST));
        assert!(f.precious_is_intact());
        assert!(!f.root.join("file").exists());
        // The planted entry is the guest's; it stays.
        assert!(
            fs::symlink_metadata(f.root.join(".file.tmp"))
                .unwrap()
                .is_symlink()
        );
    }

    #[test]
    fn a_failed_write_leaves_no_temp_file() {
        let f = fixture("guest-no-temp");
        let dir = f.share.walk(std::iter::empty(), true).ok().unwrap();
        fs::create_dir(f.root.join("file")).unwrap();
        let step = write_replacing(&dir, "file", ".file.tmp", b"new", mode(0o644)).unwrap_err();
        assert_eq!(step.0, FsOp::Rename);
        assert_eq!(names(&f.root), ["file"]);
    }

    #[test]
    fn removing_a_file_is_idempotent_and_a_missing_parent_is_fine() {
        let f = fixture("guest-remove");
        fs::write(f.root.join("term"), b"rows 1 cols 1").unwrap();
        f.share.unlink(&rel("term")).unwrap();
        f.share.unlink(&rel("term")).unwrap();
        f.share.unlink(&rel("missing/term")).unwrap();
        assert!(!f.root.join("term").exists());
        assert!(!f.root.join("missing").exists());
    }

    #[test]
    fn removing_through_a_symlinked_directory_is_refused_and_the_target_survives() {
        let f = fixture("guest-remove-through");
        symlink(&f.outside, f.root.join("home")).unwrap();
        let error = f.share.unlink(&rel("home/precious")).unwrap_err();
        assert!(blocked(&error).is_some(), "{error:?}");
        assert!(f.precious_is_intact());
    }

    #[test]
    fn removing_a_symlink_removes_the_link_not_its_target() {
        let f = fixture("guest-remove-link");
        symlink(f.outside.join("precious"), f.root.join("term")).unwrap();
        f.share.unlink(&rel("term")).unwrap();
        assert!(fs::symlink_metadata(f.root.join("term")).is_err());
        assert!(f.precious_is_intact());
    }

    #[test]
    fn removing_a_directory_is_refused() {
        let f = fixture("guest-remove-dir");
        fs::create_dir(f.root.join("term")).unwrap();
        let error = f.share.unlink(&rel("term")).unwrap_err();
        assert_eq!(
            blocked(&error).map(|(op, _, errno)| (op, errno)),
            Some((FsOp::Remove, Errno::ISDIR.raw_os_error()))
        );
    }

    #[test]
    fn ensure_dir_makes_real_directories_and_accepts_existing_ones() {
        let f = fixture("guest-ensure");
        f.share.make_dir(&rel("home")).unwrap();
        f.share.make_dir(&rel("home")).unwrap();
        f.share.make_dir(&rel("home/amoeboid/nix")).unwrap();
        assert!(f.root.join("home/amoeboid/nix").is_dir());
    }

    #[test]
    fn ensure_dir_refuses_a_symlink_a_file_and_a_missing_share() {
        let f = fixture("guest-ensure-refused");
        symlink(&f.outside, f.root.join("home")).unwrap();
        fs::write(f.root.join("file"), b"").unwrap();
        for text in ["home", "home/sub", "file", "file/sub"] {
            let error = f.share.make_dir(&rel(text)).unwrap_err();
            assert!(blocked(&error).is_some(), "{text}: {error:?}");
        }
        assert!(f.precious_is_intact());
        let gone = ShareDelivery::new(
            AbsDir::normalising(f.temp.path().join("absent").to_str().unwrap()).unwrap(),
            f.share.trash.clone(),
        );
        assert!(matches!(
            gone.make_dir(&rel("home")),
            Err(DeliveryError::Io { op: FsOp::Open, .. })
        ));
    }

    #[test]
    fn a_share_root_that_is_a_symlink_is_refused() {
        let f = fixture("guest-root-symlink");
        let link = f.temp.path().join("link");
        symlink(&f.root, &link).unwrap();
        let via_link = ShareDelivery::new(
            AbsDir::normalising(link.to_str().unwrap()).unwrap(),
            f.share.trash.clone(),
        );
        let error = via_link.write(&file("x", b"x", 0o644)).unwrap_err();
        assert!(blocked(&error).is_some(), "{error:?}");
        assert!(!f.root.join("x").exists());
    }

    fn plant_tree(f: &Fixture) {
        fs::create_dir_all(f.root.join("home/amoeboid/nix/deep")).unwrap();
        fs::write(f.root.join("home/amoeboid/nix/mine.nix"), b"mine").unwrap();
        fs::write(f.root.join("home/amoeboid/nix/deep/x"), b"x").unwrap();
    }

    #[test]
    fn replacing_a_tree_leaves_exactly_the_new_files_and_an_empty_trash() {
        let f = fixture("guest-tree");
        plant_tree(&f);
        let files = [
            file("flake.nix", b"{ }", 0o644),
            file("pkgs/default.nix", b"p", 0o644),
        ];
        f.share
            .swap_tree(&rel("home/amoeboid/nix"), &files)
            .unwrap();
        let tree = f.root.join("home/amoeboid/nix");
        assert_eq!(names(&tree), ["flake.nix", "pkgs"]);
        assert_eq!(fs::read(tree.join("pkgs/default.nix")).unwrap(), b"p");
        assert!(f.trash_is_empty());
        assert!(f.temp.path().join(".trash").is_dir());
        assert_eq!(
            fs::metadata(f.temp.path().join(".trash")).unwrap().mode() & 0o777,
            0o700
        );
    }

    #[test]
    fn replacing_a_missing_or_empty_tree_creates_the_directory() {
        let f = fixture("guest-tree-new");
        f.share
            .swap_tree(&rel("home/amoeboid/reference"), &[])
            .unwrap();
        assert!(f.root.join("home/amoeboid/reference").is_dir());
        assert!(!f.temp.path().join(".trash").exists());
    }

    #[test]
    fn a_tree_that_is_a_symlink_is_unlinked_and_its_target_untouched() {
        let f = fixture("guest-tree-symlink");
        fs::create_dir(f.root.join("home")).unwrap();
        symlink(&f.outside, f.root.join("home/nix")).unwrap();
        f.share
            .swap_tree(&rel("home/nix"), &[file("flake.nix", b"{ }", 0o644)])
            .unwrap();
        assert!(f.precious_is_intact());
        assert!(
            !fs::symlink_metadata(f.root.join("home/nix"))
                .unwrap()
                .is_symlink()
        );
        assert_eq!(names(&f.root.join("home/nix")), ["flake.nix"]);
    }

    #[test]
    fn symlinks_inside_the_old_tree_are_deleted_not_followed() {
        let f = fixture("guest-tree-inner-symlink");
        plant_tree(&f);
        symlink(&f.outside, f.root.join("home/amoeboid/nix/escape")).unwrap();
        symlink(
            f.outside.join("precious"),
            f.root.join("home/amoeboid/nix/deep/file"),
        )
        .unwrap();
        f.share
            .swap_tree(
                &rel("home/amoeboid/nix"),
                &[file("flake.nix", b"{ }", 0o644)],
            )
            .unwrap();
        assert!(f.precious_is_intact());
        assert_eq!(names(&f.root.join("home/amoeboid/nix")), ["flake.nix"]);
        assert!(f.trash_is_empty());
    }

    #[test]
    fn a_tree_below_a_symlinked_directory_is_refused() {
        let f = fixture("guest-tree-parent-symlink");
        symlink(&f.outside, f.root.join("home")).unwrap();
        let error = f
            .share
            .swap_tree(&rel("home/amoeboid/nix"), &[file("x", b"x", 0o644)])
            .unwrap_err();
        assert!(blocked(&error).is_some(), "{error:?}");
        assert!(f.precious_is_intact());
    }

    #[test]
    fn a_tree_that_is_a_file_is_replaced_by_a_directory() {
        let f = fixture("guest-tree-file");
        fs::write(f.root.join("nix"), b"file").unwrap();
        f.share
            .swap_tree(&rel("nix"), &[file("flake.nix", b"{ }", 0o644)])
            .unwrap();
        assert_eq!(names(&f.root.join("nix")), ["flake.nix"]);
    }

    #[test]
    fn what_an_earlier_crash_left_in_the_trash_is_swept_with_the_next_replacement() {
        let f = fixture("guest-trash-leftover");
        plant_tree(&f);
        let leftover = f.temp.path().join(".trash/1.1/inner");
        fs::create_dir_all(&leftover).unwrap();
        fs::write(leftover.join("old"), b"old").unwrap();
        symlink(&f.outside, f.temp.path().join(".trash/link")).unwrap();
        f.share
            .swap_tree(&rel("home/amoeboid/nix"), &[file("a", b"a", 0o644)])
            .unwrap();
        assert!(f.trash_is_empty());
        assert!(f.precious_is_intact());
    }

    #[test]
    fn the_trash_is_never_entered_through_a_symlink() {
        let f = fixture("guest-trash-symlink");
        plant_tree(&f);
        symlink(&f.outside, f.temp.path().join(".trash")).unwrap();
        let error = f
            .share
            .swap_tree(&rel("home/amoeboid/nix"), &[file("a", b"a", 0o644)])
            .unwrap_err();
        assert!(blocked(&error).is_some(), "{error:?}");
        assert!(f.precious_is_intact());
        // The old tree is still where it was: nothing was moved.
        assert_eq!(
            fs::read(f.root.join("home/amoeboid/nix/mine.nix")).unwrap(),
            b"mine"
        );
    }

    #[test]
    fn a_read_only_share_directory_is_an_io_error_with_the_path() {
        if rustix::process::geteuid().is_root() {
            return; // root ignores permission bits
        }
        let f = fixture("guest-read-only");
        fs::set_permissions(&f.root, fs::Permissions::from_mode(0o500)).unwrap();
        let error = f.share.write(&file("mounts", b"x", 0o644)).unwrap_err();
        fs::set_permissions(&f.root, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(matches!(
            error,
            DeliveryError::Io { op: FsOp::Create, ref path, .. } if path.get().ends_with("/guest/mounts")
        ));
    }

    #[test]
    fn the_port_runs_the_same_operations_on_the_blocking_pool() {
        let f = fixture("guest-port");
        let name: FileName = refined!(FileName, "hostname");
        let hostname = GuestFile {
            dest: GuestRel::of(&name),
            bytes: b"vm\n".to_vec(),
            mode: mode(0o644),
        };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        runtime.block_on(async {
            let share = &f.share;
            share.ensure_dir(&rel("home")).await.unwrap();
            share.put(&hostname).await.unwrap();
            share
                .replace_tree(&rel("home/nix"), &[file("flake.nix", b"{ }", 0o644)])
                .await
                .unwrap();
            share.remove(&rel("hostname")).await.unwrap();
            let error = share.put(&file("home/nix/flake.nix/x", b"", 0o644)).await;
            assert!(error.is_err());
        });
        assert_eq!(names(&f.root), ["home"]);
        assert_eq!(names(&f.root.join("home/nix")), ["flake.nix"]);
    }
}
