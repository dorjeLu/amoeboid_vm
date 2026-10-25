"""Starting, stopping and inspecting VMs: the effectful shell around the pure parts.

Resources are bracketed with an ExitStack: each helper process is registered for
cleanup the moment it starts, so any failure (or Ctrl-C) unwinds whatever was started.
A detached start hands its processes over to the VM by dropping the stack instead.
"""

import logging
import os
import shutil
import subprocess
import sys
import time
from collections.abc import Callable, Sequence
from contextlib import ExitStack
from dataclasses import dataclass
from pathlib import Path
from typing import IO

from amoeboid import guestfs, plan, procs, qmp, recipe, ssh
from amoeboid.config import RunConfig
from amoeboid.errors import AppError, because, fail
from amoeboid.mounts import Mode, Mount
from amoeboid.paths import BUNDLED_NIX, DEFAULT_VM, Paths, VmPaths
from amoeboid.result import Err, Ok, Result, attempt, ensure, railway, traverse

log = logging.getLogger(__name__)


@dataclass(frozen=True, slots=True)
class StartRequest:
    vm: VmPaths
    home: Path
    config: RunConfig
    host_module: Path
    recipe: Path
    mounts: tuple[Mount, ...]
    detach: bool
    boot_timeout: float


# -- start ------------------------------------------------------------------------------


@railway
def start(req: StartRequest) -> Result[int, AppError]:
    vm = req.vm
    preflight().or_bail()
    procs.sweep(vm).or_bail()
    prepare_dirs(vm).or_bail()
    packages = recipe.prepare_packages(vm).or_bail()
    recipe.publish_reference(vm, req.recipe, req.host_module).or_bail()
    build(vm, req.recipe, req.host_module, packages).or_bail()
    ssh.ensure_host_key(vm).or_bail()
    port = (Ok(req.config.ssh.port) if req.config.ssh.port else ssh.free_port()).or_bail()
    write_guest_state(req, port).or_bail()
    write_runtime_state(vm, req.mounts, port).or_bail()
    remember_mounts(vm, req.mounts).or_bail()

    with ExitStack() as cleanup:
        cleanup.callback(procs.clear_runtime, vm)
        start_shares(req, cleanup).or_bail()
        _announce(req, port)
        if not req.detach:
            return run_foreground(vm)
        run_detached(req, port, cleanup).or_bail()
        cleanup.pop_all()  # the VM and its helpers now outlive us
        log.info("'%s' is up: `amoeboid ssh%s` (127.0.0.1:%d)", vm.name, _name_flag(vm), port)
        return Ok(0)
    raise AssertionError("unreachable")  # pragma: no cover - ExitStack never swallows


def preflight() -> Result[None, AppError]:
    checks = [
        ensure(shutil.which("nix") is not None, lambda: AppError("nix not found on PATH")),
        ensure(
            shutil.which("ssh-keygen") is not None,
            lambda: AppError("ssh-keygen not found on PATH", "install OpenSSH"),
        ),
        ensure(
            os.access("/dev/kvm", os.R_OK | os.W_OK),
            lambda: AppError("/dev/kvm isn't accessible", "load kvm and join the kvm group"),
        ),
    ]
    return traverse(checks, lambda r: r).map(lambda _: None)


@railway
def prepare_dirs(vm: VmPaths) -> Result[None, AppError]:
    def mkdirs() -> None:
        vm.guest.mkdir(parents=True, exist_ok=True)
        vm.runtime.mkdir(mode=0o700, parents=True, exist_ok=True)

    attempt(mkdirs, because("can't create the VM's directories")).or_bail()
    return guestfs.ensure_dir(vm.guest, "home")


def build(vm: VmPaths, recipe: Path, host_module: Path, packages: Path) -> Result[None, AppError]:
    log.info("building %s ...", recipe)
    argv = plan.nix_build_argv(BUNDLED_NIX, recipe, host_module, packages, vm.build_link)
    return run_checked(argv, "nix build failed (output above)")


@railway
def write_guest_state(req: StartRequest, port: int) -> Result[None, AppError]:
    """Files the guest reads at boot; always replaced, never written through."""
    vm, g = req.vm, req.vm.guest
    key = attempt(vm.host_key.read_bytes, because("can't read the VM's host key")).or_bail()
    guestfs.replace_file(g, "ssh_host_ed25519_key", key, 0o600).or_bail()
    guestfs.replace_file(g, "mounts", plan.mounts_table(req.mounts).encode()).or_bail()
    guestfs.replace_file(g, "hostname", f"{vm.name}\n".encode()).or_bail()
    match _terminal_size() if not req.detach else None:
        case (rows, cols):
            guestfs.replace_file(g, "term", f"rows {rows} cols {cols}\n".encode()).or_bail()
        case None:
            guestfs.remove_file(g, "term").or_bail()
    for rel in req.config.home_files:
        copy_home_file(req, rel).or_bail()
    log.debug("ssh port %d", port)
    return Ok(None)


