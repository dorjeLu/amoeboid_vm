"""The VM's recipe lives in the VM's own home, where it and the host user can edit it.

`~/amoeboid/recipe/` inside the guest (`<state>/guest/home/amoeboid/recipe` on the host)
is seeded from the configured recipe on the VM's first start, or replaced by an
explicit `--recipe`. Every start (and only a start, from the host) builds whatever is
there at that moment.

It is read without following symlinks and snapshotted outside the share before
building, so a link planted in the guest can't pull host files into the (world-readable)
Nix store.
"""

import logging
import shutil
from pathlib import Path

from amoeboid import guestfs
from amoeboid.errors import AppError, because, fail
from amoeboid.guestfs import Tree
from amoeboid.paths import BUNDLED_NIX, VmPaths
from amoeboid.result import Ok, Result, attempt, railway

log = logging.getLogger(__name__)

GUEST_DIR = "home/amoeboid"
GUEST_RECIPE = f"{GUEST_DIR}/recipe"
GUEST_REFERENCE = f"{GUEST_DIR}/reference"


def snapshot_dir(vm: VmPaths) -> Path:
    return vm.state / "recipe"


@railway
def resolve(vm: VmPaths, source: Path, reseed: bool) -> Result[Path, AppError]:
    """Snapshot the VM's recipe for building, seeding it from `source` when needed."""
    current = None if reseed else guestfs.read_tree(vm.guest, GUEST_RECIPE).or_bail()
    if current is None:
        current = read_source(source).or_bail()
        guestfs.replace_tree(vm.guest, GUEST_RECIPE, current).or_bail()
    if "default.nix" not in current:
        return fail(
            f"the recipe of '{vm.name}' has no default.nix ({vm.guest / GUEST_RECIPE})",
            "restore it, or replace the recipe with --recipe PATH",
        )
    store(snapshot_dir(vm), current).or_bail()
    return Ok(snapshot_dir(vm))


@railway
def publish_reference(vm: VmPaths, host_module: Path) -> Result[None, AppError]:
    """Refresh read-only-by-convention copies of the rest of the VM's definition."""
    files = {
        "flake.nix": BUNDLED_NIX / "flake.nix",
        "flake.lock": BUNDLED_NIX / "flake.lock",
        "base.nix": BUNDLED_NIX / "base.nix",
        "host.nix": host_module,
    }
    tree: Tree = {}
    for name, path in files.items():
        tree[name] = attempt(path.read_bytes, because(f"can't read {path}")).or_bail()
    guestfs.replace_tree(vm.guest, GUEST_REFERENCE, tree).or_bail()
    return guestfs.replace_file(vm.guest, f"{GUEST_DIR}/README.md", GUEST_README.encode())


def read_source(path: Path) -> Result[Tree, AppError]:
    """A recipe as a tree: a single file becomes default.nix, a directory is taken whole."""

    def read() -> Tree:
        if path.is_file():
            return {"default.nix": path.read_bytes()}
        return {
            f.relative_to(path).as_posix(): f.read_bytes()
            for f in sorted(path.rglob("*"))
            if f.is_file()
        }

    return attempt(read, because(f"can't read recipe {path}"))


def store(target: Path, tree: Tree) -> Result[None, AppError]:
    """Replace `target` with `tree`: build it aside, then swap it in."""

    def write() -> None:
        fresh = target.with_name(f".{target.name}.new")
        stale = target.with_name(f".{target.name}.old")
        for leftover in (fresh, stale):
            shutil.rmtree(leftover, ignore_errors=True)
        for rel, data in tree.items():
            file = fresh / rel
            file.parent.mkdir(parents=True, exist_ok=True)
            file.write_bytes(data)
        fresh.mkdir(parents=True, exist_ok=True)
        if target.exists():
            target.rename(stale)
        fresh.rename(target)
        shutil.rmtree(stale, ignore_errors=True)

    return attempt(write, because(f"can't write {target}"))


GUEST_README = """\
# This VM's NixOS definition

- `recipe/` is the recipe this VM is built from. Edit it here: `default.nix` is the entry
  point, and it may import other files in that directory.
- `reference/` holds the rest of the definition (flake, base module, host settings) for
  reading. It is rewritten on every start, so edits there are lost.

## Installing packages

The Nix store is read-only inside the VM, so nothing can be installed from here, and
anything installed outside your home directory is gone after a restart.

1. Find the package: `nixpkgs-search REGEX` (searches the exact nixpkgs this VM uses).
2. Add `pkgs.ATTRIBUTE` to `environment.systemPackages` in `recipe/default.nix`.
3. It is installed when the VM is next restarted from the host (`amoeboid restart`).
   Rebooting from inside the VM only shuts it down.

The recipe is merged with `reference/base.nix` (user, shares, ssh, console) and
`reference/host.nix`. A recipe that fails to build keeps the VM from starting, so keep
edits small and valid Nix.
"""
