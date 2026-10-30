//! Fake ports (design §Testing, interpreter layer): pure state plus a call log, implementing the
//! real port traits. Every operation of every port is an [`OpId`]; calls are numbered per
//! operation, and a [`Script`] says "call N of operation X returns error E" with E a [`Fault`].
//!
//! An operation takes effect on its first poll and completes on the next one, so a future dropped
//! in between has already acted, exactly like a blocking task that outlives its caller; whatever
//! the effect produced (a held lock, a child) is owned by the future and goes with it.

#![allow(
    dead_code,
    reason = "the fakes' surface for the interpreter tests of the use cases (O3 onwards)"
)]

mod fs;
mod procs;
mod rest;

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::task::{Context, Poll};

pub use fs::{FakeFs, FakeGuest};
pub use procs::{FakeChild, FakeHeld, FakeLock, FakeProcs, Group, Started};
pub use rest::{FakeNix, FakeQmp, FakeSession, FakeSsh, FakeSystem};

use crate::domain::ProcIdentity;
use crate::ports::guest_delivery::DeliveryError;
use crate::ports::host_fs::FsError;
use crate::ports::nix::NixError;
use crate::ports::procs::{ProcError, SpawnError};
use crate::ports::qmp::QmpError;
use crate::ports::session::SessionError;
use crate::ports::ssh::{BannerError, FreePortError, KeyError};
use crate::ports::system::SystemError;
use crate::ports::vm_lock::LockError;

/// Locks `state`; a panic in another test thread must not hide this test's own failure.
pub(crate) fn held<T>(state: &Mutex<T>) -> MutexGuard<'_, T> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Every operation of every port.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum OpId {
    FsWriteFile,
    FsObserve,
    FsReadFile,
    FsFileMode,
    FsReadTree,
    FsRemoveFile,
    FsClearDir,
    FsCreateDirAll,
    FsReplaceDir,
    GuestEnsureDir,
    GuestPut,
    GuestRemove,
    GuestReplaceTree,
    LockAcquire,
    LockProbe,
    LockStdin,
    LockInheritable,
    ProcsIdentify,
    ProcsPresence,
    ProcsSignal,
    ProcsSpawn,
    ProcsSettle,
    ProcsWait,
    NixBuild,
    SystemKvm,
    SystemTool,
    SystemUserns,
    SystemKernel,
    SystemLegacyQemu,
    SshEnsureHostKey,
    SshFreePort,
    SshBanner,
    SshAccepts,
    SshPortFree,
    QmpPowerdown,
    SessionLinger,
    SessionEnableLinger,
}

/// An error a scripted call returns, typed by the port it belongs to.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Fault {
    Fs(FsError),
    Guest(DeliveryError),
    Lock(LockError),
    Proc(ProcError),
    Spawn(SpawnError),
    Nix(NixError),
    System(SystemError),
    Key(KeyError),
    FreePort(FreePortError),
    Banner(BannerError),
    Qmp(QmpError),
    Session(SessionError),
}

fn fs_faults(keep: fn(&FsError) -> bool) -> Vec<Fault> {
    crate::ports::host_fs::tests::errors()
        .into_iter()
        .filter(keep)
        .map(Fault::Fs)
        .collect()
}

fn lock_faults(keep: fn(&LockError) -> bool) -> Vec<Fault> {
    crate::ports::vm_lock::tests::errors()
        .into_iter()
        .filter(keep)
        .map(Fault::Lock)
        .collect()
}

fn proc_faults(keep: fn(&ProcError) -> bool) -> Vec<Fault> {
    crate::ports::procs::tests::errors()
        .into_iter()
        .filter(keep)
        .map(Fault::Proc)
        .collect()
}

