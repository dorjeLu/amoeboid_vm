//! Fakes of `VmLock` and `Procs`. The lock has a holder count: the handles `acquire` hands out
//! and the processes that were given the lock when spawned; it is free at zero, as the kernel's
//! lock is when no process of the VM is alive.

#![allow(clippy::disallowed_types, reason = "boundary: fixtures name raw files")]

use std::collections::BTreeMap;
use std::fs::File;
use std::future::Future;
use std::io;
use std::os::fd::OwnedFd;
use std::process::Stdio;
use std::sync::{Arc, Mutex};

use super::{Fault, OpId, Script, held, pause, step};
use crate::domain::{AbsDir, AbsPath, Pid, ProcIdentity, ProgramName, StartTime};
use crate::plan::Argv;
use crate::ports::Cause;
use crate::ports::procs::{
    ChildIo, Exit, LockHandoff, Presence, ProcError, Procs, Signal, Signalled, SpawnError, Spawned,
};
use crate::ports::vm_lock::{Availability, HeldLock, Inheritable, LockError, LockOp, VmLock};

struct LockState {
    holders: usize,
}

/// The VM lock as a count of holders.
#[derive(Clone)]
pub struct FakeLock {
    script: Script,
    state: Arc<Mutex<LockState>>,
}

impl FakeLock {
    pub(crate) fn new(script: &Script) -> Self {
        Self {
            script: script.clone(),
            state: Arc::new(Mutex::new(LockState { holders: 0 })),
        }
    }

    /// Handles and processes holding the lock now.
    #[must_use]
    pub fn holders(&self) -> usize {
        held(&self.state).holders
    }

    fn release(&self) {
        let mut state = held(&self.state);
        state.holders = state.holders.saturating_sub(1);
    }
}

/// A held lock: one holder until dropped.
pub struct FakeHeld {
    lock: FakeLock,
    path: AbsPath,
}

impl Drop for FakeHeld {
    fn drop(&mut self) {
        self.lock.release();
    }
}

impl HeldLock for FakeHeld {
    fn stdin(&self) -> Result<Stdio, LockError> {
        match self
            .lock
            .script
            .enter(OpId::LockStdin)
            .and_then(Fault::lock)
        {
            Some(error) => Err(error),
            None => Ok(Stdio::null()),
        }
    }

    fn inheritable(&self) -> Result<Inheritable, LockError> {
        if let Some(error) = self
            .lock
            .script
            .enter(OpId::LockInheritable)
            .and_then(Fault::lock)
        {
            return Err(error);
        }
        // Any descriptor will do: nothing is exec'd.
        File::open("/dev/null")
            .map(|file| Inheritable::new(OwnedFd::from(file)))
            .map_err(|error| LockError::Io {
                op: LockOp::Duplicate,
                path: self.path.clone(),
                cause: Cause::of(&error),
            })
    }
}

impl VmLock for FakeLock {
    type Held = FakeHeld;

    fn acquire(&self, path: &AbsPath) -> impl Future<Output = Result<FakeHeld, LockError>> + Send {
        let path = path.clone();
        step(&self.script, OpId::LockAcquire, Fault::lock, move || {
            let mut state = held(&self.state);
            if state.holders > 0 {
                return Err(LockError::Held { path });
            }
            state.holders = 1;
            Ok(FakeHeld {
                lock: self.clone(),
                path,
            })
        })
    }

    fn probe(
        &self,
        _path: &AbsPath,
    ) -> impl Future<Output = Result<Availability, LockError>> + Send {
        step(&self.script, OpId::LockProbe, Fault::lock, move || {
            Ok(if held(&self.state).holders > 0 {
                Availability::Held
            } else {
                Availability::Free
            })
        })
    }
}

struct Process {
    identity: ProcIdentity,
    alive: bool,
    holds_lock: bool,
    /// Started by the run under test (not part of the scenario's setup).
    from_run: bool,
    exit: Option<Exit>,
    cwd: AbsDir,
    group: Group,
}

/// Which process group a process was started in (see `ChildIo`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Group {
    /// The launcher's, and so the terminal's.
    Terminal,
    Own,
}

/// How a process was started.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Started {
    pub identity: ProcIdentity,
    pub cwd: AbsDir,
    pub group: Group,
}

/// What the runner script becomes once it has exec'd qemu.
const RUNNER: &str = "microvm-run";
const QEMU: &str = "qemu-system-x86_64";

