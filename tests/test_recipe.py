from pathlib import Path

import pytest

from amoeboid import recipe
from amoeboid.paths import VmPaths
from amoeboid.result import Err, Ok

ORIGINAL = b"{ pkgs, ... }: { }\n"
EDITED = b"{ pkgs, ... }: { environment.systemPackages = [ pkgs.cowsay ]; }\n"


@pytest.fixture
def vm(tmp_path: Path) -> VmPaths:
    v = VmPaths("t", tmp_path / "state", tmp_path / "run")
    v.guest.mkdir(parents=True)
    return v


@pytest.fixture
def source(tmp_path: Path) -> Path:
    file = tmp_path / "recipe.nix"
    file.write_bytes(ORIGINAL)
    return file


def guest_copy(vm: VmPaths) -> Path:
    return vm.guest / recipe.GUEST_RECIPE / "default.nix"


def built(vm: VmPaths) -> bytes:
    return (recipe.snapshot_dir(vm) / "default.nix").read_bytes()


def test_first_start_seeds_the_guest_copy(vm: VmPaths, source: Path) -> None:
    assert recipe.resolve(vm, source, reseed=False) == Ok(recipe.snapshot_dir(vm))
    assert guest_copy(vm).read_bytes() == ORIGINAL
    assert built(vm) == ORIGINAL


def test_edits_in_the_guest_copy_are_built(vm: VmPaths, source: Path) -> None:
    recipe.resolve(vm, source, reseed=False)
    guest_copy(vm).write_bytes(EDITED)
    (guest_copy(vm).parent / "extra.nix").write_bytes(b"{ }\n")
    recipe.resolve(vm, source, reseed=False)
    assert built(vm) == EDITED
    assert (recipe.snapshot_dir(vm) / "extra.nix").exists()
    assert guest_copy(vm).read_bytes() == EDITED


def test_removed_files_disappear_from_the_snapshot(vm: VmPaths, source: Path) -> None:
    recipe.resolve(vm, source, reseed=False)
    (guest_copy(vm).parent / "extra.nix").write_bytes(b"{ }\n")
    recipe.resolve(vm, source, reseed=False)
    (guest_copy(vm).parent / "extra.nix").unlink()
    recipe.resolve(vm, source, reseed=False)
    assert not (recipe.snapshot_dir(vm) / "extra.nix").exists()


def test_explicit_recipe_replaces_the_guest_copy(vm: VmPaths, source: Path, tmp_path: Path) -> None:
    recipe.resolve(vm, source, reseed=False)
    guest_copy(vm).write_bytes(EDITED)
    other = tmp_path / "other"
    other.mkdir()
    (other / "default.nix").write_bytes(b"{ }\n")
    recipe.resolve(vm, other, reseed=True)
    assert guest_copy(vm).read_bytes() == b"{ }\n"
    assert built(vm) == b"{ }\n"


def test_symlinks_in_the_guest_copy_are_refused(vm: VmPaths, source: Path, tmp_path: Path) -> None:
    recipe.resolve(vm, source, reseed=False)
    secret = tmp_path / "secret"
    secret.write_text("host secret")
    (guest_copy(vm).parent / "leak.nix").symlink_to(secret)
    result = recipe.resolve(vm, source, reseed=False)
    assert isinstance(result, Err)
    assert "not a regular file" in result.error.message
    assert b"host secret" not in b"".join(p.read_bytes() for p in recipe.snapshot_dir(vm).iterdir())


def test_deleted_guest_copy_is_reseeded(vm: VmPaths, source: Path) -> None:
    recipe.resolve(vm, source, reseed=False)
    guest_copy(vm).unlink()
    guest_copy(vm).parent.rmdir()
    recipe.resolve(vm, source, reseed=False)
    assert guest_copy(vm).read_bytes() == ORIGINAL


def test_missing_entry_point_is_reported(vm: VmPaths, source: Path) -> None:
    recipe.resolve(vm, source, reseed=False)
    guest_copy(vm).unlink()
    (guest_copy(vm).parent / "other.nix").write_bytes(b"{ }\n")
    result = recipe.resolve(vm, source, reseed=False)
    assert isinstance(result, Err)
    assert "no default.nix" in result.error.message


def test_reference_copies_and_readme(vm: VmPaths, tmp_path: Path) -> None:
    host = tmp_path / "host_u.nix"
    host.write_text("{ }")
    assert recipe.publish_reference(vm, host) == Ok(None)
    ref = vm.guest / recipe.GUEST_REFERENCE
    assert sorted(p.name for p in ref.iterdir()) == [
        "base.nix",
        "flake.lock",
        "flake.nix",
        "host.nix",
    ]
    assert (vm.guest / recipe.GUEST_DIR / "README.md").read_text().startswith("# This VM")