impl OpId {
    /// Every error the operation can return, one value per variant (the sample of the port's own
    /// catalogue tests).
    #[must_use]
    pub fn faults(self) -> Vec<Fault> {
        match self {
            Self::FsWriteFile
            | Self::FsObserve
            | Self::FsReadFile
            | Self::FsFileMode
            | Self::FsRemoveFile
            | Self::FsClearDir
            | Self::FsCreateDirAll => {
                fs_faults(|e| matches!(e, FsError::Io { .. } | FsError::Aborted { .. }))
            }
            Self::FsReadTree => fs_faults(|_| true),
            Self::FsReplaceDir => fs_faults(|e| !matches!(e, FsError::BadName { .. })),
            Self::GuestEnsureDir | Self::GuestPut | Self::GuestRemove | Self::GuestReplaceTree => {
                crate::ports::guest_delivery::tests::errors()
                    .into_iter()
                    .map(Fault::Guest)
                    .collect()
            }
            Self::LockAcquire => lock_faults(|_| true),
            Self::LockProbe => lock_faults(|e| !matches!(e, LockError::Held { .. })),
            Self::LockStdin | Self::LockInheritable => {
                lock_faults(|e| matches!(e, LockError::Io { .. }))
            }
            Self::ProcsIdentify => proc_faults(|_| true),
            Self::ProcsSettle => proc_faults(|e| matches!(e, ProcError::Io { .. })),
            Self::ProcsPresence | Self::ProcsSignal => {
                proc_faults(|e| !matches!(e, ProcError::NoSuchProcess { .. }))
            }
            Self::ProcsWait => proc_faults(|e| matches!(e, ProcError::Io { .. })),
            Self::ProcsSpawn => crate::ports::procs::tests::spawn_errors()
                .into_iter()
                .map(Fault::Spawn)
                .collect(),
            Self::NixBuild => crate::ports::nix::tests::errors()
                .into_iter()
                .map(Fault::Nix)
                .collect(),
            Self::SystemKvm
            | Self::SystemTool
            | Self::SystemUserns
            | Self::SystemKernel
            | Self::SystemLegacyQemu => crate::ports::system::tests::errors()
                .into_iter()
                .map(Fault::System)
                .collect(),
            Self::SshEnsureHostKey => crate::ports::ssh::tests::key_errors()
                .into_iter()
                .map(Fault::Key)
                .collect(),
            Self::SshFreePort => crate::ports::ssh::tests::port_errors()
                .into_iter()
                .map(Fault::FreePort)
                .collect(),
            Self::SshBanner => crate::ports::ssh::tests::banner_errors()
                .into_iter()
                .map(Fault::Banner)
                .collect(),
            Self::SshAccepts | Self::SshPortFree => Vec::new(),
            Self::QmpPowerdown => crate::ports::qmp::tests::errors()
                .into_iter()
                .map(Fault::Qmp)
                .collect(),
            Self::SessionLinger => crate::ports::session::tests::errors()
                .into_iter()
                .filter(|e| matches!(e, SessionError::Aborted))
                .map(Fault::Session)
                .collect(),
            Self::SessionEnableLinger => crate::ports::session::tests::errors()
                .into_iter()
                .map(Fault::Session)
                .collect(),
        }
    }
}

/// The `n`th call (from 0) of an operation.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct Call {
    pub op: OpId,
    pub n: usize,
}

#[derive(Default)]
struct ScriptState {
    counts: BTreeMap<OpId, usize>,
    log: Vec<Call>,
    faults: BTreeMap<Call, Fault>,
}

/// The call log of a world and the failures scripted into it; shared by all its fakes.
#[derive(Clone, Default)]
pub struct Script(Arc<Mutex<ScriptState>>);

impl Script {
    /// Scripts call `call` to return `fault`.
    pub fn fail(&self, call: Call, fault: Fault) {
        held(&self.0).faults.insert(call, fault);
    }

    /// Every call so far, in order.
    #[must_use]
    pub fn calls(&self) -> Vec<Call> {
        held(&self.0).log.clone()
    }

