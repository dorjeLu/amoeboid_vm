"""Writing into the guest-visible state dir without trusting what the guest left there.

Everything under `<state>/guest` is writable from inside the VM, so a guest could plant a
symlink (say, `home/.gitconfig -> ~/.ssh/authorized_keys` on the host) and wait for the
launcher to write through it. Every operation here walks from the share root with
`O_NOFOLLOW` directory descriptors and replaces files instead of opening existing ones.
"""

import os
import shutil
import stat
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


type Tree = dict[str, bytes]
"""A directory's regular files: POSIX relative path -> contents."""

MAX_TREE_FILES = 256
MAX_TREE_BYTES = 4 * 1024 * 1024


def read_tree(root: Path, rel: str) -> Result[Tree | None, AppError]:
    """Read every file under `root/rel`; None if it doesn't exist.

    Anything but plain files and directories (symlinks especially: they could point
    at host secrets) is refused, as are trees too large to be a NixOS recipe.
    """
    tree: Tree = {}

    def collect(dir_fd: int, prefix: str) -> None:
        with os.scandir(dir_fd) as entries:
            for entry in sorted(entries, key=lambda e: e.name):
                path = f"{prefix}{entry.name}"
                if entry.is_dir(follow_symlinks=False):
                    sub = os.open(entry.name, _DIR_FLAGS, dir_fd=dir_fd)
                    try:
                        collect(sub, f"{path}/")
                    finally:
                        os.close(sub)
                elif entry.is_file(follow_symlinks=False):
                    fd = os.open(
                        entry.name, os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC, dir_fd=dir_fd
                    )
                    with os.fdopen(fd, "rb") as f:
                        tree[path] = f.read(MAX_TREE_BYTES + 1)
                    if len(tree) > MAX_TREE_FILES or sum(map(len, tree.values())) > MAX_TREE_BYTES:
                        raise _TooBig(f"more than {MAX_TREE_FILES} files or {MAX_TREE_BYTES} bytes")
                else:
                    raise _NotPlain(f"{path} is not a regular file or directory")

    try:
        with _walk(root, PurePosixPath(rel).parts, create=False) as fd:
            collect(fd, "")
        return Ok(tree)
    except FileNotFoundError:
        return Ok(None)
    except (_TooBig, _NotPlain) as exc:
        return fail(f"refusing to read {root / rel}: {exc}")
    except OSError as exc:
        return _refused(root, rel, exc)


def replace_tree(root: Path, rel: str, tree: Tree) -> Result[None, AppError]:
    """Make `root/rel` contain exactly `tree`, deleting whatever was there."""
    parts = PurePosixPath(rel).parts
    try:
        with _walk(root, parts[:-1]) as parent, suppress(FileNotFoundError):
            if stat.S_ISDIR(os.stat(parts[-1], dir_fd=parent, follow_symlinks=False).st_mode):
                shutil.rmtree(parts[-1], dir_fd=parent)  # symlink-attack safe on Linux
            else:
                os.unlink(parts[-1], dir_fd=parent)
    except OSError as exc:
        return _refused(root, rel, exc)
    for path, data in tree.items():
        match replace_file(root, f"{rel}/{path}", data):
            case Err() as err:
                return err
            case Ok():
                pass
    return ensure_dir(root, rel)


class _TooBig(Exception):
    pass


class _NotPlain(Exception):
    pass


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
