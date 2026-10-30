//! `StdLock`: [`VmLock`] on `flock(2)` through `std::fs::File`. The lock belongs to the open file
//! description, so a `dup`ed or inherited descriptor keeps it until the last one is closed, also
//! when its holder is killed.

use std::fs::{File, TryLockError};
use std::future::Future;
use std::io;
use std::process::Stdio;

use rustix::fs::{Mode, OFlags};
use rustix::io::Errno;

use crate::domain::AbsPath;
use crate::ports::Cause;
use crate::ports::vm_lock::{Availability, HeldLock, Inheritable, LockError, LockOp, VmLock};

/// Advisory locks through `flock`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct StdLock;

/// An open lock file whose lock is held; every descriptor derived from it shares the lock.
#[derive(Debug)]
pub struct HeldFile {
    file: File,
    path: AbsPath,
}

/// Opens the lock file: never through a symlink, close-on-exec, owner-only when created.
fn open(path: &AbsPath, create: bool) -> io::Result<File> {
    let create = if create {
        OFlags::CREATE
    } else {
        OFlags::empty()
    };
    rustix::fs::open(
        path.get().as_str(),
        OFlags::RDWR | OFlags::NOFOLLOW | OFlags::CLOEXEC | create,
        Mode::from_raw_mode(0o600),
    )
    .map(File::from)
    .map_err(io::Error::from)
}

fn io_error(op: LockOp, path: &AbsPath, error: &io::Error) -> LockError {
    LockError::Io {
        op,
        path: path.clone(),
        cause: Cause::of(error),
    }
}

/// No `flock` on this filesystem (`ENOLCK` from NFS and the like, `ENOSYS`, `EOPNOTSUPP`).
fn is_unsupported(error: &io::Error) -> bool {
    [Errno::NOLCK, Errno::NOSYS, Errno::OPNOTSUPP]
        .iter()
        .any(|errno| error.raw_os_error() == Some(errno.raw_os_error()))
}

/// The lock attempt on an open file, classified.
fn try_lock(file: &File, path: &AbsPath) -> Result<(), LockError> {
    match file.try_lock() {
        Ok(()) => Ok(()),
        Err(TryLockError::WouldBlock) => Err(LockError::Held { path: path.clone() }),
        Err(TryLockError::Error(error)) if is_unsupported(&error) => {
            Err(LockError::Unsupported { path: path.clone() })
        }
        Err(TryLockError::Error(error)) => Err(io_error(LockOp::Lock, path, &error)),
    }
}

fn acquire_now(path: &AbsPath) -> Result<HeldFile, LockError> {
    let file = open(path, true).map_err(|error| io_error(LockOp::Open, path, &error))?;
    try_lock(&file, path)?;
    Ok(HeldFile {
        file,
        path: path.clone(),
    })
}

fn probe_now(path: &AbsPath) -> Result<Availability, LockError> {
    let file = match open(path, false) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Availability::Free),
        Err(error) => return Err(io_error(LockOp::Open, path, &error)),
    };
    match try_lock(&file, path) {
        Ok(()) => Ok(Availability::Free),
        Err(LockError::Held { .. }) => Ok(Availability::Held),
        Err(other) => Err(other),
    }
}

async fn blocking<T: Send + 'static>(
    path: AbsPath,
    work: impl FnOnce(&AbsPath) -> Result<T, LockError> + Send + 'static,
) -> Result<T, LockError> {
    let again = path.clone();
    tokio::task::spawn_blocking(move || work(&path))
        .await
        .unwrap_or_else(|_| Err(LockError::Aborted { path: again }))
}

impl VmLock for StdLock {
    type Held = HeldFile;

    fn acquire(&self, path: &AbsPath) -> impl Future<Output = Result<HeldFile, LockError>> + Send {
        blocking(path.clone(), acquire_now)
    }

    fn probe(
        &self,
        path: &AbsPath,
    ) -> impl Future<Output = Result<Availability, LockError>> + Send {
        blocking(path.clone(), probe_now)
    }
}

impl HeldLock for HeldFile {
    fn stdin(&self) -> Result<Stdio, LockError> {
        // `try_clone` is `F_DUPFD_CLOEXEC`; the child's `dup2` onto fd 0 clears the flag there.
        self.file
            .try_clone()
            .map(Stdio::from)
            .map_err(|error| io_error(LockOp::Duplicate, &self.path, &error))
    }

