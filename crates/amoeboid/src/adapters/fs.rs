//! `StdFs`: [`HostFs`] on `std::fs` and `rustix`. The synchronous functions below do the work;
//! the trait impl runs them with `spawn_blocking`. Every write is atomic, so a cancelled future
//! (the blocking task finishes anyway) never leaves a half-written file.

use std::fs::{self, DirBuilder, File};
use std::future::Future;
use std::io::{self, Write as _};
use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use rustix::fs::{
    Access, AtFlags, CWD, Mode, OFlags, RenameFlags, accessat, fchmod, openat, renameat_with,
};
use rustix::io::Errno;

use crate::domain::file_mode::ModeBits;
use crate::domain::{AbsDir, AbsPath, FileMode, GuestRel, Observation, TreeFile};
use crate::ports::Cause;
use crate::ports::host_fs::{FsError, FsOp, HostFs, Replacement};

/// Names never copied from a configuration directory, at any depth.
const IGNORED: [&str; 3] = ["__pycache__", "result", ".git"];

/// Deepest directory nesting `copy_dir` follows; symlink loops hit it.
pub const MAX_COPY_DEPTH: usize = 64;

/// The real host filesystem.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct StdFs;

impl HostFs for StdFs {
    fn write_file(
        &self,
        path: &AbsPath,
        bytes: Vec<u8>,
        mode: FileMode,
    ) -> impl Future<Output = Result<(), FsError>> + Send {
        let path = path.clone();
        blocking(FsOp::Write, path.clone(), move || {
            write_atomic(&path, &bytes, mode)
        })
    }

    fn observe(&self, path: &AbsPath) -> impl Future<Output = Result<Observation, FsError>> + Send {
        let path = path.clone();
        blocking(FsOp::Stat, path.clone(), move || Ok(observe(&path)))
    }

    fn read_file(&self, path: &AbsPath) -> impl Future<Output = Result<Vec<u8>, FsError>> + Send {
        let path = path.clone();
        blocking(FsOp::Read, path.clone(), move || read(&path))
    }

    fn file_mode(&self, path: &AbsPath) -> impl Future<Output = Result<FileMode, FsError>> + Send {
        let path = path.clone();
        blocking(FsOp::Stat, path.clone(), move || file_mode(&path))
    }

    fn read_tree(
        &self,
        root: &AbsDir,
    ) -> impl Future<Output = Result<Vec<TreeFile>, FsError>> + Send {
        let root = root.clone();
        blocking(FsOp::Read, root.to_path(), move || read_tree(&root))
    }

    fn remove_file(&self, path: &AbsPath) -> impl Future<Output = Result<(), FsError>> + Send {
        let path = path.clone();
        blocking(FsOp::Remove, path.clone(), move || remove_file(&path))
    }

    fn clear_dir(&self, dir: &AbsDir) -> impl Future<Output = Result<(), FsError>> + Send {
        let dir = dir.clone();
        blocking(FsOp::Remove, dir.to_path(), move || clear_dir(&dir))
    }

    fn create_dir_all(
        &self,
        dir: &AbsDir,
        mode: FileMode,
    ) -> impl Future<Output = Result<(), FsError>> + Send {
        let dir = dir.clone();
        blocking(FsOp::Create, dir.to_path(), move || {
            create_dir_all(&dir, mode)
        })
    }

    fn replace_dir(
        &self,
        target: &AbsDir,
        source: &AbsDir,
    ) -> impl Future<Output = Result<Replacement, FsError>> + Send {
        let (target, source) = (target.clone(), source.clone());
        blocking(FsOp::Exchange, target.to_path(), move || {
            replace_dir(&target, &source)
        })
    }
}

async fn blocking<T: Send + 'static>(
    op: FsOp,
    path: AbsPath,
    work: impl FnOnce() -> Result<T, FsError> + Send + 'static,
) -> Result<T, FsError> {
    tokio::task::spawn_blocking(work)
        .await
        .unwrap_or_else(|_| Err(FsError::Aborted { op, path }))
}

fn remove_file(path: &AbsPath) -> Result<(), FsError> {
    match fs::remove_file(raw(path)) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(fail(FsOp::Remove, path, &error)),
    }
}

fn clear_dir(dir: &AbsDir) -> Result<(), FsError> {
    match fs::remove_dir_all(raw_dir(dir)) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(fail(FsOp::Remove, &dir.to_path(), &error)),
    }
}

fn fail(op: FsOp, path: &AbsPath, error: &io::Error) -> FsError {
    FsError::Io {
        op,
        path: path.clone(),
        entry: None,
        cause: Cause::of(error),
    }
}

fn fail_at(op: FsOp, root: &AbsDir, rel: &Path, error: &io::Error) -> FsError {
    let entry = rel.to_string_lossy();
    FsError::Io {
        op,
        path: root.to_path(),
        entry: (!entry.is_empty()).then(|| entry.into_owned()),
        cause: Cause::of(error),
    }
}

fn raw(path: &AbsPath) -> &Path {
    Path::new(path.get())
}

fn raw_dir(dir: &AbsDir) -> &Path {
    Path::new(dir.get())
}

/// A temp file next to its target, removed unless [`Staged::commit`] renames it into place.
struct Staged<'a> {
    target: &'a AbsPath,
    temp: PathBuf,
    parent: PathBuf,
    committed: bool,
}

impl<'a> Staged<'a> {
    /// Creates the temp file (`O_EXCL`, never through a symlink), fills it and syncs it.
    fn stage(target: &'a AbsPath, bytes: &[u8], mode: FileMode) -> Result<Self, FsError> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let parent = target.parent();
        let name = target.get().rsplit('/').next().unwrap_or_default();
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let temp = raw_dir(&parent).join(format!(".{name}.{}.{n}.tmp", std::process::id()));
        let flags =
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        let fd = openat(CWD, &temp, flags, Mode::RUSR | Mode::WUSR)
            .map_err(|errno| fail(FsOp::Create, target, &io::Error::from(errno)))?;
        let staged = Self {
            target,
            temp,
            parent: raw_dir(&parent).to_path_buf(),
            committed: false,
        };
        // Not subject to the umask.
        fchmod(&fd, Mode::from_raw_mode(u32::from(*mode.get())))
            .map_err(|errno| fail(FsOp::Chmod, target, &io::Error::from(errno)))?;
        let mut file = File::from(fd);
        file.write_all(bytes)
            .map_err(|error| fail(FsOp::Write, target, &error))?;
        file.sync_all()
            .map_err(|error| fail(FsOp::Sync, target, &error))?;
        Ok(staged)
    }

    /// Renames the temp file over the target, then syncs the directory.
    fn commit(mut self) -> Result<(), FsError> {
        fs::rename(&self.temp, raw(self.target))
            .map_err(|error| fail(FsOp::Rename, self.target, &error))?;
        self.committed = true;
        sync_dir(&self.parent).map_err(|error| fail(FsOp::Sync, self.target, &error))
    }
}

impl Drop for Staged<'_> {
    fn drop(&mut self) {
        if !self.committed {
            // Best effort: the old content is intact either way.
            let _ = fs::remove_file(&self.temp);
        }
    }
}

