"""Tracking and reaping the processes behind a VM.

Every process the launcher starts gets a `<role>.pid` file in the VM's runtime dir
(a tmpfs). A pid only counts as ours while /proc says it is still the program we
started, so recycled pids are never signalled.
"""

import contextlib
import logging
import os
import signal
import time
from collections.abc import Iterable
from dataclasses import dataclass
from pathlib import Path

from amoeboid.errors import AppError, fail
from amoeboid.paths import VmPaths
from amoeboid.result import Ok, Result

log = logging.getLogger(__name__)

QEMU = "qemu"


@dataclass(frozen=True, slots=True)
class Tracked:
    role: str
    pid: int

    @property
    def program(self) -> str:
        """What the process's executable name must contain: qemu, or virtiofsd."""
        return QEMU if self.role == QEMU else "virtiofsd"


def record(vm: VmPaths, role: str, pid: int) -> None:
    vm.pid_file(role).write_text(f"{pid}\n")


def forget(vm: VmPaths, role: str) -> None:
    vm.pid_file(role).unlink(missing_ok=True)


def tracked(vm: VmPaths) -> list[Tracked]:
    out: list[Tracked] = []
    for file in sorted(vm.runtime.glob("*.pid")) if vm.runtime.is_dir() else []:
        try:
            out.append(Tracked(file.stem, int(file.read_text().strip())))
        except OSError, ValueError:
            file.unlink(missing_ok=True)
    return out


def alive(proc: Tracked) -> bool:
    # The executable, not argv[0]: microvm's runner renames qemu to microvm@<host>.
    try:
        exe = Path(f"/proc/{proc.pid}/exe").readlink().name
    except OSError:
        return False
    return proc.program in exe


def running_qemu(vm: VmPaths) -> Tracked | None:
    return next((p for p in tracked(vm) if p.role == QEMU and alive(p)), None)


def terminate(procs: Iterable[Tracked], grace: float = 5.0) -> None:
    """SIGTERM, then SIGKILL whatever is still alive after `grace` seconds."""
    pending = [p for p in procs if alive(p)]
    for p in pending:
        _signal(p.pid, signal.SIGTERM)
    deadline = time.monotonic() + grace
    while pending and time.monotonic() < deadline:
        time.sleep(0.1)
        pending = [p for p in pending if alive(p)]
    for p in pending:
        _signal(p.pid, signal.SIGKILL)


def wait_gone(proc: Tracked, timeout: float) -> bool:
    deadline = time.monotonic() + timeout
    while alive(proc):
        if time.monotonic() >= deadline:
            return False
        time.sleep(0.2)
    return True


def sweep(vm: VmPaths) -> Result[None, AppError]:
    """Clear the runtime dir for a new start.

    Refuses if the VM's qemu is still running; kills (with a warning) helpers left
    behind by an earlier run that didn't clean up, then removes stale runtime files.
    """
    if (qemu := running_qemu(vm)) is not None:
        return fail(
            f"VM '{vm.name}' is already running (qemu pid {qemu.pid})",
            f"connect with `amoeboid ssh --name {vm.name}` or stop it with "
            f"`amoeboid stop --name {vm.name}`",
        )
    leftovers = [p for p in tracked(vm) if alive(p)]
    for p in leftovers:
        log.warning(
            "killing leftover %s (pid %d) from an earlier run of '%s'", p.role, p.pid, vm.name
        )
    terminate(leftovers)
    clear_runtime(vm)
    return Ok(None)


def clear_runtime(vm: VmPaths) -> None:
    if not vm.runtime.is_dir():
        return
    for entry in vm.runtime.iterdir():
        if entry.is_file() or entry.is_socket() or entry.is_symlink():
            entry.unlink(missing_ok=True)


def _signal(pid: int, sig: signal.Signals) -> None:
    with contextlib.suppress(ProcessLookupError):
        os.kill(pid, sig)