@railway
def copy_home_file(req: StartRequest, rel: str) -> Result[None, AppError]:
    source = req.home / rel
    if not source.is_file():
        log.warning("not copying ~/%s into the VM: no such file", rel)
        return Ok(None)
    data = attempt(source.read_bytes, because(f"can't read {source}")).or_bail()
    mode = source.stat().st_mode & 0o777
    return guestfs.replace_file(req.vm.guest, f"home/{rel}", data, mode)


def write_runtime_state(vm: VmPaths, mounts: Sequence[Mount], port: int) -> Result[None, AppError]:
    def write() -> None:
        vm.qemu_args.write_text(plan.qemu_args(mounts, port))
        vm.ssh_port.write_text(f"{port}\n")

    return attempt(write, because(f"can't write to {vm.runtime}"))


@railway
def start_shares(req: StartRequest, cleanup: ExitStack) -> Result[None, AppError]:
    vm = req.vm
    binary = vm.build_link / "virtiofsd" / "bin" / "virtiofsd"
    shares = plan.shares(vm.guest, req.mounts)
    for share in shares:
        argv = plan.virtiofsd_argv(
            binary,
            share,
            vm.runtime,
            guest_uid=req.config.uid,
            guest_gid=req.config.gid,
            host_uid=os.getuid(),
            host_gid=os.getgid(),
        )
        spawn(vm, share.role, argv, cleanup).or_bail()
    sockets = [vm.runtime / s.socket for s in shares]
    return _wait_until(
        lambda: all(s.is_socket() for s in sockets),
        timeout=10,
        error=lambda: AppError(
            "virtiofsd didn't create its sockets: "
            + ", ".join(s.name for s in sockets if not s.is_socket())
        ),
    )


def spawn(
    vm: VmPaths,
    role: str,
    argv: list[str],
    cleanup: ExitStack,
    stdout: IO[bytes] | None = None,
) -> Result[subprocess.Popen[bytes], AppError]:
    """Start a helper in its own session (immune to terminal signals) and track it."""

    def go() -> subprocess.Popen[bytes]:
        proc = subprocess.Popen(
            argv,
            cwd=vm.runtime,
            stdin=subprocess.DEVNULL if stdout is not None else None,
            stdout=stdout,
            stderr=subprocess.STDOUT if stdout is not None else None,
            start_new_session=True,
        )
        procs.record(vm, role, proc.pid)
        # Unwound last-in first-out: terminate, then reap so no zombie lingers.
        cleanup.callback(proc.poll)
        cleanup.callback(procs.terminate, [procs.Tracked(role, proc.pid)])
        return proc

    return attempt(go, because(f"can't start {role}"))


def run_foreground(vm: VmPaths) -> Result[int, AppError]:
    """Boot with the serial console on this terminal until the guest powers off."""

    def go() -> int:
        proc = subprocess.Popen([str(runner(vm))], cwd=vm.runtime)
        procs.record(vm, procs.QEMU, proc.pid)
        try:
            while True:
                try:
                    return proc.wait()
                except KeyboardInterrupt:
                    # qemu owns the terminal in raw mode, so this came from elsewhere;
                    # ask the guest to shut down and keep waiting.
                    log.warning("interrupted: powering the VM off")
                    qmp.powerdown(vm.control_socket)
        finally:
            procs.forget(vm, procs.QEMU)

    return attempt(go, because("can't run the VM"))


@railway
def run_detached(req: StartRequest, port: int, cleanup: ExitStack) -> Result[None, AppError]:
    """Boot in the background, stream the console until sshd answers, then return."""
    vm = req.vm
    console = attempt(
        lambda: vm.console_log.open("wb"), because(f"can't open {vm.console_log}")
    ).or_bail()
    cleanup.callback(console.close)
    qemu = spawn(vm, procs.QEMU, [str(runner(vm))], cleanup, stdout=console).or_bail()
    tail = _Tail(vm.console_log)
    ready = ssh.wait_for_sshd(
        port,
        req.boot_timeout,
        still_booting=lambda: qemu.poll() is None,
        tick=tail.pump,
    )
    tail.pump()
    match ready:
        case Ok():
            return Ok(None)
        case Err(error):
            return Err(AppError(error.message, f"console log: {vm.console_log}"))


def runner(vm: VmPaths) -> Path:
    return vm.build_link / "runner" / "bin" / "microvm-run"


