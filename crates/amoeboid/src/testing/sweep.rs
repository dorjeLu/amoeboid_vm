//! The three interpreter sweeps (design §Interpreter sweeps), enumerated rather than random.
//! A [`Subject`] is an operation over a fresh fake [`World`]; the sweeps run it again and again
//! with a failure scripted into one call, with a failure scripted into one call plus one of the
//! calls that follow it (the cleanup), and dropped after `m` polls. Every run must end with the
//! release invariant ([`World::leaks`]) intact. Run them on a runtime with paused time
//! ([`paused_runtime`]): a pending operation advances the clock instead of sleeping.

use std::fmt::Debug;
use std::future::Future;
use std::pin::{Pin, pin};
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use super::fakes::{Call, Fault, Leak, World};

/// A current-thread runtime whose clock only moves when told to.
///
/// # Errors
///
/// The runtime can't be built.
pub fn paused_runtime() -> std::io::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .start_paused(true)
        .build()
}

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;

/// An operation under test.
pub trait Subject {
    type Ok: Debug;
    type Err: PartialEq + Debug;

    /// A fresh world with the scenario's setup done.
    fn world(&self) -> World;

    /// The operation, run against `world`.
    fn run<'a>(&'a self, world: &'a World) -> BoxFuture<'a, Result<Self::Ok, Self::Err>>;

    /// The operation passes the lock on (restart) instead of releasing it.
    fn lock_handed_on(&self) -> bool {
        false
    }

    /// The error when `fault` is injected into `call` and the cleanup succeeds.
    fn expect(&self, call: &Call, fault: &Fault) -> Self::Err;

    /// The error when `cause` is injected into `call` and the cleanup call `cleanup_call` fails
    /// with `cleanup`.
    fn expect_with_cleanup(
        &self,
        call: &Call,
        cause: &Fault,
        cleanup_call: &Call,
        cleanup: &Fault,
    ) -> Self::Err;

