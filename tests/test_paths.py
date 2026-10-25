from pathlib import Path

import pytest

from amoeboid.errors import AppError
from amoeboid.paths import Paths, absolute, resolve_paths, vm_name
from amoeboid.result import Err, Ok


def paths(env: dict[str, str]) -> Paths:
    match resolve_paths(env, uid=4242):
        case Ok(p):
            return p
        case Err(e):
            pytest.fail(e.render())


def test_defaults_derive_from_home(tmp_path: Path) -> None:
    p = paths({"HOME": str(tmp_path)})
    assert p.config_file == tmp_path / ".config/amoeboid/run_config.kdl"
    assert p.data_dir == tmp_path / ".local/share/amoeboid"
    assert p.runtime_dir == Path("/tmp/amoeboid-4242/amoeboid")


def test_xdg_variables_win_and_relative_ones_are_ignored(tmp_path: Path) -> None:
    env = {
        "HOME": str(tmp_path),
        "XDG_CONFIG_HOME": str(tmp_path / "cfg"),
        "XDG_DATA_HOME": "relative/data",
        "XDG_RUNTIME_DIR": str(tmp_path / "run"),
    }
    p = paths(env)
    assert p.config_dir == tmp_path / "cfg/amoeboid"
    assert p.data_dir == tmp_path / ".local/share/amoeboid"
    assert p.runtime_dir == tmp_path / "run/amoeboid"


def test_tmpdir_is_the_runtime_fallback(tmp_path: Path) -> None:
    p = paths({"HOME": str(tmp_path), "TMPDIR": str(tmp_path / "t")})
    assert p.runtime_dir == tmp_path / "t/amoeboid-4242/amoeboid"


@pytest.mark.parametrize("home", [None, "", "relative/home"])
def test_home_must_be_absolute(home: str | None) -> None:
    env = {} if home is None else {"HOME": home}
    assert isinstance(resolve_paths(env, uid=1), Err)


def test_vm_paths(tmp_path: Path) -> None:
    vm = paths({"HOME": str(tmp_path), "XDG_RUNTIME_DIR": "/run/user/7"}).vm("box")
    assert vm.guest == tmp_path / ".local/share/amoeboid/box/guest"
    assert vm.pid_file("qemu") == Path("/run/user/7/amoeboid/box/qemu.pid")


def test_absolute_resolves_relative_and_tilde(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setenv("HOME", str(tmp_path))
    assert absolute("a/../b", tmp_path) == tmp_path / "b"
    assert absolute("~/x", Path("/elsewhere")) == tmp_path / "x"
    assert absolute("/abs", tmp_path) == Path("/abs")


def test_absolute_resolves_symlinks(tmp_path: Path) -> None:
    (tmp_path / "real").mkdir()
    (tmp_path / "link").symlink_to(tmp_path / "real")
    assert absolute("link", tmp_path) == tmp_path / "real"


@pytest.mark.parametrize("name", ["default", "proj-1", "A9"])
def test_good_vm_names(name: str) -> None:
    assert vm_name(name) == Ok(name)


@pytest.mark.parametrize("name", ["", "-x", "a_b", "a.b", "x" * 64, "a/b"])
def test_bad_vm_names(name: str) -> None:
    match vm_name(name):
        case Err(AppError(message=m)):
            assert repr(name) in m
        case Ok():
            pytest.fail("accepted")