fn sync_dir(dir: &Path) -> io::Result<()> {
    File::open(dir)?.sync_all()
}

fn write_atomic(path: &AbsPath, bytes: &[u8], mode: FileMode) -> Result<(), FsError> {
    Staged::stage(path, bytes, mode)?.commit()
}

/// Canonicalises `path` and says what it is. Every failure to look is `Missing`; a kind that
/// can't be used, or a resolved name that is not UTF-8, is `Other`. Executability is the invoking
/// user's (`faccessat` with `AT_EACCESS`), so a file the user can't run is not `executable`.
pub(super) fn observe(path: &AbsPath) -> Observation {
    let Ok(resolved) = fs::canonicalize(raw(path)) else {
        return Observation::Missing;
    };
    let Ok(meta) = fs::metadata(&resolved) else {
        return Observation::Missing;
    };
    let Some(text) = resolved.to_str() else {
        return Observation::Other;
    };
    if meta.is_dir() {
        AbsDir::parse(text.to_owned()).map_or(Observation::Other, |resolved| {
            Observation::Directory { resolved }
        })
    } else if meta.is_file() {
        let executable = accessat(CWD, &resolved, Access::EXEC_OK, AtFlags::EACCESS).is_ok();
        AbsPath::parse(text.to_owned()).map_or(Observation::Other, |resolved| Observation::File {
            resolved,
            executable,
        })
    } else {
        Observation::Other
    }
}

fn read(path: &AbsPath) -> Result<Vec<u8>, FsError> {
    fs::read(raw(path)).map_err(|error| fail(FsOp::Read, path, &error))
}

/// The permission bits (not setuid, setgid or sticky) of what `path` resolves to.
fn file_mode(path: &AbsPath) -> Result<FileMode, FsError> {
    let meta = fs::metadata(raw(path)).map_err(|error| fail(FsOp::Stat, path, &error))?;
    Ok(ModeBits::of_st_mode(meta.permissions().mode()))
}

/// The regular files below `root` (not through symlinks), directories walked in name order.
fn read_tree(root: &AbsDir) -> Result<Vec<TreeFile>, FsError> {
    let mut files = Vec::new();
    walk(root, raw_dir(root), "", 0, &mut files)?;
    Ok(files)
}

/// `prefix` is the path of `dir` below the root, with a trailing `/` (empty for the root).
fn walk(
    root: &AbsDir,
    dir: &Path,
    prefix: &str,
    depth: usize,
    files: &mut Vec<TreeFile>,
) -> Result<(), FsError> {
    let below = Path::new(prefix);
    if depth > MAX_COPY_DEPTH {
        return Err(FsError::TooDeep {
            path: root.clone(),
            max: MAX_COPY_DEPTH,
        });
    }
    let mut names: Vec<_> = fs::read_dir(dir)
        .and_then(|entries| entries.map(|entry| entry.map(|e| e.file_name())).collect())
        .map_err(|e| fail_at(FsOp::List, root, below, &e))?;
    names.sort();
    for name in names {
        let rel = format!("{prefix}{}", name.to_string_lossy());
        let Some(text) = name.to_str() else {
            return Err(FsError::BadName {
                path: root.clone(),
                entry: rel,
            });
        };
        let (rel, path) = (format!("{prefix}{text}"), dir.join(&name));
        let meta = fs::symlink_metadata(&path)
            .map_err(|e| fail_at(FsOp::Stat, root, Path::new(&rel), &e))?;
        if meta.is_dir() {
            walk(
                root,
                &path,
                &format!("{rel}/"),
                depth.saturating_add(1),
                files,
            )?;
        } else if meta.is_file() {
            let bytes =
                fs::read(&path).map_err(|e| fail_at(FsOp::Read, root, Path::new(&rel), &e))?;
            let rel = GuestRel::parse(rel.clone()).map_err(|_| FsError::BadName {
                path: root.clone(),
                entry: rel,
            })?;
            files.push(TreeFile { rel, bytes });
        }
    }
    Ok(())
}

fn create_dir_all(dir: &AbsDir, mode: FileMode) -> Result<(), FsError> {
    DirBuilder::new()
        .recursive(true)
        .mode(u32::from(*mode.get()))
        .create(raw_dir(dir))
        .map_err(|error| fail(FsOp::Create, &dir.to_path(), &error))
}

/// `source` copied to `dest` (which must not exist), skipping [`IGNORED`] names. Symlinks are
/// followed (their targets copied, as Python's `copytree` did); sockets, pipes and devices are
/// left out. File and directory modes keep their permission bits (no setuid or sticky).
fn copy_dir(source: &AbsDir, dest: &AbsDir, dest_raw: &Path) -> Result<(), FsError> {
    copy_tree(source, dest, raw_dir(source), dest_raw, Path::new(""), 0)
}

fn copy_tree(
    src_root: &AbsDir,
    dst_root: &AbsDir,
    src: &Path,
    dst: &Path,
    rel: &Path,
    depth: usize,
) -> Result<(), FsError> {
    if depth > MAX_COPY_DEPTH {
        return Err(FsError::TooDeep {
            path: src_root.clone(),
            max: MAX_COPY_DEPTH,
        });
    }
    let meta = fs::metadata(src).map_err(|e| fail_at(FsOp::Stat, src_root, rel, &e))?;
    let bits = meta.permissions().mode() & 0o777;
    if meta.is_dir() {
        DirBuilder::new()
            .mode(0o700)
            .create(dst)
            .map_err(|e| fail_at(FsOp::Create, dst_root, rel, &e))?;
        let entries = fs::read_dir(src).map_err(|e| fail_at(FsOp::List, src_root, rel, &e))?;
        for entry in entries {
            let entry = entry.map_err(|e| fail_at(FsOp::List, src_root, rel, &e))?;
            let name = entry.file_name();
            if IGNORED.iter().any(|ignored| name == *ignored) {
                continue;
            }
            copy_tree(
                src_root,
                dst_root,
                &src.join(&name),
                &dst.join(&name),
                &rel.join(&name),
                depth.saturating_add(1),
            )?;
        }
        fs::set_permissions(dst, fs::Permissions::from_mode(bits))
            .map_err(|e| fail_at(FsOp::Chmod, dst_root, rel, &e))
    } else if meta.is_file() {
        fs::copy(src, dst).map_err(|e| fail_at(FsOp::Write, dst_root, rel, &e))?;
        fs::set_permissions(dst, fs::Permissions::from_mode(bits))
            .map_err(|e| fail_at(FsOp::Chmod, dst_root, rel, &e))
    } else {
        Ok(())
    }
}

fn remove_any(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => fs::remove_dir_all(path),
        Ok(_) => fs::remove_file(path),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// How `renameat2(RENAME_EXCHANGE)` failed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ExchangeFailure {
    /// The kernel or filesystem doesn't implement it.
    Unsupported,
    Other(Errno),
}

fn classify_exchange(errno: Errno) -> ExchangeFailure {
    if errno == Errno::INVAL || errno == Errno::NOSYS || errno == Errno::OPNOTSUPP {
        ExchangeFailure::Unsupported
    } else {
        ExchangeFailure::Other(errno)
    }
}

