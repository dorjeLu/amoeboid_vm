from pathlib import Path

from amoeboid import vm
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