    /// Logs a call of `op`; the scripted failure, if any.
    pub(crate) fn enter(&self, op: OpId) -> Option<Fault> {
        let mut state = held(&self.0);
        let count = state.counts.entry(op).or_insert(0);
        let call = Call { op, n: *count };
        *count = count.saturating_add(1);
        state.log.push(call);
        state.faults.get(&call).cloned()
    }
}

/// Pending once, then ready: the point where a caller can be dropped between an effect and its
/// result.
pub(crate) async fn pause() {
    struct Pause(bool);
    impl Future for Pause {
        type Output = ();
        fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
            if self.0 {
                Poll::Ready(())
            } else {
                self.0 = true;
                cx.waker().wake_by_ref();
                Poll::Pending
            }
        }
    }
    Pause(false).await;
}

macro_rules! extract {
    ($($name:ident: $variant:ident($error:ty)),* $(,)?) => {
        impl Fault {
            $(
                pub(crate) fn $name(self) -> Option<$error> {
                    if let Self::$variant(error) = self { Some(error) } else { None }
                }
            )*
        }
    };
}

extract! {
    fs: Fs(FsError), guest: Guest(DeliveryError), lock: Lock(LockError), proc: Proc(ProcError),
    spawn: Spawn(SpawnError), nix: Nix(NixError), system: System(SystemError), key: Key(KeyError),
    free_port: FreePort(FreePortError), banner: Banner(BannerError), qmp: Qmp(QmpError),
    session: Session(SessionError),
}

/// One call of `op`: logged and, unless a failure is scripted for it, `effect` runs now (first
/// poll); the result is delivered on the next poll. A scripted failure of another port's type is
/// ignored, which the sweeps notice as an unexpected outcome.
pub(crate) async fn step<T, E>(
    script: &Script,
    op: OpId,
    scripted: fn(Fault) -> Option<E>,
    effect: impl FnOnce() -> Result<T, E>,
) -> Result<T, E> {
    let out = match script.enter(op).and_then(scripted) {
        Some(error) => Err(error),
        None => effect(),
    };
    pause().await;
    out
}

/// What a finished run left behind that it should have released.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Leak {
    /// A process of the run is still alive.
    Process(ProcIdentity),
    /// The lock still has this many holders (handles and processes).
    Lock { holders: usize },
}

/// All fakes sharing one script and one lock.
#[derive(Clone)]
pub struct World {
    pub script: Script,
    pub fs: FakeFs,
    pub guest: FakeGuest,
    pub lock: FakeLock,
    pub procs: FakeProcs,
    pub nix: FakeNix,
    pub system: FakeSystem,
    pub ssh: FakeSsh,
    pub qmp: FakeQmp,
    pub session: FakeSession,
}

impl Default for World {
    fn default() -> Self {
        Self::new()
    }
}

impl World {
    #[must_use]
    pub fn new() -> Self {
        let script = Script::default();
        let lock = FakeLock::new(&script);
        Self {
            fs: FakeFs::new(&script),
            guest: FakeGuest::new(&script),
            procs: FakeProcs::new(&script, &lock),
            nix: FakeNix::new(&script),
            system: FakeSystem::new(&script),
            ssh: FakeSsh::new(&script),
            qmp: FakeQmp::new(&script),
            session: FakeSession::new(&script),
            lock,
            script,
        }
    }