    /// Whether a call made after a failure is cleanup (the cleanup sweep fails each such call).
    fn is_cleanup(&self, _call: &Call) -> bool {
        true
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Fault,
    Cleanup,
    Cancellation,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Problem {
    /// The undisturbed run did not succeed.
    BaselineFailed(String),
    /// The result is not the expected one.
    Unexpected { expected: String, got: String },
    /// The release invariant is broken.
    Leaked(Vec<Leak>),
    /// A call happened after the future was dropped.
    RanAfterDrop(Vec<Call>),
    /// The calls before the drop are not a prefix of the undisturbed run's.
    OffScript { calls: Vec<Call> },
    /// The operation finished or never settled when it should not have.
    Stuck,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Failure {
    pub kind: Kind,
    /// What was injected, or after how many polls the future was dropped.
    pub case: String,
    pub problem: Problem,
}

/// How many runs each sweep made.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub struct Report {
    pub faults: usize,
    pub cleanups: usize,
    pub cancellations: usize,
}

/// Polls to completion, advancing the paused clock whenever it is pending. Gives up after a
/// bound, as an operation that waits forever would never end.
async fn drive<T>(future: BoxFuture<'_, T>) -> Option<T> {
    const MAX_POLLS: usize = 10_000;
    let mut future = pin!(future);
    let mut cx = Context::from_waker(Waker::noop());
    for _ in 0..MAX_POLLS {
        if let Poll::Ready(out) = future.as_mut().poll(&mut cx) {
            return Some(out);
        }
        tokio::time::advance(Duration::from_secs(1)).await;
    }
    None
}

/// Lets detached tasks and timers run: whatever the dropped future left behind would act now.
async fn settle() {
    for _ in 0..8 {
        tokio::time::advance(Duration::from_secs(3600)).await;
        tokio::task::yield_now().await;
    }
}

fn render<T: Debug>(value: &T) -> String {
    format!("{value:?}")
}

/// The calls made by the undisturbed run, or why it did not succeed.
async fn baseline<S: Subject>(subject: &S) -> Result<Vec<Call>, Failure> {
    let world = subject.world();
    let result = drive(subject.run(&world)).await;
    match result {
        Some(Ok(_)) => Ok(world.script.calls()),
        Some(Err(error)) => Err(baseline_failure(render(&error))),
        None => Err(baseline_failure("it never finished".to_owned())),
    }
}

fn baseline_failure(what: String) -> Failure {
    Failure {
        kind: Kind::Fault,
        case: "no fault".to_owned(),
        problem: Problem::BaselineFailed(what),
    }
}

/// One run with `faults` scripted: the world after it and whether it matched `expected`.
async fn faulted<S: Subject>(
    subject: &S,
    faults: &[(Call, &Fault)],
    expected: &S::Err,
    kind: Kind,
    case: &str,
) -> (World, Option<Failure>) {
    let world = subject.world();
    for (call, fault) in faults {
        world.script.fail(*call, (*fault).clone());
    }
    let result = drive(subject.run(&world)).await;
    let failure = |problem| Failure {
        kind,
        case: case.to_owned(),
        problem,
    };
    let problem = match result {
        Some(Err(ref error)) if error == expected => None,
        Some(ref other) => Some(Problem::Unexpected {
            expected: render(expected),
            got: render(other),
        }),
        None => Some(Problem::Stuck),
    };
    drop(result);
    let problem = problem.or_else(|| {
        let leaks = world.leaks(subject.lock_handed_on());
        (!leaks.is_empty()).then_some(Problem::Leaked(leaks))
    });
    (world, problem.map(failure))
}

/// Fails each call of the undisturbed run with each error it can return.
///
/// # Errors
///
/// Every case that did not end with the expected error and the release invariant.
pub async fn fault_injection<S: Subject>(subject: &S) -> Result<usize, Vec<Failure>> {
    let calls = baseline(subject).await.map_err(|f| vec![f])?;
    let mut failures = Vec::new();
    let mut runs: usize = 0;
    for call in calls {
        for fault in call.op.faults() {
            let case = format!("{call:?} fails with {fault:?}");
            let expected = subject.expect(&call, &fault);
            let (_, failure) =
                faulted(subject, &[(call, &fault)], &expected, Kind::Fault, &case).await;
            runs = runs.saturating_add(1);
            failures.extend(failure);
        }
    }
    if failures.is_empty() {
        Ok(runs)
    } else {
        Err(failures)
    }
}

/// Fails each call of the undisturbed run, then each cleanup call that follows the failure.
///
/// # Errors
///
/// Every case that did not end with the expected error (carrying both) and the release invariant.
pub async fn cleanup_failure<S: Subject>(subject: &S) -> Result<usize, Vec<Failure>> {
    let calls = baseline(subject).await.map_err(|f| vec![f])?;
    let mut failures = Vec::new();
    let mut runs: usize = 0;
    for call in calls {
        for cause in call.op.faults() {
            let expected = subject.expect(&call, &cause);
            let case = format!("{call:?} fails with {cause:?}");
            let (world, failure) =
                faulted(subject, &[(call, &cause)], &expected, Kind::Fault, &case).await;
            if failure.is_some() {
                continue; // the fault sweep reports it
            }
            let log = world.script.calls();
            let after = log
                .iter()
                .skip_while(|seen| **seen != call)
                .skip(1)
                .filter(|seen| subject.is_cleanup(seen));
            for cleanup_call in after {
                for cleanup in cleanup_call.op.faults() {
                    let expected =
                        subject.expect_with_cleanup(&call, &cause, cleanup_call, &cleanup);
                    let case = format!(
                        "{call:?} fails with {cause:?}, then {cleanup_call:?} fails with {cleanup:?}"
                    );
                    let injected = [(call, &cause), (*cleanup_call, &cleanup)];
                    let (_, failure) =
                        faulted(subject, &injected, &expected, Kind::Cleanup, &case).await;
                    runs = runs.saturating_add(1);
                    failures.extend(failure);
                }
            }
        }
    }
    if failures.is_empty() {
        Ok(runs)
    } else {
        Err(failures)
    }
}

/// Drops the operation after `m` polls, for every `m` before it completes.
///
/// # Errors
///
/// Every case that left something behind or ran a call after the drop.
pub async fn cancellation<S: Subject>(subject: &S) -> Result<usize, Vec<Failure>> {
    let whole = baseline(subject).await.map_err(|f| vec![f])?;
    let mut failures = Vec::new();
    let mut runs: usize = 0;
    for polls in 0.. {
        let world = subject.world();
        let mut dropped = false;
        {
            let mut future = pin!(subject.run(&world));
            let mut cx = Context::from_waker(Waker::noop());
            for index in 0..polls {
                if index > 0 {
                    tokio::time::advance(Duration::from_secs(1)).await;
                }
                if future.as_mut().poll(&mut cx).is_ready() {
                    dropped = true; // completed within `polls` polls: no case is left
                    break;
                }
            }
        }
        if dropped {
            break;
        }
        let at_drop = world.script.calls();
        settle().await;
        let after = world.script.calls();
        let case = format!("dropped after {polls} polls");
        let failure = |problem| Failure {
            kind: Kind::Cancellation,
            case: case.clone(),
            problem,
        };
        runs = runs.saturating_add(1);
        if after.len() > at_drop.len() {
            failures.push(failure(Problem::RanAfterDrop(
                after.iter().skip(at_drop.len()).copied().collect(),
            )));
        } else if !whole.starts_with(&at_drop) {
            failures.push(failure(Problem::OffScript { calls: at_drop }));
        }
        let leaks = world.leaks(subject.lock_handed_on());
        if !leaks.is_empty() {
            failures.push(failure(Problem::Leaked(leaks)));
        }
    }
    if failures.is_empty() {
        Ok(runs)
    } else {
        Err(failures)
    }
}

/// All three sweeps.
///
/// # Errors
///
/// The failures of every sweep, in the order fault, cleanup, cancellation.
pub async fn sweeps<S: Subject>(subject: &S) -> Result<Report, Vec<Failure>> {
    let mut failures = Vec::new();
    let mut report = Report::default();
    match fault_injection(subject).await {
        Ok(runs) => report.faults = runs,
        Err(found) => failures.extend(found),
    }
    match cleanup_failure(subject).await {
        Ok(runs) => report.cleanups = runs,
        Err(found) => failures.extend(found),
    }
    match cancellation(subject).await {
        Ok(runs) => report.cancellations = runs,
        Err(found) => failures.extend(found),
    }
    if failures.is_empty() {
        Ok(report)
    } else {
        Err(failures)
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::domain::{AbsDir, AbsPath, FileMode, UserName};
    use crate::plan::loginctl_argv;
    use crate::ports::host_fs::{FsError, HostFs as _};
    use crate::ports::procs::{
        ChildIo, LockHandoff, ProcError, Procs as _, Signal, SpawnError, Spawned as _,
    };
    use crate::ports::vm_lock::{LockError, VmLock as _};
    use crate::testing::fakes::{FakeChild, FakeHeld, OpId};
    use crate::testing::{self};

    /// How the toy operation misbehaves, if it does.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    enum Bug {
        None,
        /// Forgets the child when the write fails, instead of stopping it.
        ForgetsChildOnFailure,
        /// Forgets the child after the write, then waits once more: cancelling there leaks it.
        ForgetsChildThenWaits,
        /// Starts a detached task that makes a call whenever it is first polled.
        DetachedCall,
        /// Reports the wrong error for a failed lock.
        WrongError,
    }

    #[derive(Clone, PartialEq, Eq, Debug)]
    enum ToyError {
        Lock(LockError),
        Spawn(SpawnError),
        Write(FsError),
        /// The operation failed, and so did stopping what it had started.
        Failed {
            cause: Box<ToyError>,
            cleanup: Vec<ProcError>,
        },
    }

    #[allow(
        dead_code,
        reason = "a started toy owns the lock and the child until dropped"
    )]
    struct Started {
        lock: FakeHeld,
        child: Option<FakeChild>,
    }