#[derive(Default)]
struct ProcsState {
    next: u32,
    procs: BTreeMap<u32, Process>,
    /// How a child ends when something waits for it while it is alive; none = it never ends.
    ends_with: Option<Exit>,
}

/// Processes with identities. A child ends by a signal (SIGTERM and SIGKILL both kill it), by
/// [`FakeProcs::end`], or, when something waits, as [`FakeProcs::end_waits_with`] says.
#[derive(Clone)]
pub struct FakeProcs {
    script: Script,
    lock: FakeLock,
    state: Arc<Mutex<ProcsState>>,
}

impl FakeProcs {
    pub(crate) fn new(script: &Script, lock: &FakeLock) -> Self {
        Self {
            script: script.clone(),
            lock: lock.clone(),
            state: Arc::default(),
        }
    }

    /// Makes `wait` on a live child end with `exit` (default: it waits forever).
    pub fn end_waits_with(&self, exit: Exit) {
        held(&self.state).ends_with = Some(exit);
    }

    /// Ends the process now, as if it exited by itself.
    pub fn end(&self, pid: Pid, exit: Exit) {
        self.kill(pid.get().to_owned(), exit);
    }

    /// Identities of the processes started by the run that are alive.
    #[must_use]
    pub fn alive_from_run(&self) -> Vec<ProcIdentity> {
        held(&self.state)
            .procs
            .values()
            .filter(|p| p.alive && p.from_run)
            .map(|p| p.identity.clone())
            .collect()
    }

    /// Identities of every process ever started by the run, in start order.
    #[must_use]
    pub fn started(&self) -> Vec<ProcIdentity> {
        held(&self.state)
            .procs
            .values()
            .filter(|p| p.from_run)
            .map(|p| p.identity.clone())
            .collect()
    }

    /// How every process of the run was started (working directory, own process group), in order.
    #[must_use]
    pub fn started_with(&self) -> Vec<Started> {
        held(&self.state)
            .procs
            .values()
            .filter(|p| p.from_run)
            .map(|p| Started {
                identity: p.identity.clone(),
                cwd: p.cwd.clone(),
                group: p.group,
            })
            .collect()
    }

    fn kill(&self, pid: u32, exit: Exit) {
        let mut state = held(&self.state);
        let Some(process) = state.procs.get_mut(&pid) else {
            return;
        };
        if !process.alive {
            return;
        }
        process.alive = false;
        process.exit = Some(exit);
        let holds = process.holds_lock;
        drop(state);
        if holds {
            self.lock.release();
        }
    }
}

/// A child of the fake: SIGTERM to its identity when dropped, unless it was waited for.
pub struct FakeChild {
    procs: FakeProcs,
    identity: ProcIdentity,
    reaped: bool,
}

impl Drop for FakeChild {
    fn drop(&mut self) {
        if !self.reaped {
            self.procs.kill(*self.identity.pid.get(), Exit::Signal(15));
        }
    }
}

impl Spawned for FakeChild {
    fn identity(&self) -> &ProcIdentity {
        &self.identity
    }

    async fn settle(&mut self) -> Result<ProcIdentity, ProcError> {
        let pid = self.identity.pid;
        let scripted = self
            .procs
            .script
            .enter(OpId::ProcsSettle)
            .and_then(Fault::proc);
        pause().await;
        if let Some(error) = scripted {
            return Err(error);
        }
        let mut state = held(&self.procs.state);
        let process = state
            .procs
            .get_mut(pid.get())
            .filter(|p| p.alive)
            .ok_or(ProcError::NoSuchProcess { pid })?;
        // The runner script execs qemu; anything else keeps its program.
        if let (RUNNER, Ok(program)) = (
            process.identity.program.get().as_str(),
            ProgramName::parse(QEMU.to_owned()),
        ) {
            process.identity.program = program;
        }
        self.identity = process.identity.clone();
        Ok(self.identity.clone())
    }

    fn release(mut self) -> ProcIdentity {
        self.reaped = true;
        self.identity.clone()
    }

    async fn wait(&mut self) -> Result<Exit, ProcError> {
        let pid = *self.identity.pid.get();
        let procs = self.procs.clone();
        let scripted = procs.script.enter(OpId::ProcsWait).and_then(Fault::proc);
        pause().await;
        if let Some(error) = scripted {
            return Err(error);
        }
        loop {
            let (exit, ends_with) = {
                let state = held(&procs.state);
                (state.procs.get(&pid).and_then(|p| p.exit), state.ends_with)
            };
            if let Some(exit) = exit {
                self.reaped = true;
                return Ok(exit);
            }
            match ends_with {
                Some(exit) => procs.kill(pid, exit),
                None => std::future::pending::<()>().await,
            }
        }
    }
}