    /// The release invariant: no live process of the run, and the lock free unless the operation
    /// hands it on. Empty when it holds.
    #[must_use]
    pub fn leaks(&self, lock_handed_on: bool) -> Vec<Leak> {
        let mut leaks: Vec<Leak> = self
            .procs
            .alive_from_run()
            .into_iter()
            .map(Leak::Process)
            .collect();
        let holders = self.lock.holders();
        if holders > 0 && !lock_handed_on {
            leaks.push(Leak::Lock { holders });
        }
        leaks
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::domain::{
        AbsDir, AbsPath, FileMode, GuestFile, GuestRel, LoopbackPort, Observation, UserName,
    };
    use crate::plan::loginctl_argv;
    use crate::ports::guest_delivery::GuestDelivery as _;
    use crate::ports::host_fs::{HostFs as _, Replacement};
    use crate::ports::procs::{
        ChildIo, Exit, LockHandoff, Presence, Procs as _, Signal, Signalled, Spawned as _,
    };
    use crate::ports::ssh::Ssh as _;
    use crate::ports::vm_lock::{Availability, VmLock as _};
    use crate::testing::sweep::paused_runtime;

    fn path(text: &str) -> AbsPath {
        AbsPath::parse(text.to_owned()).unwrap()
    }

    fn dir(text: &str) -> AbsDir {
        AbsDir::parse(text.to_owned()).unwrap()
    }

    fn mode() -> FileMode {
        FileMode::parse(0o644).unwrap()
    }

    fn rel(text: &str) -> GuestRel {
        GuestRel::parse(text.to_owned()).unwrap()
    }

    #[test]
    fn host_files_need_their_directory_and_removal_is_idempotent() {
        let world = World::new();
        paused_runtime().unwrap().block_on(async {
            let fs = &world.fs;
            assert!(
                fs.write_file(&path("/d/f"), b"x".to_vec(), mode())
                    .await
                    .is_err()
            );
            fs.create_dir_all(&dir("/d/sub"), mode()).await.unwrap();
            fs.write_file(&path("/d/sub/f"), b"x".to_vec(), mode())
                .await
                .unwrap();
            assert_eq!(fs.read_file(&path("/d/sub/f")).await.unwrap(), b"x");
            assert_eq!(fs.file_mode(&path("/d/sub/f")).await.unwrap(), mode());
            assert_eq!(
                fs.observe(&path("/d/sub/f")).await.unwrap(),
                Observation::File {
                    resolved: path("/d/sub/f"),
                    executable: false
                }
            );
            assert_eq!(
                fs.observe(&path("/d/sub")).await.unwrap(),
                Observation::Directory {
                    resolved: dir("/d/sub")
                }
            );
            assert_eq!(
                fs.observe(&path("/nope")).await.unwrap(),
                Observation::Missing
            );
            fs.remove_file(&path("/d/sub/f")).await.unwrap();
            fs.remove_file(&path("/d/sub/f")).await.unwrap();
            fs.write_file(&path("/d/sub/g"), b"y".to_vec(), mode())
                .await
                .unwrap();
            fs.clear_dir(&dir("/d")).await.unwrap();
            fs.clear_dir(&dir("/d")).await.unwrap();
            assert_eq!(fs.paths(), Vec::<String>::new());
            assert_eq!(
                fs.observe(&path("/d/sub")).await.unwrap(),
                Observation::Missing
            );
        });
    }

    #[test]
    fn replacing_a_dir_copies_the_source_over_the_old_target_and_read_tree_lists_it() {
        let world = World::new();
        paused_runtime().unwrap().block_on(async {
            let fs = &world.fs;
            fs.add_file(&path("/src/a"), b"1", mode());
            fs.add_file(&path("/src/d/b"), b"2", mode());
            fs.add_file(&path("/dst/old"), b"3", mode());
            assert_eq!(
                fs.replace_dir(&dir("/dst"), &dir("/src")).await.unwrap(),
                Replacement::Atomic
            );
            let tree = fs.read_tree(&dir("/dst")).await.unwrap();
            let listed: Vec<(&str, &[u8])> = tree
                .iter()
                .map(|f| (f.rel.get().as_str(), f.bytes.as_slice()))
                .collect();
            assert_eq!(listed, [("a", &b"1"[..]), ("d/b", &b"2"[..])]);
        });
    }

    #[test]
    fn delivered_guest_files_replace_a_tree_whole() {
        let world = World::new();
        let file = |dest: &str| GuestFile {
            dest: rel(dest),
            bytes: dest.as_bytes().to_vec(),
            mode: mode(),
        };
        paused_runtime().unwrap().block_on(async {
            let guest = &world.guest;
            guest.put(&file("home/x")).await.unwrap();
            guest
                .replace_tree(&rel("home/nix"), &[file("a"), file("d/b")])
                .await
                .unwrap();
            guest
                .replace_tree(&rel("home/nix"), &[file("c")])
                .await
                .unwrap();
            assert_eq!(guest.paths(), ["home/nix/c", "home/x"]);
            guest.remove(&rel("home/x")).await.unwrap();
            assert_eq!(guest.content(&rel("home/nix/c")), Some(b"c".to_vec()));
            guest.ensure_dir(&rel("home/empty")).await.unwrap();
        });
    }

    #[test]
    fn a_child_holds_the_lock_until_it_is_signalled_waited_for_or_dropped() {
        let world = World::new();
        let user = UserName::parse("agent".to_owned()).unwrap();
        paused_runtime().unwrap().block_on(async {
            let lock = world.lock.acquire(&path("/l")).await.unwrap();
            let mut child = world
                .procs
                .spawn(
                    &loginctl_argv(&user),
                    &AbsDir::root(),
                    LockHandoff::Stdin(&lock),
                    ChildIo::Null,
                )
                .unwrap();
            drop(lock);
            assert_eq!(
                world.lock.probe(&path("/l")).await.unwrap(),
                Availability::Held
            );
            let who = child.identity().clone();
            assert_eq!(world.procs.presence(&who).await.unwrap(), Presence::Alive);
            assert_eq!(world.procs.identify(who.pid).await.unwrap(), who);
            let other = ProcIdentity {
                start: crate::domain::StartTime::parse(99).unwrap(),
                ..who.clone()
            };
            assert_eq!(
                world.procs.presence(&other).await.unwrap(),
                Presence::Replaced
            );
            assert_eq!(
                world.procs.signal(&other, Signal::Kill).await.unwrap(),
                Signalled::Replaced
            );
            assert_eq!(
                world.procs.signal(&who, Signal::Terminate).await.unwrap(),
                Signalled::Sent
            );
            assert_eq!(
                world.procs.signal(&who, Signal::Terminate).await.unwrap(),
                Signalled::Gone
            );
            assert_eq!(world.procs.presence(&who).await.unwrap(), Presence::Gone);
            assert_eq!(
                world.lock.probe(&path("/l")).await.unwrap(),
                Availability::Free
            );
            assert_eq!(child.wait().await.unwrap(), Exit::Signal(15));
        });
    }

    #[test]
    fn waiting_ends_a_live_child_as_the_world_says_and_an_unwaited_drop_terminates_it() {
        let world = World::new();
        let user = UserName::parse("agent".to_owned()).unwrap();
        world.procs.end_waits_with(Exit::Code(3));
        paused_runtime().unwrap().block_on(async {
            let mut waited = world
                .procs
                .spawn(
                    &loginctl_argv(&user),
                    &AbsDir::root(),
                    LockHandoff::<FakeHeld>::None,
                    ChildIo::Null,
                )
                .unwrap();
            let dropped = world
                .procs
                .spawn(
                    &loginctl_argv(&user),
                    &AbsDir::root(),
                    LockHandoff::<FakeHeld>::None,
                    ChildIo::Null,
                )
                .unwrap();
            assert_eq!(world.procs.alive_from_run().len(), 2);
            assert_eq!(waited.wait().await.unwrap(), Exit::Code(3));
            drop(dropped);
            assert_eq!(world.procs.alive_from_run(), []);
            assert_eq!(world.procs.started().len(), 2);
        });
    }

    #[test]
    fn every_operation_has_at_least_one_fault_and_faults_are_typed_for_their_port() {
        let all = [
            OpId::FsWriteFile,
            OpId::FsObserve,
            OpId::FsReadFile,
            OpId::FsFileMode,
            OpId::FsReadTree,
            OpId::FsRemoveFile,
            OpId::FsClearDir,
            OpId::FsCreateDirAll,
            OpId::FsReplaceDir,
            OpId::GuestEnsureDir,
            OpId::GuestPut,
            OpId::GuestRemove,
            OpId::GuestReplaceTree,
            OpId::LockAcquire,
            OpId::LockProbe,
            OpId::LockStdin,
            OpId::LockInheritable,
            OpId::ProcsIdentify,
            OpId::ProcsPresence,
            OpId::ProcsSignal,
            OpId::ProcsSpawn,
            OpId::ProcsSettle,
            OpId::ProcsWait,
            OpId::NixBuild,
            OpId::SystemKvm,
            OpId::SystemTool,
            OpId::SystemUserns,
            OpId::SystemKernel,
            OpId::SystemLegacyQemu,
            OpId::SshEnsureHostKey,
            OpId::SshFreePort,
            OpId::SshBanner,
            OpId::QmpPowerdown,
            OpId::SessionLinger,
            OpId::SessionEnableLinger,
        ];
        for op in all {
            assert!(!op.faults().is_empty(), "{op:?} has no fault");
        }
        assert_eq!(
            OpId::LockProbe.faults().len(),
            3,
            "Held is not a probe error"
        );
        assert_eq!(OpId::FsReadTree.faults().len(), 4);
        for op in [OpId::SshAccepts, OpId::SshPortFree] {
            assert_eq!(op.faults(), [], "{op:?} answers a plain yes or no");
        }
    }

    #[test]
    fn a_spawn_records_where_and_in_which_group_and_settling_follows_the_runners_exec() {
        use super::{Group, Started};
        let world = World::new();
        let dir = AbsDir::parse("/run/user/1/amb_vm".to_owned()).unwrap();
        let file = path("/store/runner/bin/microvm-run");
        let runner = crate::domain::Executable::observe(&file, |file| Observation::File {
            resolved: file.clone(),
            executable: true,
        })
        .unwrap();
        let argv = crate::plan::runner_argv(&runner);
        paused_runtime().unwrap().block_on(async {
            let mut child = world
                .procs
                .spawn(&argv, &dir, LockHandoff::<FakeHeld>::None, ChildIo::Inherit)
                .unwrap();
            let other = world
                .procs
                .spawn(&argv, &dir, LockHandoff::<FakeHeld>::None, ChildIo::Null)
                .unwrap();
            let before = child.identity().clone();
            assert_eq!(before.program.get(), "microvm-run");
            let after = child.settle().await.unwrap();
            assert_eq!(after.program.get(), "qemu-system-x86_64");
            assert_eq!(after.pid, before.pid);
            assert_eq!(world.procs.presence(&after).await.unwrap(), Presence::Alive);
            assert_eq!(
                world.procs.presence(&before).await.unwrap(),
                Presence::Replaced
            );
            assert_eq!(child.release(), after);
            assert_eq!(
                world.procs.started_with(),
                [
                    Started {
                        identity: after,
                        cwd: dir.clone(),
                        group: Group::Terminal
                    },
                    Started {
                        identity: other.identity().clone(),
                        cwd: dir,
                        group: Group::Own
                    },
                ]
            );
        });
    }

    #[test]
    fn the_fake_ssh_knows_taken_and_listening_ports() {
        let world = World::new();
        let port = LoopbackPort::parse(4000).unwrap();
        paused_runtime().unwrap().block_on(async {
            assert!(world.ssh.port_free(port).await);
            assert!(!world.ssh.accepts(port).await);
            assert!(world.ssh.banner(port).await.is_err());
            world.ssh.take(port);
            world.ssh.listen(port);
            assert!(!world.ssh.port_free(port).await);
            assert!(world.ssh.accepts(port).await);
            assert!(world.ssh.banner(port).await.is_ok());
        });
    }
}