    impl std::fmt::Debug for Started {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("Started")
        }
    }

    struct Toy(Bug);

    fn path(text: &str) -> AbsPath {
        AbsPath::parse(text.to_owned()).unwrap()
    }

    impl Toy {
        /// Two steps: take the lock and start a child that holds it, then write the run record;
        /// when the record can't be written, stop the child.
        async fn run_in(&self, world: &World) -> Result<Started, ToyError> {
            let lock = world
                .lock
                .acquire(&path("/data/proj/lock"))
                .await
                .map_err(ToyError::Lock)?;
            let user = UserName::parse("agent".to_owned()).unwrap();
            let child = world
                .procs
                .spawn(
                    &loginctl_argv(&user),
                    &AbsDir::root(),
                    LockHandoff::Stdin(&lock),
                    ChildIo::Null,
                )
                .map_err(ToyError::Spawn)?;
            if self.0 == Bug::DetachedCall {
                let detached = world.clone();
                tokio::spawn(async move {
                    let _ = detached.fs.observe(&path("/data/proj")).await;
                });
            }
            let record = world
                .fs
                .write_file(
                    &path("/data/proj/run"),
                    b"run".to_vec(),
                    FileMode::parse(0o644).unwrap(),
                )
                .await;
            let Err(cause) = record else {
                if self.0 == Bug::ForgetsChildThenWaits {
                    std::mem::forget(child);
                    let _ = world.fs.observe(&path("/data/proj")).await;
                    return Ok(Started { lock, child: None });
                }
                return Ok(Started {
                    lock,
                    child: Some(child),
                });
            };
            if self.0 == Bug::ForgetsChildOnFailure {
                std::mem::forget(child);
                return Err(ToyError::Write(cause));
            }
            match world
                .procs
                .signal(child.identity(), Signal::Terminate)
                .await
            {
                Ok(_) => Err(ToyError::Write(cause)),
                Err(error) => Err(ToyError::Failed {
                    cause: Box::new(ToyError::Write(cause)),
                    cleanup: vec![error],
                }),
            }
        }
    }

    fn testing_fs_error() -> FsError {
        FsError::Aborted {
            op: crate::ports::host_fs::FsOp::Write,
            path: path("/data/proj/run"),
        }
    }

    impl Subject for Toy {
        type Ok = Started;
        type Err = ToyError;

        fn world(&self) -> World {
            let world = World::new();
            world
                .fs
                .add_dir(&crate::domain::AbsDir::parse("/data/proj".to_owned()).unwrap());
            world
        }

        fn run<'a>(&'a self, world: &'a World) -> BoxFuture<'a, Result<Started, ToyError>> {
            Box::pin(self.run_in(world))
        }

        fn expect(&self, call: &Call, fault: &Fault) -> ToyError {
            match fault {
                Fault::Lock(error) if self.0 == Bug::WrongError => {
                    ToyError::Spawn(SpawnError::Lock(error.clone()))
                }
                // The handoff of the lock to the child is part of the spawn step.
                Fault::Lock(error) if call.op == OpId::LockStdin => {
                    ToyError::Spawn(SpawnError::Lock(error.clone()))
                }
                Fault::Lock(error) => ToyError::Lock(error.clone()),
                Fault::Spawn(error) => ToyError::Spawn(error.clone()),
                Fault::Fs(error) => ToyError::Write(error.clone()),
                Fault::Guest(_)
                | Fault::Proc(_)
                | Fault::Nix(_)
                | Fault::System(_)
                | Fault::Key(_)
                | Fault::FreePort(_)
                | Fault::Banner(_)
                | Fault::Qmp(_)
                | Fault::Session(_) => ToyError::Write(testing_fs_error()),
            }
        }

        fn expect_with_cleanup(
            &self,
            call: &Call,
            cause: &Fault,
            _cleanup_call: &Call,
            cleanup: &Fault,
        ) -> ToyError {
            ToyError::Failed {
                cause: Box::new(self.expect(call, cause)),
                cleanup: match cleanup {
                    Fault::Proc(error) => vec![error.clone()],
                    Fault::Guest(_)
                    | Fault::Lock(_)
                    | Fault::Fs(_)
                    | Fault::Spawn(_)
                    | Fault::Nix(_)
                    | Fault::System(_)
                    | Fault::Key(_)
                    | Fault::FreePort(_)
                    | Fault::Banner(_)
                    | Fault::Qmp(_)
                    | Fault::Session(_) => vec![],
                },
            }
        }
    }

    fn run_sweeps(bug: Bug) -> Result<Report, Vec<Failure>> {
        paused_runtime().unwrap().block_on(sweeps(&Toy(bug)))
    }

    fn lock_fault_count() -> usize {
        OpId::LockAcquire.faults().len()
    }

    #[test]
    fn the_three_sweeps_pass_over_a_toy_two_step_operation() {
        let _one = testing::one_spawn_at_a_time();
        let report = run_sweeps(Bug::None).unwrap();
        // Calls of the undisturbed run: lock.acquire, lock.stdin, procs.spawn, fs.write_file.
        let expected = Report {
            faults: lock_fault_count()
                + OpId::LockStdin.faults().len()
                + OpId::ProcsSpawn.faults().len()
                + OpId::FsWriteFile.faults().len(),
            // Only a failed write is followed by a cleanup call (procs.signal).
            cleanups: OpId::FsWriteFile.faults().len() * OpId::ProcsSignal.faults().len(),
            // Dropped before the first poll, after the lock's pause and after the write's.
            cancellations: 3,
        };
        assert_eq!(report, expected);
    }

    #[test]
    fn a_stdin_failure_of_the_lock_handoff_unwinds_the_lock() {
        let _one = testing::one_spawn_at_a_time();
        let failures = paused_runtime()
            .unwrap()
            .block_on(fault_injection(&Toy(Bug::None)));
        assert!(failures.is_ok(), "{failures:?}");
    }

    #[test]
    fn forgetting_a_child_after_a_failed_write_is_reported_as_a_leak() {
        let _one = testing::one_spawn_at_a_time();
        let failures = run_sweeps(Bug::ForgetsChildOnFailure).unwrap_err();
        // The write fails with each of its two errors; the forgotten child keeps the lock too.
        assert_eq!(failures.len(), 2, "{failures:?}");
        for failure in &failures {
            assert_eq!(failure.kind, Kind::Fault);
            assert!(
                matches!(&failure.problem, Problem::Leaked(leaks)
                    if matches!(leaks.as_slice(), [Leak::Process(_), Leak::Lock { holders: 1 }])),
                "{failure:?}"
            );
        }
    }

    #[test]
    fn a_child_forgotten_before_a_last_wait_leaks_when_the_caller_is_dropped_there() {
        let _one = testing::one_spawn_at_a_time();
        let failures = cancellation_only(Bug::ForgetsChildThenWaits).unwrap_err();
        assert!(
            failures
                .iter()
                .all(|f| f.kind == Kind::Cancellation && matches!(f.problem, Problem::Leaked(_)))
        );
    }

    fn cancellation_only(bug: Bug) -> Result<usize, Vec<Failure>> {
        paused_runtime().unwrap().block_on(cancellation(&Toy(bug)))
    }

    #[test]
    fn a_detached_task_that_calls_after_the_drop_is_reported() {
        let _one = testing::one_spawn_at_a_time();
        let failures = cancellation_only(Bug::DetachedCall).unwrap_err();
        assert!(
            failures
                .iter()
                .any(|f| matches!(f.problem, Problem::RanAfterDrop(_))),
            "{failures:?}"
        );
    }

    #[test]
    fn a_wrong_error_is_reported_with_expected_and_got() {
        let _one = testing::one_spawn_at_a_time();
        let failures = run_sweeps(Bug::WrongError).unwrap_err();
        assert!(
            failures
                .iter()
                .all(|f| matches!(f.problem, Problem::Unexpected { .. }))
        );
        assert_eq!(
            failures.iter().filter(|f| f.kind == Kind::Fault).count(),
            lock_fault_count()
        );
    }

    #[test]
    fn the_release_invariant_names_a_live_child_and_a_held_lock() {
        let _one = testing::one_spawn_at_a_time();
        let world = World::new();
        let rt = paused_runtime().unwrap();
        rt.block_on(async {
            let lock = world.lock.acquire(&path("/data/proj/lock")).await.unwrap();
            let user = UserName::parse("agent".to_owned()).unwrap();
            let child = world
                .procs
                .spawn(
                    &loginctl_argv(&user),
                    &AbsDir::root(),
                    LockHandoff::Stdin(&lock),
                    ChildIo::Null,
                )
                .unwrap();
            let id = child.identity().clone();
            assert_eq!(
                world.leaks(false),
                [Leak::Process(id.clone()), Leak::Lock { holders: 2 }]
            );
            assert_eq!(world.leaks(true), [Leak::Process(id)]);
            drop(child);
            assert_eq!(world.leaks(false), [Leak::Lock { holders: 1 }]);
            drop(lock);
            assert_eq!(world.leaks(false), []);
        });
    }

    #[test]
    fn a_scripted_failure_of_the_n_th_call_hits_only_that_call() {
        let world = World::new();
        world.script.fail(
            Call {
                op: OpId::LockAcquire,
                n: 1,
            },
            Fault::Lock(LockError::Aborted {
                path: path("/data/proj/lock"),
            }),
        );
        let rt = paused_runtime().unwrap();
        rt.block_on(async {
            let first = world.lock.acquire(&path("/data/proj/lock")).await;
            assert!(first.is_ok());
            drop(first);
            let second = world.lock.acquire(&path("/data/proj/lock")).await;
            assert_eq!(
                second.err(),
                Some(LockError::Aborted {
                    path: path("/data/proj/lock")
                })
            );
            assert!(world.lock.acquire(&path("/data/proj/lock")).await.is_ok());
        });
    }

    #[test]
    fn a_second_acquire_of_a_held_lock_is_refused_and_a_dropped_one_is_free() {
        let world = World::new();
        let rt = paused_runtime().unwrap();
        rt.block_on(async {
            let held = world.lock.acquire(&path("/l")).await.unwrap();
            assert_eq!(
                world.lock.acquire(&path("/l")).await.err(),
                Some(LockError::Held { path: path("/l") })
            );
            drop(held);
            assert!(world.lock.acquire(&path("/l")).await.is_ok());
        });
    }
}
