from pathlib import Path

import pytest

from amoeboid.mounts import Mode, Mount, parse_spec, validate
from amoeboid.result import Err, Ok

CWD = Path("/work")


@pytest.mark.parametrize(
    ("spec", "mount"),
    [
        ("/srv/data", Mount(Path("/srv/data"), Mode.RW)),
        ("/srv/data:ro", Mount(Path("/srv/data"), Mode.RO)),
        ("/srv/data:rw", Mount(Path("/srv/data"), Mode.RW)),
        ("rel/dir:ro", Mount(Path("/work/rel/dir"), Mode.RO)),
        ("/odd:name", Mount(Path("/odd:name"), Mode.RW)),
        ("/odd:name:ro", Mount(Path("/odd:name"), Mode.RO)),
        ("/srv/../srv/x/", Mount(Path("/srv/x"), Mode.RW)),
    ],
)
def test_parse_spec(spec: str, mount: Mount) -> None:
    assert parse_spec(spec, CWD) == Ok(mount)


def test_parse_spec_rejects_empty_path() -> None:
    assert isinstance(parse_spec(":ro", CWD), Err)


HOME = Path("/home/u")


def rw(p: str) -> Mount:
    return Mount(Path(p), Mode.RW)


def check(*mounts: Mount, dirs: set[str] | None = None) -> Ok[list[Mount]] | Err[str]:
    existing = {m.path for m in mounts} if dirs is None else {Path(d) for d in dirs}
    return validate(mounts, [HOME], lambda p: p in existing).map_err(lambda e: e.message)


def test_parents_mount_before_children() -> None:
    a, b, c, d = rw("/home/u/dev/z"), rw("/home/u/dev"), rw("/home/u/dev-x"), rw("/data")
    assert check(a, b, c, d) == Ok([d, b, a, c])


@pytest.mark.parametrize("path", ["/", "/home", "/home/u"])
def test_refuses_to_cover_the_guest_home(path: str) -> None:
    match check(rw(path)):
        case Err(message):
            assert "would hide the VM's home directory /home/u" in message
        case Ok():
            pytest.fail("accepted")


def test_reports_every_problem_at_once() -> None:
    result = check(rw("/a"), rw("/a"), rw("/missing"), rw("/home"), dirs={"/a", "/home"})
    match result:
        case Err(message):
            assert "/a: given more than once" in message
            assert "/missing: not a directory" in message
            assert "/home: would hide" in message
        case Ok():
            pytest.fail("accepted")


def test_same_path_with_different_modes_is_a_duplicate() -> None:
    assert isinstance(check(rw("/a"), Mount(Path("/a"), Mode.RO)), Err)
