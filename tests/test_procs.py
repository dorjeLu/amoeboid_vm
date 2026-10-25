import shutil
import subprocess
from collections.abc import Callable, Iterator
from pathlib import Path

import pytest

from amoeboid import procs
from amoeboid.paths import VmPaths
from amoeboid.result import Err, Ok


@pytest.fixture
def vm(tmp_path: Path) -> VmPaths:
    v = VmPaths("t", tmp_path / "state", tmp_path / "run")
    v.runtime.mkdir()
    return v


type Spawn = Callable[[str], subprocess.Popen[bytes]]


@pytest.fixture
def spawn(tmp_path: Path) -> Iterator[Spawn]:
    """Start `sleep` under a chosen executable name; kill everything afterwards."""
    started: list[subprocess.Popen[bytes]] = []

    def start(name: str) -> subprocess.Popen[bytes]:
        exe = tmp_path / "bin" / name
        exe.parent.mkdir(exist_ok=True)
        shutil.copy(shutil.which("sleep") or "/bin/sleep", exe)
        proc = subprocess.Popen([str(exe), "60"])
        started.append(proc)
        return proc

    yield start
    for p in started:
        p.kill()
        p.wait()


def test_sweep_kills_leftover_helpers_with_a_warning(
    vm: VmPaths, spawn: Spawn, caplog: pytest.LogCaptureFixture
) -> None:
    helper = spawn("virtiofsd")
    procs.record(vm, "virtiofsd-d1", helper.pid)
    (vm.runtime / "d1.sock").write_text("")
    assert procs.sweep(vm) == Ok(None)
    assert helper.wait(timeout=5) is not None
    assert "killing leftover virtiofsd-d1" in caplog.text
    assert list(vm.runtime.iterdir()) == []


def test_sweep_leaves_unrelated_processes_with_recycled_pids_alone(
    vm: VmPaths, spawn: Spawn
) -> None:
    stranger = spawn("not-ours")
    procs.record(vm, "virtiofsd-d1", stranger.pid)
    assert procs.sweep(vm) == Ok(None)
    assert stranger.poll() is None


def test_sweep_refuses_while_qemu_runs(vm: VmPaths, spawn: Spawn) -> None:
    qemu = spawn("qemu-system-x86_64")
    procs.record(vm, procs.QEMU, qemu.pid)
    result = procs.sweep(vm)
    assert isinstance(result, Err)
    assert "already running" in result.error.message
    assert qemu.poll() is None


def test_garbage_pid_files_are_dropped(vm: VmPaths) -> None:
    vm.pid_file("qemu").write_text("not a pid")
    assert procs.tracked(vm) == []
    assert not vm.pid_file("qemu").exists()