    fn inheritable(&self) -> Result<Inheritable, LockError> {
        // Plain `dup` leaves `FD_CLOEXEC` cleared.
        rustix::io::dup(&self.file)
            .map(Inheritable::new)
            .map_err(|errno| io_error(LockOp::Duplicate, &self.path, &io::Error::from(errno)))
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use std::fs;
    use std::os::fd::AsFd as _;
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _, symlink};
    use std::process::{Child, Command};

    use rustix::io::{FdFlags, fcntl_getfd};

    use super::*;
    use crate::domain::AbsDir;
    use crate::testing::{self, TempDir};

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
    }

    fn lock_path(temp: &TempDir) -> AbsPath {
        let dir = AbsDir::normalising(temp.path().to_str().unwrap()).unwrap();
        AbsPath::parse(format!("{}/lock", dir.get())).unwrap()
    }

    fn acquire(path: &AbsPath) -> Result<HeldFile, LockError> {
        runtime().block_on(StdLock.acquire(path))
    }

    fn probe(path: &AbsPath) -> Result<Availability, LockError> {
        runtime().block_on(StdLock.probe(path))
    }

    /// A `sleep` that dies with the test.
    struct Sleeper(Child);

    impl Sleeper {
        fn start(stdin: Stdio) -> Self {
            Self(
                Command::new("sleep")
                    .arg("60")
                    .stdin(stdin)
                    .spawn()
                    .unwrap(),
            )
        }

        fn end(mut self) {
            self.0.kill().unwrap();
            self.0.wait().unwrap();
        }
    }

    impl Drop for Sleeper {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    #[test]
    fn the_lock_file_is_made_private_on_demand_and_never_deleted() {
        let _quiet = testing::one_spawn_at_a_time();
        let temp = TempDir::new("lock-file").unwrap();
        let path = lock_path(&temp);
        assert!(fs::metadata(path.get()).is_err());
        let held = acquire(&path).unwrap();
        assert_eq!(fs::metadata(path.get()).unwrap().mode() & 0o7777, 0o600);
        drop(held);
        assert!(fs::metadata(path.get()).is_ok());
        assert_eq!(probe(&path), Ok(Availability::Free));
        assert!(fs::metadata(path.get()).is_ok());
        drop(acquire(&path).unwrap());
        assert!(fs::metadata(path.get()).is_ok());
    }

    #[test]
    fn a_held_lock_refuses_a_second_acquire_and_probes_as_held_until_dropped() {
        let _quiet = testing::one_spawn_at_a_time();
        let temp = TempDir::new("lock-held").unwrap();
        let path = lock_path(&temp);
        let held = acquire(&path).unwrap();
        assert_eq!(
            acquire(&path).map(|_| ()),
            Err(LockError::Held { path: path.clone() })
        );
        assert_eq!(probe(&path), Ok(Availability::Held));
        drop(held);
        assert_eq!(probe(&path), Ok(Availability::Free));
        assert!(acquire(&path).is_ok());
    }

    #[test]
    fn probing_a_missing_lock_file_is_free_and_creates_nothing() {
        let _quiet = testing::one_spawn_at_a_time();
        let temp = TempDir::new("lock-probe-missing").unwrap();
        let path = lock_path(&temp);
        assert_eq!(probe(&path), Ok(Availability::Free));
        assert!(fs::metadata(path.get()).is_err());
    }

    #[test]
    fn probing_does_not_keep_the_lock() {
        let _quiet = testing::one_spawn_at_a_time();
        let temp = TempDir::new("lock-probe-release").unwrap();
        let path = lock_path(&temp);
        drop(acquire(&path).unwrap());
        assert_eq!(probe(&path), Ok(Availability::Free));
        assert_eq!(probe(&path), Ok(Availability::Free));
        assert!(acquire(&path).is_ok());
    }

    #[test]
    fn a_child_that_inherited_stdin_holds_the_lock_until_it_exits() {
        let _quiet = testing::one_spawn_at_a_time();
        let temp = TempDir::new("lock-stdin").unwrap();
        let path = lock_path(&temp);
        let held = acquire(&path).unwrap();
        let child = Sleeper::start(held.stdin().unwrap());
        drop(held);
        assert_eq!(probe(&path), Ok(Availability::Held));
        assert_eq!(
            acquire(&path).map(|_| ()),
            Err(LockError::Held { path: path.clone() })
        );
        child.end();
        assert_eq!(probe(&path), Ok(Availability::Free));
        assert!(acquire(&path).is_ok());
    }

    #[test]
    fn a_child_that_inherited_the_cleared_dup_holds_the_lock_until_it_exits() {
        let _quiet = testing::one_spawn_at_a_time();
        let temp = TempDir::new("lock-dup").unwrap();
        let path = lock_path(&temp);
        let held = acquire(&path).unwrap();
        let inheritable = held.inheritable().unwrap();
        let child = Sleeper::start(Stdio::null());
        drop(inheritable);
        drop(held);
        assert_eq!(probe(&path), Ok(Availability::Held));
        child.end();
        assert_eq!(probe(&path), Ok(Availability::Free));
    }

    #[test]
    fn a_child_spawned_without_a_handle_does_not_hold_the_lock() {
        let _quiet = testing::one_spawn_at_a_time();
        let temp = TempDir::new("lock-no-inherit").unwrap();
        let path = lock_path(&temp);
        let held = acquire(&path).unwrap();
        let child = Sleeper::start(Stdio::null());
        drop(held);
        assert_eq!(probe(&path), Ok(Availability::Free));
        child.end();
    }

    #[test]
    fn only_the_inheritable_dup_lacks_close_on_exec() {
        let _quiet = testing::one_spawn_at_a_time();
        let temp = TempDir::new("lock-flags").unwrap();
        let held = acquire(&lock_path(&temp)).unwrap();
        let flags = |fd: &dyn Fn() -> FdFlags| fd();
        assert_eq!(
            flags(&|| fcntl_getfd(held.file.as_fd()).unwrap()),
            FdFlags::CLOEXEC
        );
        let inheritable = held.inheritable().unwrap();
        assert_eq!(
            flags(&|| fcntl_getfd(inheritable.as_fd()).unwrap()),
            FdFlags::empty()
        );
    }

    #[test]
    fn a_symlink_at_the_lock_path_is_not_followed() {
        let _quiet = testing::one_spawn_at_a_time();
        let temp = TempDir::new("lock-symlink").unwrap();
        let path = lock_path(&temp);
        fs::write(temp.path().join("elsewhere"), b"").unwrap();
        symlink(temp.path().join("elsewhere"), path.get()).unwrap();
        for result in [acquire(&path).map(|_| ()), probe(&path).map(|_| ())] {
            assert!(
                matches!(
                    result,
                    Err(LockError::Io {
                        op: LockOp::Open,
                        ..
                    })
                ),
                "{result:?}"
            );
        }
    }

    #[test]
    fn a_missing_directory_is_an_open_error_naming_the_lock_file() {
        let _quiet = testing::one_spawn_at_a_time();
        let temp = TempDir::new("lock-no-dir").unwrap();
        let path =
            AbsPath::parse(format!("{}/gone/lock", lock_path(&temp).parent().get())).unwrap();
        assert_eq!(
            acquire(&path).map(|_| ()),
            Err(LockError::Io {
                op: LockOp::Open,
                path,
                cause: Cause {
                    kind: io::ErrorKind::NotFound,
                    errno: Some(Errno::NOENT.raw_os_error())
                }
            })
        );
    }

    #[test]
    fn an_unreadable_lock_file_is_an_open_error_for_probe_too() {
        if rustix::process::geteuid().is_root() {
            return;
        }
        let temp = TempDir::new("lock-denied").unwrap();
        let path = lock_path(&temp);
        drop(acquire(&path).unwrap());
        fs::set_permissions(path.get(), fs::Permissions::from_mode(0o000)).unwrap();
        assert!(matches!(
            probe(&path),
            Err(LockError::Io {
                op: LockOp::Open,
                cause: Cause {
                    kind: io::ErrorKind::PermissionDenied,
                    ..
                },
                ..
            })
        ));
    }

    #[test]
    fn only_the_errnos_of_a_filesystem_without_locks_read_as_unsupported() {
        let _quiet = testing::one_spawn_at_a_time();
        testing::table(
            &[
                (Errno::NOLCK, true),
                (Errno::NOSYS, true),
                (Errno::OPNOTSUPP, true),
                (Errno::AGAIN, false),
                (Errno::ACCESS, false),
                (Errno::BADF, false),
            ],
            |errno| is_unsupported(&io::Error::from(*errno)),
        );
        assert!(!is_unsupported(&io::Error::other("no errno")));
    }
}
