"""Writing into the guest-visible state dir without trusting what the guest left there.

Everything under `<state>/guest` is writable from inside the VM, so a guest could plant a
symlink (say, `home/.gitconfig -> ~/.ssh/authorized_keys` on the host) and wait for the
launcher to write through it. Every operation here walks from the share root with
`O_NOFOLLOW` directory descriptors and replaces files instead of opening existing ones.
"""

import os
from collections.abc import Generator
from contextlib import contextmanager, suppress
from pathlib import Path, PurePosixPath

from amoeboid.errors import AppError, fail
from amoeboid.result import Err, Ok, Result

_DIR_FLAGS = os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC


def ensure_dir(root: Path, rel: str) -> Result[None, AppError]:
    """Create `root/rel` (and parents) as real directories."""
    try:
        with _walk(root, PurePosixPath(rel).parts):
            return Ok(None)
    except OSError as exc:
        return _refused(root, rel, exc)


def replace_file(root: Path, rel: str, data: bytes, mode: int = 0o644) -> Result[None, AppError]:
    """Atomically-enough replace `root/rel` with a fresh regular file holding `data`."""
    parts = PurePosixPath(rel).parts
    if not parts:
        return fail(f"empty path under {root}")
    try:
        with _walk(root, parts[:-1]) as dir_fd:
            name = parts[-1]
            _unlink(name, dir_fd)
            flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW | os.O_CLOEXEC
            fd = os.open(name, flags, mode, dir_fd=dir_fd)
            try:
                os.fchmod(fd, mode)  # not subject to the umask
                os.write(fd, data)
            finally:
                os.close(fd)
        return Ok(None)
    except OSError as exc:
        return _refused(root, rel, exc)


def remove_file(root: Path, rel: str) -> Result[None, AppError]:
    parts = PurePosixPath(rel).parts
    try:
        with _walk(root, parts[:-1], create=False) as dir_fd:
            _unlink(parts[-1], dir_fd)
        return Ok(None)
    except FileNotFoundError:
        return Ok(None)
    except OSError as exc:
        return _refused(root, rel, exc)


@contextmanager
def _walk(root: Path, parts: tuple[str, ...], *, create: bool = True) -> Generator[int]:
    """Yield an fd for `root/parts...`, refusing symlinks and `..` along the way."""
    fd = os.open(root, _DIR_FLAGS)
    try:
        for part in parts:
            if part in {"..", "/"}:
                raise PermissionError(f"path component {part!r} not allowed")
            if create:
                with suppress(FileExistsError):
                    os.mkdir(part, 0o755, dir_fd=fd)
            next_fd = os.open(part, _DIR_FLAGS, dir_fd=fd)
            os.close(fd)
            fd = next_fd
        yield fd
    finally:
        os.close(fd)


def _unlink(name: str, dir_fd: int) -> None:
    with suppress(FileNotFoundError):
        os.unlink(name, dir_fd=dir_fd)


def _refused(root: Path, rel: str, exc: OSError) -> Err[AppError]:
    return fail(
        f"can't write {root / rel}: {exc.strerror or exc}",
        "something inside the VM may have replaced a directory there with a symlink; "
        "inspect it and remove the offending entry",
    )
