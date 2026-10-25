"""The NixOS configuration a VM is built from: presets, per-VM copies, guest copies.

    bundled nix/  --first run-->  ~/.config/amoeboid/nix_presets/default/
    preset        --new VM----->  <state>/nix/                (host-owned, what is built)
    <state>/nix/  --new VM----->  ~/amoeboid/nix/ in the VM   (the VM's own, editable copy)
    <state>/nix/  --every start-> ~/amoeboid/reference/ in the VM (what is running)

The host never reads anything back from the VM's copies; folding the VM's edits into
the host-side configuration is left to whoever drives amoeboid.
"""

import logging
import re
import shutil
from pathlib import Path

from amoeboid import guestfs
from amoeboid.errors import AppError, because, fail
from amoeboid.guestfs import Tree
from amoeboid.paths import BUNDLED_NIX, Paths, VmPaths, absolute, is_path_spec
from amoeboid.result import Ok, Result, attempt, railway

log = logging.getLogger(__name__)

DEFAULT_PRESET = "default"
REQUIRED = ("flake.nix",)

GUEST_DIR = "home/amoeboid"
GUEST_OWN = f"{GUEST_DIR}/nix"
GUEST_REFERENCE = f"{GUEST_DIR}/reference"

_PRESET_NAME = re.compile(r"[A-Za-z0-9][A-Za-z0-9._-]*")
_IGNORED = shutil.ignore_patterns("__pycache__", "result", ".git")


# -- presets ----------------------------------------------------------------------------


def preset_names(paths: Paths) -> list[str]:
    root = paths.presets_dir
    if not root.is_dir():
        return []
    return sorted(d.name for d in root.iterdir() if (d / "flake.nix").is_file())


def resolve_base(spec: str, paths: Paths, cwd: Path) -> Result[Path, AppError]:
    """A preset name (under nix_presets/) or a directory path, checked for a flake."""
    if is_path_spec(spec):
        base = absolute(spec, cwd)
    elif _PRESET_NAME.fullmatch(spec):
        base = paths.presets_dir / spec
    else:
        return fail(f"bad preset name {spec!r}", "use letters, digits, '.', '_', '-'")
    if missing := [f for f in REQUIRED if not (base / f).is_file()]:
        names = ", ".join(preset_names(paths)) or "none"
        return fail(
            f"{base} is not a NixOS config dir (missing {', '.join(missing)})",
            f"presets in {paths.presets_dir}: {names}",
        )
    return Ok(base)


def ensure_default_preset(paths: Paths) -> Result[bool, AppError]:
    """Copy the bundled config to nix_presets/default unless it exists; True if copied."""
    target = paths.presets_dir / DEFAULT_PRESET
    if target.exists():
        return Ok(False)

    def copy() -> bool:
        shutil.copytree(BUNDLED_NIX, target, ignore=_IGNORED)
        return True

    return attempt(copy, because(f"can't create {target}"))


# -- per VM -----------------------------------------------------------------------------


@railway
def prepare(
    vm: VmPaths, base: Path | None, default_base: Path | None, host_module: Path
) -> Result[Path, AppError]:
    """The VM's host-side config dir, creating or rebasing it, plus its guest copies.

    A new VM copies `base` (or `default_base`); an existing one keeps its own config
    unless `base` is given, which replaces it. The guest's own copy is seeded when the
    VM is created (or if it went missing) and otherwise left alone; the reference copy
    is rewritten on every start.
    """
    own = vm.nix_config
    if base is not None or not own.is_dir():
        source = base or default_base
        if source is None:
            return fail(f"no NixOS config to create '{vm.name}' from")
        verb = "rebasing" if own.is_dir() else "creating"
        log.info("%s '%s' from %s", verb, vm.name, source)
        replace_dir(own, source).or_bail()
    tree = read_tree(own).or_bail()
    tree["host.nix"] = attempt(host_module.read_bytes, because("can't read host module")).or_bail()
    if not (vm.guest / GUEST_OWN).exists():
        guestfs.replace_tree(vm.guest, GUEST_OWN, tree).or_bail()
    guestfs.replace_tree(vm.guest, GUEST_REFERENCE, tree).or_bail()
    guestfs.replace_file(vm.guest, f"{GUEST_DIR}/README.md", GUEST_README.encode()).or_bail()
    return Ok(own)


def replace_dir(target: Path, source: Path) -> Result[None, AppError]:
    """Replace `target` with a copy of `source`: copy aside, then swap in."""

    def copy() -> None:
        fresh = target.with_name(f".{target.name}.new")
        stale = target.with_name(f".{target.name}.old")
        for leftover in (fresh, stale):
            shutil.rmtree(leftover, ignore_errors=True)
        shutil.copytree(source, fresh, ignore=_IGNORED)
        if target.exists():
            target.rename(stale)
        fresh.rename(target)
        shutil.rmtree(stale, ignore_errors=True)

    return attempt(copy, because(f"can't copy {source} to {target}"))


def read_tree(root: Path) -> Result[Tree, AppError]:
    """Every regular file under a host-owned directory."""

    def read() -> Tree:
        return {
            f.relative_to(root).as_posix(): f.read_bytes()
            for f in sorted(root.rglob("*"))
            if f.is_file() and not f.is_symlink()
        }

    return attempt(read, because(f"can't read {root}"))


GUEST_README = """\
# This VM's NixOS configuration

- `nix/` is your own copy of the configuration this VM was created from: the flake,
  `base.nix` (user, shares, ssh, console), `recipe.nix` (packages, services, tweaks)
  and `host.nix` (the host's user and machine settings, passed in as the `host` input).
  Edit it freely; amoeboid never overwrites or reads it. Changes there are not applied
  automatically: whoever runs this VM decides when and how to adopt them.
- `reference/` is the configuration this VM is actually running. It is rewritten on
  every start, so edits there are lost; diff your copy against it to see your changes.

The Nix store is read-only inside this VM, so nothing can be installed through Nix from
here, and anything installed outside your home directory is gone after a restart. Find
packages with `nixpkgs-search REGEX` (it searches the exact nixpkgs this VM uses), then
add them to `nix/recipe.nix`.
"""