def _announce(req: StartRequest, port: int) -> None:
    vm = req.vm
    for m in req.mounts:
        log.info("  sharing %s (%s)", m.path, m.mode)
    flag = _name_flag(vm)
    log.info("ssh: `amoeboid ssh%s` (127.0.0.1:%d, user %s)", flag, port, req.config.user)
    if not req.detach:
        log.info("console attached; leave with `poweroff` (or `amoeboid stop%s`)", flag)


def _name_flag(vm: VmPaths) -> str:
    return "" if vm.name == DEFAULT_VM else f" --name {vm.name}"


class _Tail:
    """Copies new console output to stderr while waiting for a detached boot.

    The output is the guest's, so terminal control sequences are stripped first.
    """

    def __init__(self, path: Path) -> None:
        self._path = path
        self._offset = 0

    def pump(self) -> None:
        try:
            with self._path.open("rb") as f:
                f.seek(self._offset)
                chunk = f.read()
        except OSError:
            return
        self._offset += len(chunk)
        sys.stderr.buffer.write(plan.printable_console(chunk))
        sys.stderr.flush()


# -- stop -------------------------------------------------------------------------------


@railway
def stop(vm: VmPaths, timeout: float) -> Result[int, AppError]:
    qemu = procs.running_qemu(vm)
    if qemu is None:
        leftovers = [p for p in procs.tracked(vm) if procs.alive(p)]
        procs.terminate(leftovers)
        procs.clear_runtime(vm)
        log.info("VM '%s' isn't running", vm.name)
        return Ok(0)
    log.info("powering off '%s' ...", vm.name)
    match qmp.powerdown(vm.control_socket):
        case Ok():
            if not procs.wait_gone(qemu, timeout):
                log.warning("guest didn't power off within %.0fs; killing qemu", timeout)
        case Err(error):
            log.warning("%s; killing qemu", error.message)
    procs.terminate(procs.tracked(vm))
    procs.clear_runtime(vm)
    log.info("stopped '%s'", vm.name)
    return Ok(0)


# -- inspection -------------------------------------------------------------------------


@dataclass(frozen=True, slots=True)
class Status:
    name: str
    running: bool
    ssh_port: int | None
    mounts: tuple[Mount, ...]


def status(vm: VmPaths) -> Status:
    running = procs.running_qemu(vm) is not None
    port: int | None = None
    if running:
        try:
            port = int(vm.ssh_port.read_text().strip())
        except OSError, ValueError:
            port = None
    return Status(vm.name, running, port, last_mounts(vm))


def statuses(paths: Paths) -> list[Status]:
    if not paths.data_dir.is_dir():
        return []
    names = sorted(d.name for d in paths.data_dir.iterdir() if (d / "guest").is_dir())
    return [status(paths.vm(n)) for n in names]


def remember_mounts(vm: VmPaths, mounts: Sequence[Mount]) -> Result[None, AppError]:
    """Record the shares host-side; guest/mounts can't be trusted for `restart`/`list`."""

    def write() -> None:
        vm.last_mounts.write_text(plan.mounts_table(mounts))

    return attempt(write, because(f"can't write {vm.last_mounts}"))


def last_mounts(vm: VmPaths) -> tuple[Mount, ...]:
    try:
        lines = vm.last_mounts.read_text().splitlines()
    except OSError:
        return ()
    out: list[Mount] = []
    for line in lines:
        match line.split("\t"):
            case [_, mode, path] if mode in tuple(Mode):
                out.append(Mount(Path(path), Mode(mode)))
            case _:
                pass
    return tuple(out)


def ssh_port(vm: VmPaths) -> Result[int, AppError]:
    hint = f"start it with `amoeboid start --name {vm.name}`"
    if procs.running_qemu(vm) is None:
        return fail(f"VM '{vm.name}' isn't running", hint)
    return attempt(
        lambda: int(vm.ssh_port.read_text().strip()),
        because(f"can't read the ssh port of '{vm.name}'", hint),
        (OSError, ValueError),
    )


# -- helpers ----------------------------------------------------------------------------


def run_checked(argv: list[str], failure: str) -> Result[None, AppError]:
    match attempt(lambda: subprocess.run(argv, check=False), because(f"can't run {argv[0]}")):
        case Ok(done) if done.returncode == 0:
            return Ok(None)
        case Ok(_):
            return fail(failure)
        case Err() as err:
            return err


def _terminal_size() -> tuple[int, int] | None:
    if not sys.stdout.isatty():
        return None
    size = shutil.get_terminal_size(fallback=(0, 0))
    return (size.lines, size.columns) if size.lines > 0 else None


def _wait_until(
    condition: Callable[[], bool], timeout: float, error: Callable[[], AppError]
) -> Result[None, AppError]:
    deadline = time.monotonic() + timeout
    while not condition():
        if time.monotonic() >= deadline:
            return Err(error())
        time.sleep(0.05)
    return Ok(None)
