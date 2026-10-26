import shlex
from pathlib import Path

from amoeboid import vm
from amoeboid.config import RunConfig
from amoeboid.mounts import Mode, Mount
from amoeboid.paths import VmPaths
from amoeboid.result import Ok


def test_last_mounts_come_from_the_host_side_record_not_the_guest(tmp_path: Path) -> None:
    v = VmPaths("t", tmp_path / "state", tmp_path / "run")
    v.guest.mkdir(parents=True)
    mounts = [Mount(Path("/srv/a"), Mode.RW), Mount(Path("/srv/b"), Mode.RO)]
    assert vm.remember_mounts(v, mounts) == Ok(None)
    (v.guest / "mounts").write_text(f"d1\trw\t{tmp_path.parent}\n")  # written by the guest
    assert vm.last_mounts(v) == tuple(mounts)
    assert vm.status(v).mounts == tuple(mounts)


def test_logged_ssh_command_is_what_amoeboid_ssh_runs(tmp_path: Path) -> None:
    v = VmPaths("proj", tmp_path / "my state", tmp_path / "run")
    req = vm.StartRequest(
        vm=v,
        home=Path("/home/u"),
        config=RunConfig("u", 1000, 1000),
        base_nix_config=None,
        default_nix_config=None,
        mounts=(),
        detach=True,
        boot_timeout=1.0,
    )
    argv = shlex.split(vm.ssh_command(req, 40022))
    assert argv[:3] == ["ssh", "-p", "40022"]
    assert f"UserKnownHostsFile={v.known_hosts}" in argv  # quoted despite the space
    assert "HostKeyAlias=amoeboid-proj" in argv
    assert argv[-1] == "u@127.0.0.1"