/// Makes `target` a copy of `source`: copy aside to `.NAME.new`, swap with `target` in one step
/// (or rename into place if `target` is missing), then remove the old tree. A crash leaves the old
/// tree or the new one; a leftover `.NAME.new` or `.NAME.old` is removed by the next call. Where
/// the filesystem can't exchange, [`sequential_swap`] runs instead and the result says so.
fn replace_dir(target: &AbsDir, source: &AbsDir) -> Result<Replacement, FsError> {
    replace_dir_with(target, source, exchange)
}

/// [`replace_dir`] with the exchange step injected, so tests can make it unsupported.
fn replace_dir_with(
    target: &AbsDir,
    source: &AbsDir,
    exchange: impl FnOnce(&Path, &AbsDir) -> Result<(), ExchangeFailure>,
) -> Result<Replacement, FsError> {
    let parent = target.parent();
    let name = target.get().rsplit('/').next().unwrap_or_default();
    let fresh = raw_dir(&parent).join(format!(".{name}.new"));
    let stale = raw_dir(&parent).join(format!(".{name}.old"));
    let target_raw = raw_dir(target);
    let in_target = |op, error: &io::Error| fail(op, &target.to_path(), error);
    for leftover in [&fresh, &stale] {
        remove_any(leftover).map_err(|e| in_target(FsOp::Remove, &e))?;
    }
    if let Err(error) = copy_dir(source, target, &fresh) {
        let _ = remove_any(&fresh);
        return Err(error);
    }
    let swapped = match fs::symlink_metadata(target_raw) {
        Ok(meta) if meta.is_dir() => match exchange(&fresh, target) {
            Ok(()) => Ok(Replacement::Atomic),
            Err(ExchangeFailure::Unsupported) => {
                sequential_swap(&fresh, &stale, target).map(|()| Replacement::NonAtomic)
            }
            Err(ExchangeFailure::Other(errno)) => {
                Err(in_target(FsOp::Exchange, &io::Error::from(errno)))
            }
        },
        Ok(_) => Err(in_target(
            FsOp::Stat,
            &io::Error::from(io::ErrorKind::NotADirectory),
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => fs::rename(&fresh, target_raw)
            .map(|()| Replacement::Atomic)
            .map_err(|e| in_target(FsOp::Rename, &e)),
        Err(error) => Err(in_target(FsOp::Stat, &error)),
    };
    let how = match swapped {
        Ok(how) => how,
        Err(error) => {
            let _ = remove_any(&fresh);
            return Err(error);
        }
    };
    // After an exchange `fresh` holds the old tree, after the sequence `stale` does; best effort,
    // the next call removes what is left.
    let _ = remove_any(&fresh);
    let _ = remove_any(&stale);
    sync_dir(raw_dir(&parent))
        .map(|()| how)
        .map_err(|e| in_target(FsOp::Sync, &e))
}

/// The two-rename replacement for filesystems without `RENAME_EXCHANGE`: `target` aside to
/// `stale`, `fresh` in. Between the renames there is no `target`; if the second rename fails the
/// first is undone.
fn sequential_swap(fresh: &Path, stale: &Path, target: &AbsDir) -> Result<(), FsError> {
    let in_target = |error: &io::Error| fail(FsOp::Rename, &target.to_path(), error);
    fs::rename(raw_dir(target), stale).map_err(|e| in_target(&e))?;
    if let Err(error) = fs::rename(fresh, raw_dir(target)) {
        let _ = fs::rename(stale, raw_dir(target));
        return Err(in_target(&error));
    }
    Ok(())
}

fn exchange(fresh: &Path, target: &AbsDir) -> Result<(), ExchangeFailure> {
    renameat_with(CWD, fresh, CWD, raw_dir(target), RenameFlags::EXCHANGE)
        .map_err(classify_exchange)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use std::os::unix::fs::{MetadataExt as _, symlink};

    use rustix::fs::{FileType, mknodat};

    use super::*;
    use crate::diagnostic::Diagnostic as _;
    use crate::domain::{FileName, Segment as _};
    use crate::refined;
    use crate::testing::{self, TempDir};

    fn dir_of(temp: &TempDir) -> AbsDir {
        AbsDir::normalising(temp.path().to_str().unwrap()).unwrap()
    }

    fn at(dir: &AbsDir, rel: &str) -> AbsPath {
        AbsPath::parse(format!("{}/{rel}", dir.get())).unwrap()
    }

    fn dir_at(dir: &AbsDir, rel: &str) -> AbsDir {
        AbsDir::parse(format!("{}/{rel}", dir.get())).unwrap()
    }

    fn mode(bits: u16) -> FileMode {
        FileMode::parse(bits).unwrap()
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    }

    /// The operation and kind of an I/O error, `None` for the other variants.
    fn io_error(error: &FsError) -> Option<(FsOp, io::ErrorKind)> {
        match error {
            FsError::Io { op, cause, .. } => Some((*op, cause.kind)),
            FsError::TooDeep { .. } | FsError::BadName { .. } | FsError::Aborted { .. } => None,
        }
    }

    fn errno_of(error: &FsError) -> Option<i32> {
        match error {
            FsError::Io { cause, .. } => cause.errno,
            FsError::TooDeep { .. } | FsError::BadName { .. } | FsError::Aborted { .. } => None,
        }
    }

    #[test]
    fn removing_a_file_twice_succeeds_and_a_symlink_goes_without_its_target() {
        let temp = TempDir::new("fs-remove-file").unwrap();
        let root = dir_of(&temp);
        fs::write(temp.path().join("a"), b"x").unwrap();
        symlink(temp.path().join("a"), temp.path().join("link")).unwrap();
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        rt.block_on(async {
            assert_eq!(StdFs.remove_file(&at(&root, "link")).await, Ok(()));
            assert!(temp.path().join("a").exists());
            assert_eq!(StdFs.remove_file(&at(&root, "a")).await, Ok(()));
            assert_eq!(StdFs.remove_file(&at(&root, "a")).await, Ok(()));
        });
        assert_eq!(names(temp.path()), Vec::<String>::new());
    }

    #[test]
    fn removing_a_directory_as_a_file_is_refused_and_leaves_it() {
        let temp = TempDir::new("fs-remove-dir-as-file").unwrap();
        let root = dir_of(&temp);
        fs::create_dir(temp.path().join("d")).unwrap();
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let error = rt.block_on(StdFs.remove_file(&at(&root, "d"))).unwrap_err();
        assert_eq!(io_error(&error).map(|(op, _)| op), Some(FsOp::Remove));
        assert!(temp.path().join("d").is_dir());
    }

    #[test]
    fn clearing_a_directory_removes_the_tree_not_what_symlinks_point_at_and_is_idempotent() {
        let temp = TempDir::new("fs-clear-dir").unwrap();
        let root = dir_of(&temp);
        fs::create_dir_all(temp.path().join("run/sub")).unwrap();
        fs::write(temp.path().join("run/sub/f"), b"x").unwrap();
        fs::write(temp.path().join("keep"), b"x").unwrap();
        symlink(temp.path().join("keep"), temp.path().join("run/link")).unwrap();
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        rt.block_on(async {
            assert_eq!(StdFs.clear_dir(&dir_at(&root, "run")).await, Ok(()));
            assert_eq!(StdFs.clear_dir(&dir_at(&root, "run")).await, Ok(()));
        });
        assert_eq!(names(temp.path()), ["keep"]);
    }

    #[test]
    fn clearing_a_path_that_is_a_file_is_refused() {
        let temp = TempDir::new("fs-clear-file").unwrap();
        let root = dir_of(&temp);
        fs::write(temp.path().join("f"), b"x").unwrap();
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let error = rt
            .block_on(StdFs.clear_dir(&dir_at(&root, "f")))
            .unwrap_err();
        assert_eq!(io_error(&error).map(|(op, _)| op), Some(FsOp::Remove));
        assert!(temp.path().join("f").is_file());
    }

    #[test]
    fn a_written_file_has_exactly_the_requested_mode_whatever_the_umask() {
        let temp = TempDir::new("fs-mode").unwrap();
        let root = dir_of(&temp);
        for bits in [0o600, 0o644, 0o666, 0o755] {
            let path = at(&root, &format!("f{bits:o}"));
            write_atomic(&path, b"x", mode(bits)).unwrap();
            let meta = fs::metadata(raw(&path)).unwrap();
            assert_eq!(meta.mode() & 0o7777, u32::from(bits));
        }
    }

    #[test]
    fn a_rewrite_replaces_the_content_and_leaves_no_temp_file() {
        let temp = TempDir::new("fs-rewrite").unwrap();
        let root = dir_of(&temp);
        let path = at(&root, "run_config.kdl");
        write_atomic(&path, b"old", mode(0o644)).unwrap();
        write_atomic(&path, b"new content", mode(0o644)).unwrap();
        assert_eq!(fs::read(raw(&path)).unwrap(), b"new content");
        assert_eq!(names(temp.path()), ["run_config.kdl"]);
    }

    #[test]
    fn a_planted_symlink_at_the_target_is_replaced_not_written_through() {
        let temp = TempDir::new("fs-symlink").unwrap();
        let root = dir_of(&temp);
        let victim = at(&root, "victim");
        fs::write(raw(&victim), b"precious").unwrap();
        let path = at(&root, "config");
        symlink(raw(&victim), raw(&path)).unwrap();
        write_atomic(&path, b"mine", mode(0o644)).unwrap();
        assert_eq!(fs::read(raw(&victim)).unwrap(), b"precious");
        assert!(!fs::symlink_metadata(raw(&path)).unwrap().is_symlink());
        assert_eq!(fs::read(raw(&path)).unwrap(), b"mine");
    }

    #[test]
    fn a_staged_write_that_is_never_committed_leaves_the_old_content_and_no_temp_file() {
        let temp = TempDir::new("fs-interrupted").unwrap();
        let root = dir_of(&temp);
        let path = at(&root, "mounts");
        fs::write(raw(&path), b"old").unwrap();
        let staged = Staged::stage(&path, b"half of the new content", mode(0o644)).unwrap();
        assert_eq!(fs::read(raw(&path)).unwrap(), b"old");
        assert_eq!(names(temp.path()).len(), 2, "target and temp file");
        drop(staged);
        assert_eq!(fs::read(raw(&path)).unwrap(), b"old");
        assert_eq!(names(temp.path()), ["mounts"]);
    }

    #[test]
    fn two_staged_writes_of_one_target_use_different_temp_files() {
        let temp = TempDir::new("fs-two-stages").unwrap();
        let path = at(&dir_of(&temp), "f");
        let first = Staged::stage(&path, b"1", mode(0o600)).unwrap();
        let second = Staged::stage(&path, b"2", mode(0o600)).unwrap();
        second.commit().unwrap();
        first.commit().unwrap();
        assert_eq!(fs::read(raw(&path)).unwrap(), b"1");
        assert_eq!(names(temp.path()), ["f"]);
    }

    #[test]
    fn a_failed_rename_keeps_the_old_target_and_removes_the_temp_file() {
        let temp = TempDir::new("fs-rename-fails").unwrap();
        let root = dir_of(&temp);
        let path = at(&root, "config");
        fs::create_dir(raw(&path)).unwrap();
        fs::write(at(&root, "config/inner").get(), b"kept").unwrap();
        let error = write_atomic(&path, b"new", mode(0o644)).unwrap_err();
        assert_eq!(
            io_error(&error),
            Some((FsOp::Rename, io::ErrorKind::IsADirectory))
        );
        assert_eq!(fs::read(at(&root, "config/inner").get()).unwrap(), b"kept");
        assert_eq!(names(temp.path()), ["config"]);
    }

    #[test]
    fn writing_below_a_missing_directory_is_a_create_error() {
        let temp = TempDir::new("fs-missing-dir").unwrap();
        let path = at(&dir_of(&temp), "absent/file");
        let error = write_atomic(&path, b"x", mode(0o644)).unwrap_err();
        assert_eq!(
            io_error(&error),
            Some((FsOp::Create, io::ErrorKind::NotFound))
        );
        assert_eq!(
            error.message(),
            format!("can't create {path}: entity not found")
        );
    }

    #[test]
    fn a_read_only_directory_refuses_the_write_and_keeps_the_old_content() {
        if rustix::process::geteuid().is_root() {
            return; // root ignores permission bits
        }
        let temp = TempDir::new("fs-read-only").unwrap();
        let root = dir_of(&temp);
        let path = at(&root, "f");
        fs::write(raw(&path), b"old").unwrap();
        fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o500)).unwrap();
        let error = write_atomic(&path, b"new", mode(0o644)).unwrap_err();
        fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(
            io_error(&error),
            Some((FsOp::Create, io::ErrorKind::PermissionDenied))
        );
        assert_eq!(fs::read(raw(&path)).unwrap(), b"old");
        assert_eq!(names(temp.path()), ["f"]);
    }

    #[test]
    fn reading_returns_the_bytes_and_a_missing_file_is_a_typed_error() {
        let temp = TempDir::new("fs-read").unwrap();
        let root = dir_of(&temp);
        let path = at(&root, "f");
        fs::write(raw(&path), b"\x00bytes\xff").unwrap();
        assert_eq!(read(&path).unwrap(), b"\x00bytes\xff");
        let error = read(&at(&root, "absent")).unwrap_err();
        assert_eq!(
            io_error(&error),
            Some((FsOp::Read, io::ErrorKind::NotFound))
        );
    }

    #[test]
    fn nested_directories_are_created_with_the_mode_and_existing_ones_are_fine() {
        let temp = TempDir::new("fs-mkdir").unwrap();
        let root = dir_of(&temp);
        let nested = dir_at(&root, "a/b/c");
        create_dir_all(&nested, mode(0o700)).unwrap();
        create_dir_all(&nested, mode(0o700)).unwrap();
        assert_eq!(
            fs::metadata(raw_dir(&nested)).unwrap().mode() & 0o777,
            0o700
        );
        fs::write(at(&root, "file").get(), b"").unwrap();
        let error = create_dir_all(&dir_at(&root, "file"), mode(0o700)).unwrap_err();
        assert_eq!(io_error(&error).map(|(op, _)| op), Some(FsOp::Create));
    }

    /// A configuration-like tree with files, executables, ignored names and nesting.
    fn plant_tree(root: &AbsDir) {
        let p = |rel: &str| PathBuf::from(format!("{}/{rel}", root.get()));
        for dir in [
            "pkgs/deep",
            "__pycache__",
            ".git",
            "result",
            "pkgs/__pycache__",
        ] {
            fs::create_dir_all(p(dir)).unwrap();
        }
        fs::write(p("flake.nix"), b"{ }").unwrap();
        fs::write(p("pkgs/deep/x.nix"), b"x").unwrap();
        fs::write(p("run.sh"), b"#!/bin/sh").unwrap();
        fs::set_permissions(p("run.sh"), fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(p("__pycache__/m.pyc"), b"junk").unwrap();
        fs::write(p(".git/HEAD"), b"ref").unwrap();
        fs::write(p("pkgs/__pycache__/n.pyc"), b"junk").unwrap();
        fs::write(p(".gitignore"), b"result").unwrap();
    }

    fn listing(root: &Path) -> Vec<String> {
        fn walk(dir: &Path, rel: &str, out: &mut Vec<String>) {
            for entry in fs::read_dir(dir).unwrap() {
                let entry = entry.unwrap();
                let name = format!("{rel}{}", entry.file_name().to_str().unwrap());
                if entry.file_type().unwrap().is_dir() {
                    out.push(format!("{name}/"));
                    walk(&entry.path(), &format!("{name}/"), out);
                } else {
                    out.push(name);
                }
            }
        }
        let mut out = Vec::new();
        walk(root, "", &mut out);
        out.sort();
        out
    }

    #[test]
    fn a_copy_skips_pycache_result_and_git_at_every_depth_and_keeps_modes() {
        let temp = TempDir::new("fs-copy").unwrap();
        let root = dir_of(&temp);
        let src = dir_at(&root, "src");
        fs::create_dir(raw_dir(&src)).unwrap();
        plant_tree(&src);
        let dst = dir_at(&root, "dst");
        copy_dir(&src, &dst, raw_dir(&dst)).unwrap();
        assert_eq!(
            listing(raw_dir(&dst)),
            [
                ".gitignore",
                "flake.nix",
                "pkgs/",
                "pkgs/deep/",
                "pkgs/deep/x.nix",
                "run.sh"
            ]
        );
        assert_eq!(fs::read(at(&dst, "pkgs/deep/x.nix").get()).unwrap(), b"x");
        assert_eq!(
            fs::metadata(at(&dst, "run.sh").get()).unwrap().mode() & 0o777,
            0o755
        );
        assert_eq!(
            fs::metadata(at(&dst, "flake.nix").get()).unwrap().mode() & 0o777,
            0o644
        );
    }

    #[test]
    fn a_copy_follows_symlinks_to_files_and_directories_and_skips_special_files() {
        let temp = TempDir::new("fs-copy-links").unwrap();
        let root = dir_of(&temp);
        let outside = dir_at(&root, "outside");
        fs::create_dir(raw_dir(&outside)).unwrap();
        fs::write(at(&outside, "real.nix").get(), b"real").unwrap();
        let src = dir_at(&root, "src");
        fs::create_dir(raw_dir(&src)).unwrap();
        symlink(at(&outside, "real.nix").get(), at(&src, "file-link").get()).unwrap();
        symlink(raw_dir(&outside), at(&src, "dir-link").get()).unwrap();
        mknodat(CWD, at(&src, "pipe").get(), FileType::Fifo, Mode::RUSR, 0).unwrap();
        let dst = dir_at(&root, "dst");
        copy_dir(&src, &dst, raw_dir(&dst)).unwrap();
        assert_eq!(
            listing(raw_dir(&dst)),
            ["dir-link/", "dir-link/real.nix", "file-link"]
        );
        assert!(
            !fs::symlink_metadata(at(&dst, "file-link").get())
                .unwrap()
                .is_symlink()
        );
        assert_eq!(fs::read(at(&dst, "file-link").get()).unwrap(), b"real");
    }

    #[test]
    fn a_dangling_symlink_fails_the_copy_naming_the_entry() {
        let temp = TempDir::new("fs-copy-dangling").unwrap();
        let root = dir_of(&temp);
        let src = dir_at(&root, "src");
        fs::create_dir(raw_dir(&src)).unwrap();
        symlink("nowhere", at(&src, "broken").get()).unwrap();
        let dst = dir_at(&root, "dst");
        let error = copy_dir(&src, &dst, raw_dir(&dst)).unwrap_err();
        assert_eq!(
            error,
            FsError::Io {
                op: FsOp::Stat,
                path: src.to_path(),
                entry: Some("broken".to_owned()),
                cause: Cause {
                    kind: io::ErrorKind::NotFound,
                    errno: Some(2)
                },
            }
        );
        assert_eq!(
            error.message(),
            format!("can't inspect {src}/broken: entity not found")
        );
    }

    #[test]
    fn a_symlink_loop_is_stopped_by_the_kernel_as_a_stat_error() {
        let temp = TempDir::new("fs-copy-loop").unwrap();
        let root = dir_of(&temp);
        let src = dir_at(&root, "src");
        fs::create_dir(raw_dir(&src)).unwrap();
        symlink(".", at(&src, "loop").get()).unwrap();
        let dst = dir_at(&root, "dst");
        let error = copy_dir(&src, &dst, raw_dir(&dst)).unwrap_err();
        assert_eq!(io_error(&error).map(|(op, _)| op), Some(FsOp::Stat));
        assert_eq!(errno_of(&error), Some(Errno::LOOP.raw_os_error()));
    }

    #[test]
    fn a_real_tree_deeper_than_the_limit_is_refused() {
        let temp = TempDir::new("fs-copy-deep").unwrap();
        let root = dir_of(&temp);
        let src = dir_at(&root, "src");
        let levels = vec!["d"; MAX_COPY_DEPTH + 1].join("/");
        fs::create_dir_all(format!("{}/{levels}", src.get())).unwrap();
        let dst = dir_at(&root, "dst");
        let error = copy_dir(&src, &dst, raw_dir(&dst)).unwrap_err();
        assert_eq!(
            error,
            FsError::TooDeep {
                path: src,
                max: MAX_COPY_DEPTH
            }
        );
        let shallow = vec!["d"; MAX_COPY_DEPTH].join("/");
        let ok_src = dir_at(&root, "ok");
        fs::create_dir_all(format!("{}/{shallow}", ok_src.get())).unwrap();
        let ok_dst = dir_at(&root, "ok-dst");
        copy_dir(&ok_src, &ok_dst, raw_dir(&ok_dst)).unwrap();
    }

    #[test]
    fn replacing_a_missing_directory_creates_it() {
        let temp = TempDir::new("fs-replace-missing").unwrap();
        let root = dir_of(&temp);
        let (src, target) = (dir_at(&root, "preset"), dir_at(&root, "vm/nix"));
        fs::create_dir_all(raw_dir(&src)).unwrap();
        plant_tree(&src);
        fs::create_dir(raw_dir(&root).join("vm")).unwrap();
        assert_eq!(replace_dir(&target, &src), Ok(Replacement::Atomic));
        assert_eq!(listing(raw_dir(&target)).len(), 6);
        assert_eq!(names(&raw_dir(&root).join("vm")), ["nix"]);
    }

    #[test]
    fn replacing_an_existing_directory_swaps_in_the_copy_and_removes_the_old_tree() {
        let temp = TempDir::new("fs-replace").unwrap();
        let root = dir_of(&temp);
        let (src, target) = (dir_at(&root, "preset"), dir_at(&root, "nix"));
        fs::create_dir_all(raw_dir(&src)).unwrap();
        plant_tree(&src);
        fs::create_dir_all(at(&target, "own/deep").get()).unwrap();
        fs::write(at(&target, "own/deep/mine.nix").get(), b"mine").unwrap();
        fs::write(at(&target, "flake.nix").get(), b"old").unwrap();
        let before = listing(raw_dir(&src));
        replace_dir(&target, &src).unwrap();
        assert_eq!(fs::read(at(&target, "flake.nix").get()).unwrap(), b"{ }");
        assert!(!at(&target, "own").get().is_empty() && !raw(&at(&target, "own")).exists());
        assert_eq!(names(temp.path()), ["nix", "preset"]);
        // The preset itself is untouched.
        assert_eq!(listing(raw_dir(&src)), before);
    }

    #[test]
    fn a_leftover_new_directory_of_a_crashed_run_is_cleared_first() {
        let temp = TempDir::new("fs-replace-leftover").unwrap();
        let root = dir_of(&temp);
        let (src, target) = (dir_at(&root, "preset"), dir_at(&root, "nix"));
        fs::create_dir_all(raw_dir(&src)).unwrap();
        fs::write(at(&src, "flake.nix").get(), b"fresh").unwrap();
        fs::create_dir(raw_dir(&target)).unwrap();
        fs::write(at(&target, "flake.nix").get(), b"old").unwrap();
        let leftover = dir_at(&root, ".nix.new");
        fs::create_dir_all(at(&leftover, "half").get()).unwrap();
        replace_dir(&target, &src).unwrap();
        assert_eq!(fs::read(at(&target, "flake.nix").get()).unwrap(), b"fresh");
        assert_eq!(names(temp.path()), ["nix", "preset"]);
        // A plain file in the way is cleared too.
        fs::write(raw_dir(&leftover), b"junk").unwrap();
        replace_dir(&target, &src).unwrap();
        assert_eq!(names(temp.path()), ["nix", "preset"]);
    }

    #[test]
    fn a_failing_copy_leaves_the_old_directory_and_no_new_one() {
        let temp = TempDir::new("fs-replace-fails").unwrap();
        let root = dir_of(&temp);
        let (src, target) = (dir_at(&root, "preset"), dir_at(&root, "nix"));
        fs::create_dir_all(raw_dir(&src)).unwrap();
        fs::write(at(&src, "flake.nix").get(), b"fresh").unwrap();
        symlink("nowhere", at(&src, "broken").get()).unwrap();
        fs::create_dir(raw_dir(&target)).unwrap();
        fs::write(at(&target, "flake.nix").get(), b"old").unwrap();
        let error = replace_dir(&target, &src).unwrap_err();
        assert_eq!(
            io_error(&error),
            Some((FsOp::Stat, io::ErrorKind::NotFound))
        );
        assert_eq!(fs::read(at(&target, "flake.nix").get()).unwrap(), b"old");
        assert_eq!(names(temp.path()), ["nix", "preset"]);
    }

    #[test]
    fn a_target_that_is_not_a_directory_is_refused_and_left_alone() {
        let temp = TempDir::new("fs-replace-file").unwrap();
        let root = dir_of(&temp);
        let (src, target) = (dir_at(&root, "preset"), dir_at(&root, "nix"));
        fs::create_dir_all(raw_dir(&src)).unwrap();
        fs::write(raw_dir(&target), b"a file").unwrap();
        let error = replace_dir(&target, &src).unwrap_err();
        assert_eq!(
            io_error(&error),
            Some((FsOp::Stat, io::ErrorKind::NotADirectory))
        );
        assert_eq!(fs::read(raw_dir(&target)).unwrap(), b"a file");
        assert_eq!(names(temp.path()), ["nix", "preset"]);
    }

    #[test]
    fn a_missing_source_or_parent_is_a_typed_error() {
        let temp = TempDir::new("fs-replace-missing-source").unwrap();
        let root = dir_of(&temp);
        let src = dir_at(&root, "preset");
        let error = replace_dir(&dir_at(&root, "nix"), &src).unwrap_err();
        assert_eq!(
            io_error(&error),
            Some((FsOp::Stat, io::ErrorKind::NotFound))
        );
        fs::create_dir(raw_dir(&src)).unwrap();
        let error = replace_dir(&dir_at(&root, "no/parent/nix"), &src).unwrap_err();
        assert_eq!(
            io_error(&error),
            Some((FsOp::Create, io::ErrorKind::NotFound))
        );
        assert_eq!(names(temp.path()), ["preset"]);
    }

    #[test]
    fn exchange_errors_of_an_unsupporting_filesystem_are_told_apart() {
        testing::table(
            &[
                (Errno::INVAL, ExchangeFailure::Unsupported),
                (Errno::NOSYS, ExchangeFailure::Unsupported),
                (Errno::OPNOTSUPP, ExchangeFailure::Unsupported),
                (Errno::ACCESS, ExchangeFailure::Other(Errno::ACCESS)),
                (Errno::NOENT, ExchangeFailure::Other(Errno::NOENT)),
            ],
            |errno| classify_exchange(*errno),
        );
    }

    /// A source tree with one file `flake.nix` = `fresh`, and a target with `flake.nix` = `old`.
    fn plant_replacement(label: &str) -> (TempDir, AbsDir, AbsDir, AbsDir) {
        let temp = TempDir::new(label).unwrap();
        let root = dir_of(&temp);
        let (src, target) = (dir_at(&root, "preset"), dir_at(&root, "nix"));
        fs::create_dir_all(raw_dir(&src)).unwrap();
        fs::write(at(&src, "flake.nix").get(), b"fresh").unwrap();
        fs::create_dir_all(at(&target, "own").get()).unwrap();
        fs::write(at(&target, "flake.nix").get(), b"old").unwrap();
        (temp, root, src, target)
    }

    #[test]
    fn a_filesystem_without_exchange_gets_the_two_rename_sequence_and_it_is_reported() {
        let (temp, root, src, target) = plant_replacement("fs-fallback");
        // A leftover of a crashed fallback is cleared first.
        fs::create_dir(raw_dir(&dir_at(&root, ".nix.old"))).unwrap();
        let how = replace_dir_with(&target, &src, |_, _| Err(ExchangeFailure::Unsupported));
        assert_eq!(how, Ok(Replacement::NonAtomic));
        assert_eq!(fs::read(at(&target, "flake.nix").get()).unwrap(), b"fresh");
        assert_eq!(listing(raw_dir(&target)), ["flake.nix"]);
        assert_eq!(names(temp.path()), ["nix", "preset"]);
    }

    #[test]
    fn an_exchange_that_fails_for_another_reason_is_an_error_and_keeps_the_old_tree() {
        let (temp, _root, src, target) = plant_replacement("fs-exchange-fails");
        let error = replace_dir_with(&target, &src, |_, _| {
            Err(ExchangeFailure::Other(Errno::ACCESS))
        })
        .unwrap_err();
        assert_eq!(
            io_error(&error),
            Some((FsOp::Exchange, io::ErrorKind::PermissionDenied))
        );
        assert_eq!(fs::read(at(&target, "flake.nix").get()).unwrap(), b"old");
        assert_eq!(names(temp.path()), ["nix", "preset"]);
    }

    #[test]
    fn a_sequence_whose_second_rename_fails_puts_the_old_tree_back() {
        let (temp, root, _src, target) = plant_replacement("fs-fallback-undo");
        let stale = raw_dir(&root).join(".nix.old");
        // `fresh` does not exist, so the second rename fails after the first succeeded.
        let error = sequential_swap(&raw_dir(&root).join(".nix.new"), &stale, &target).unwrap_err();
        assert_eq!(
            io_error(&error),
            Some((FsOp::Rename, io::ErrorKind::NotFound))
        );
        assert_eq!(fs::read(at(&target, "flake.nix").get()).unwrap(), b"old");
        assert_eq!(names(temp.path()), ["nix", "preset"]);
    }

    #[test]
    fn a_sequence_whose_first_rename_fails_changes_nothing() {
        let (temp, root, src, target) = plant_replacement("fs-fallback-first");
        let missing = dir_at(&root, "absent");
        let error = sequential_swap(
            &raw_dir(&src).join("x"),
            &raw_dir(&root).join(".s"),
            &missing,
        )
        .unwrap_err();
        assert_eq!(
            io_error(&error),
            Some((FsOp::Rename, io::ErrorKind::NotFound))
        );
        assert_eq!(fs::read(at(&target, "flake.nix").get()).unwrap(), b"old");
        assert_eq!(names(temp.path()), ["nix", "preset"]);
    }

    fn seen(path: &AbsPath) -> Observation {
        observe(path)
    }

    #[test]
    fn a_directory_is_observed_with_its_symlinks_resolved() {
        let temp = TempDir::new("fs-observe-dir").unwrap();
        let root = dir_of(&temp);
        let real = dir_at(&root, "real");
        fs::create_dir(raw_dir(&real)).unwrap();
        // The temp dir itself may sit below a symlink (/tmp -> ...): resolve it first.
        let canonical = |dir: &AbsDir| {
            AbsDir::parse(
                fs::canonicalize(raw_dir(dir))
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .to_owned(),
            )
            .unwrap()
        };
        symlink(raw_dir(&real), at(&root, "link").get()).unwrap();
        symlink("link", at(&root, "link-to-link").get()).unwrap();
        let expected = Observation::Directory {
            resolved: canonical(&real),
        };
        for name in ["real", "link", "link-to-link"] {
            assert_eq!(seen(&at(&root, name)), expected, "{name}");
        }
    }

    #[test]
    fn a_file_is_observed_resolved_and_executable_only_with_an_execute_bit() {
        let temp = TempDir::new("fs-observe-file").unwrap();
        let root = dir_of(&temp);
        let (plain, run) = (at(&root, "plain"), at(&root, "run"));
        fs::write(raw(&plain), b"x").unwrap();
        fs::write(raw(&run), b"#!/bin/sh").unwrap();
        fs::set_permissions(raw(&plain), fs::Permissions::from_mode(0o644)).unwrap();
        fs::set_permissions(raw(&run), fs::Permissions::from_mode(0o755)).unwrap();
        symlink(raw(&run), at(&root, "link").get()).unwrap();
        let resolved = |path: &AbsPath| {
            AbsPath::parse(
                fs::canonicalize(raw(path))
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .to_owned(),
            )
            .unwrap()
        };
        for (name, executable) in [("plain", false), ("run", true), ("link", true)] {
            let path = at(&root, name);
            assert_eq!(
                seen(&path),
                Observation::File {
                    resolved: resolved(&path),
                    executable
                },
                "{name}"
            );
        }
        assert_eq!(
            seen(&at(&root, "link")),
            Observation::File {
                resolved: resolved(&run),
                executable: true
            }
        );
    }

    #[test]
    fn a_missing_dangling_or_unreachable_path_is_missing() {
        let temp = TempDir::new("fs-observe-missing").unwrap();
        let root = dir_of(&temp);
        symlink("nowhere", at(&root, "dangling").get()).unwrap();
        symlink("loop", at(&root, "loop").get()).unwrap();
        fs::write(at(&root, "file").get(), b"").unwrap();
        for name in ["absent", "dangling", "loop", "file/below", "absent/below"] {
            assert_eq!(seen(&at(&root, name)), Observation::Missing, "{name}");
        }
    }

    #[test]
    fn sockets_and_pipes_are_other() {
        let temp = TempDir::new("fs-observe-other").unwrap();
        let root = dir_of(&temp);
        mknodat(CWD, at(&root, "pipe").get(), FileType::Fifo, Mode::RUSR, 0).unwrap();
        assert_eq!(seen(&at(&root, "pipe")), Observation::Other);
    }

    #[test]
    fn a_resolved_name_that_is_not_utf8_is_other() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt as _;

        let temp = TempDir::new("fs-observe-bytes").unwrap();
        let root = dir_of(&temp);
        let odd = temp.path().join(OsStr::from_bytes(b"odd-\xff"));
        fs::create_dir(&odd).unwrap();
        fs::write(odd.join("f"), b"").unwrap();
        symlink(&odd, at(&root, "to-dir").get()).unwrap();
        symlink(odd.join("f"), at(&root, "to-file").get()).unwrap();
        for name in ["to-dir", "to-file"] {
            assert_eq!(seen(&at(&root, name)), Observation::Other, "{name}");
        }
    }

    #[test]
    fn a_directory_without_search_permission_hides_what_is_below_it() {
        if rustix::process::geteuid().is_root() {
            return; // root ignores permission bits
        }
        let temp = TempDir::new("fs-observe-denied").unwrap();
        let root = dir_of(&temp);
        let locked = dir_at(&root, "locked");
        fs::create_dir(raw_dir(&locked)).unwrap();
        fs::write(at(&locked, "f").get(), b"").unwrap();
        fs::set_permissions(raw_dir(&locked), fs::Permissions::from_mode(0o000)).unwrap();
        let below = seen(&at(&root, "locked/f"));
        fs::set_permissions(raw_dir(&locked), fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(below, Observation::Missing);
    }

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
    }

    #[test]
    fn the_port_runs_the_same_operations_on_the_blocking_pool() {
        let temp = TempDir::new("fs-port").unwrap();
        let root = dir_of(&temp);
        let (src, target) = (dir_at(&root, "preset"), dir_at(&root, "vm/nix"));
        fs::create_dir_all(raw_dir(&src)).unwrap();
        fs::write(at(&src, "flake.nix").get(), b"flake").unwrap();
        let file: FileName = refined!(FileName, "config.kdl");
        let path = root.join(&file);
        runtime().block_on(async {
            let fs = StdFs;
            fs.create_dir_all(&dir_at(&root, "vm"), mode(0o755))
                .await
                .unwrap();
            fs.write_file(&path, b"text".to_vec(), mode(0o644))
                .await
                .unwrap();
            assert_eq!(fs.read_file(&path).await.unwrap(), b"text");
            assert_eq!(fs.replace_dir(&target, &src).await, Ok(Replacement::Atomic));
            assert!(matches!(
                fs.observe(&path).await,
                Ok(Observation::File {
                    executable: false,
                    ..
                })
            ));
            assert_eq!(
                fs.observe(&at(&root, "absent")).await,
                Ok(Observation::Missing)
            );
            assert_eq!(
                fs.read_file(&at(&target, "flake.nix")).await.unwrap(),
                b"flake"
            );
            let missing = fs.read_file(&at(&root, "absent")).await.unwrap_err();
            assert_eq!(
                io_error(&missing),
                Some((FsOp::Read, io::ErrorKind::NotFound))
            );
        });
        assert_eq!(file.segment(), "config.kdl");
    }

    #[test]
    fn a_write_whose_future_is_dropped_still_lands_whole() {
        use std::pin::pin;
        use std::task::{Context, Poll, Waker};

        let temp = TempDir::new("fs-cancel").unwrap();
        let path = at(&dir_of(&temp), "f");
        let runtime = runtime();
        {
            let _entered = runtime.enter();
            let mut future = pin!(StdFs.write_file(&path, vec![b'x'; 1 << 20], mode(0o644)));
            // One poll starts the blocking task; dropping the future abandons it.
            let first = future
                .as_mut()
                .poll(&mut Context::from_waker(Waker::noop()));
            assert!(matches!(first, Poll::Pending | Poll::Ready(Ok(()))));
        }
        // Dropping the runtime waits for the blocking task: the file is whole, no temp file left.
        drop(runtime);
        assert_eq!(fs::read(raw(&path)).unwrap().len(), 1 << 20);
        assert_eq!(names(temp.path()), ["f"]);
    }

    fn tree(root: &AbsDir) -> Result<Vec<(String, Vec<u8>)>, FsError> {
        read_tree(root).map(|files| {
            files
                .into_iter()
                .map(|file| (file.rel.to_string(), file.bytes))
                .collect()
        })
    }

    #[test]
    fn a_tree_is_its_regular_files_in_walk_order_without_links_or_special_files() {
        use std::os::unix::net::UnixListener;

        let temp = TempDir::new("fs-tree").unwrap();
        let root = dir_of(&temp);
        let base = temp.path();
        fs::create_dir_all(base.join("a/deep")).unwrap();
        fs::create_dir_all(base.join("empty")).unwrap();
        fs::create_dir_all(base.join("elsewhere")).unwrap();
        fs::write(base.join("flake.nix"), b"flake").unwrap();
        fs::write(base.join("a/x.nix"), b"x").unwrap();
        fs::write(base.join("a/deep/y.nix"), b"y").unwrap();
        fs::write(base.join("a-b"), b"dash").unwrap();
        fs::write(base.join("B"), b"upper").unwrap();
        fs::write(base.join("elsewhere/z"), b"z").unwrap();
        symlink(base.join("flake.nix"), base.join("link-file")).unwrap();
        symlink(base.join("elsewhere"), base.join("link-dir")).unwrap();
        symlink(base.join("nowhere"), base.join("dangling")).unwrap();
        let _socket = UnixListener::bind(base.join("sock")).unwrap();
        let expected: Vec<(String, Vec<u8>)> = [
            ("B", "upper"),
            ("a/deep/y.nix", "y"),
            ("a/x.nix", "x"),
            ("a-b", "dash"),
            ("elsewhere/z", "z"),
            ("flake.nix", "flake"),
        ]
        .map(|(rel, text)| (rel.to_owned(), text.as_bytes().to_vec()))
        .to_vec();
        assert_eq!(tree(&root), Ok(expected));
    }

    #[test]
    fn an_empty_tree_has_no_files_and_a_missing_one_is_a_list_error() {
        let temp = TempDir::new("fs-tree-empty").unwrap();
        let root = dir_of(&temp);
        assert_eq!(tree(&root), Ok(Vec::new()));
        let error = tree(&dir_at(&root, "absent")).unwrap_err();
        assert_eq!(
            io_error(&error),
            Some((FsOp::List, io::ErrorKind::NotFound))
        );
    }

    #[test]
    fn a_file_name_that_is_not_utf8_is_a_bad_name_with_its_lossy_path() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt as _;

        let temp = TempDir::new("fs-tree-name").unwrap();
        let root = dir_of(&temp);
        fs::create_dir(temp.path().join("sub")).unwrap();
        fs::write(
            temp.path()
                .join("sub")
                .join(OsStr::from_bytes(b"x\xff.nix")),
            b"x",
        )
        .unwrap();
        assert_eq!(
            tree(&root),
            Err(FsError::BadName {
                path: root,
                entry: "sub/x\u{fffd}.nix".to_owned(),
            })
        );
    }

    #[test]
    fn a_tree_deeper_than_the_copy_follows_is_too_deep() {
        let temp = TempDir::new("fs-tree-deep").unwrap();
        let root = dir_of(&temp);
        let mut deep = temp.path().to_path_buf();
        for _ in 0..=MAX_COPY_DEPTH {
            deep.push("d");
        }
        fs::create_dir_all(&deep).unwrap();
        assert_eq!(
            tree(&root),
            Err(FsError::TooDeep {
                path: root,
                max: MAX_COPY_DEPTH
            })
        );
    }

    #[test]
    fn a_file_mode_is_the_permission_bits_of_what_a_path_resolves_to() {
        let temp = TempDir::new("fs-mode").unwrap();
        let root = dir_of(&temp);
        let file = temp.path().join("f");
        fs::write(&file, b"x").unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o4750)).unwrap();
        symlink(&file, temp.path().join("l")).unwrap();
        runtime().block_on(async {
            for (name, expected) in [("f", 0o750), ("l", 0o750)] {
                let seen = StdFs.file_mode(&at(&root, name)).await.unwrap();
                assert_eq!(*seen.get(), expected, "{name}");
            }
            let error = StdFs.file_mode(&at(&root, "absent")).await.unwrap_err();
            assert_eq!(
                io_error(&error),
                Some((FsOp::Stat, io::ErrorKind::NotFound))
            );
            let listed = StdFs.read_tree(&root).await.unwrap();
            assert_eq!(listed.len(), 1);
        });
    }
}