fn pid_of(raw: u32) -> Result<Pid, SpawnError> {
    Pid::parse(raw).map_err(|_| SpawnError::Exec {
        cause: Cause {
            kind: io::ErrorKind::InvalidData,
            errno: None,
        },
    })
}

impl Procs for FakeProcs {
    type Child = FakeChild;

    fn spawn<H: HeldLock>(
        &self,
        argv: &Argv,
        cwd: &AbsDir,
        lock: LockHandoff<'_, H>,
        io: ChildIo,
    ) -> Result<FakeChild, SpawnError> {
        if let Some(error) = self.script.enter(OpId::ProcsSpawn).and_then(Fault::spawn) {
            return Err(error);
        }
        let holds_lock = match lock {
            LockHandoff::Stdin(held_lock) => {
                held_lock.stdin().map_err(SpawnError::Lock)?;
                true
            }
            LockHandoff::Inherited(held_lock) => {
                // Open across the spawn, closed right after.
                drop(held_lock.inheritable().map_err(SpawnError::Lock)?);
                true
            }
            LockHandoff::None => false,
        };
        let group = match io {
            ChildIo::Inherit => Group::Terminal,
            ChildIo::Null | ChildIo::Log(_) => Group::Own,
        };
        let name = argv.program().rsplit('/').next().unwrap_or_default();
        let program = ProgramName::parse(name.to_owned()).map_err(|_| SpawnError::Exec {
            cause: Cause {
                kind: io::ErrorKind::InvalidInput,
                errno: None,
            },
        })?;
        let mut state = held(&self.state);
        state.next = state.next.saturating_add(1);
        let raw = state.next.saturating_add(1000);
        let identity = ProcIdentity {
            pid: pid_of(raw)?,
            start: StartTime::parse(u64::from(state.next)).map_err(|_| SpawnError::Exec {
                cause: Cause {
                    kind: io::ErrorKind::InvalidData,
                    errno: None,
                },
            })?,
            program,
        };
        state.procs.insert(
            raw,
            Process {
                identity: identity.clone(),
                alive: true,
                holds_lock,
                from_run: true,
                exit: None,
                cwd: cwd.clone(),
                group,
            },
        );
        drop(state);
        if holds_lock {
            {
                let mut lock = held(&self.lock.state);
                lock.holders = lock.holders.saturating_add(1);
            }
        }
        Ok(FakeChild {
            procs: self.clone(),
            identity,
            reaped: false,
        })
    }

    fn identify(&self, pid: Pid) -> impl Future<Output = Result<ProcIdentity, ProcError>> + Send {
        step(&self.script, OpId::ProcsIdentify, Fault::proc, move || {
            held(&self.state)
                .procs
                .get(pid.get())
                .filter(|p| p.alive)
                .map(|p| p.identity.clone())
                .ok_or(ProcError::NoSuchProcess { pid })
        })
    }

    fn presence(
        &self,
        who: &ProcIdentity,
    ) -> impl Future<Output = Result<Presence, ProcError>> + Send {
        let who = who.clone();
        step(&self.script, OpId::ProcsPresence, Fault::proc, move || {
            Ok(match held(&self.state).procs.get(who.pid.get()) {
                Some(p) if p.identity != who => Presence::Replaced,
                Some(p) if p.alive => Presence::Alive,
                Some(_) | None => Presence::Gone,
            })
        })
    }

    fn signal(
        &self,
        who: &ProcIdentity,
        signal: Signal,
    ) -> impl Future<Output = Result<Signalled, ProcError>> + Send {
        let who = who.clone();
        step(&self.script, OpId::ProcsSignal, Fault::proc, move || {
            let seen = held(&self.state)
                .procs
                .get(who.pid.get())
                .map(|p| (p.identity == who, p.alive));
            Ok(match seen {
                Some((false, _)) => Signalled::Replaced,
                Some((true, true)) => {
                    let number = match signal {
                        Signal::Terminate => 15,
                        Signal::Kill => 9,
                    };
                    self.kill(*who.pid.get(), Exit::Signal(number));
                    Signalled::Sent
                }
                Some((true, false)) | None => Signalled::Gone,
            })
        })
    }
}
