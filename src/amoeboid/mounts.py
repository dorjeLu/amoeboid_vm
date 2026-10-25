"""Host directories shared into the VM, each at its own path and with an access policy."""

from collections.abc import Callable, Iterable
from dataclasses import dataclass
from enum import StrEnum
from pathlib import Path

from amoeboid.errors import AppError, fail
from amoeboid.paths import absolute
from amoeboid.result import Err, Ok, Result, traverse_all


class Mode(StrEnum):
    RW = "rw"
    RO = "ro"


@dataclass(frozen=True, slots=True)
class Mount:
    path: Path
    mode: Mode

    def spec(self) -> str:
        return f"{self.path}:{self.mode}"


SPEC_HELP = "PATH[:rw|:ro], e.g. ~/dev/app or ~/dev/libs:ro (default policy: rw)"


def parse_spec(spec: str, cwd: Path) -> Result[Mount, AppError]:
    """Parse `PATH[:rw|:ro]`; relative paths resolve against `cwd`."""
    raw, mode = spec, Mode.RW
    head, sep, tail = spec.rpartition(":")
    if sep and tail in Mode:
        raw, mode = head, Mode(tail)
    if not raw:
        return fail(f"empty path in mount {spec!r}", SPEC_HELP)
    return Ok(Mount(absolute(raw, cwd), mode))


def validate(
    mounts: Iterable[Mount],
    protected: Iterable[Path],
    is_dir: Callable[[Path], bool],
) -> Result[list[Mount], AppError]:
    """Check every mount, reporting all problems at once; on success, order parents first.

    `protected` are directories that must stay visible in the guest (its home): sharing
    one of them, or anything above one, would cover it up.
    """
    guarded = list(protected)
    seen: set[Path] = set()

    def check(mount: Mount) -> Result[Mount, str]:
        p = mount.path
        if p in seen:
            return Err(f"{p}: given more than once")
        seen.add(p)
        if not is_dir(p):
            return Err(f"{p}: not a directory")
        if covered := next((g for g in guarded if g == p or g.is_relative_to(p)), None):
            return Err(f"{p}: would hide the VM's home directory {covered}")
        return Ok(mount)

    match traverse_all(list(mounts), check):
        case Ok(valid):
            # Nested shares mount on top of their parent, so parents go first; a path's
            # parts sort before any extension of them.
            return Ok(sorted(valid, key=lambda m: m.path.parts))
        case Err(problems):
            return fail("can't share these directories:\n" + "\n".join(f"  {p}" for p in problems))
