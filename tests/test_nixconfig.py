import shutil
from pathlib import Path

import pytest

from amoeboid import nixconfig
from amoeboid.paths import BUNDLED_NIX, Paths, VmPaths
from amoeboid.result import Err, Ok


@pytest.fixture
def paths(tmp_path: Path) -> Paths:
    return Paths(tmp_path, tmp_path / "cfg", tmp_path / "data", tmp_path / "run")


@pytest.fixture
def vm(paths: Paths) -> VmPaths:
    v = paths.vm("t")
    v.guest.mkdir(parents=True)
    return v


@pytest.fixture
def host_module(tmp_path: Path) -> Path:
    h = tmp_path / "host_u.nix"
    h.write_text('{ amoeboid.user = "u"; }')
    return h


def preset(paths: Paths, name: str, recipe: str) -> Path:
    d = paths.presets_dir / name
    d.mkdir(parents=True)
    (d / "flake.nix").write_text("{ }")
    (d / "recipe.nix").write_text(recipe)
    return d


# -- presets ----------------------------------------------------------------------------


def test_default_preset_is_a_copy_of_the_bundled_config(paths: Paths) -> None:
    assert nixconfig.ensure_default_preset(paths) == Ok(True)
    default = paths.presets_dir / "default"
    for name in ("flake.nix", "flake.lock", "base.nix", "recipe.nix", "host-default.nix"):
        assert (default / name).read_bytes() == (BUNDLED_NIX / name).read_bytes()
    assert nixconfig.preset_names(paths) == ["default"]


def test_default_preset_is_never_overwritten(paths: Paths) -> None:
    nixconfig.ensure_default_preset(paths)
    (paths.presets_dir / "default/recipe.nix").write_text("mine")
    assert nixconfig.ensure_default_preset(paths) == Ok(False)
    assert (paths.presets_dir / "default/recipe.nix").read_text() == "mine"


def test_resolve_by_name_or_path(paths: Paths, tmp_path: Path) -> None:
    custom = preset(paths, "mycustom", "{ }")
    assert nixconfig.resolve_base("mycustom", paths, Path("/")) == Ok(custom)
    assert nixconfig.resolve_base(str(custom), paths, Path("/")) == Ok(custom)
    assert nixconfig.resolve_base("./cfg/nix_presets/mycustom", paths, tmp_path) == Ok(custom)


@pytest.mark.parametrize("spec", ["missing", "./nowhere", "bad name", "-x"])
def test_resolve_rejects_non_configs(paths: Paths, spec: str) -> None:
    preset(paths, "mycustom", "{ }")
    match nixconfig.resolve_base(spec, paths, Path("/")):
        case Err(error):
            assert error.message
        case Ok(found):
            pytest.fail(f"accepted {found}")


def test_unknown_preset_lists_the_available_ones(paths: Paths) -> None:
    preset(paths, "mycustom", "{ }")
    result = nixconfig.resolve_base("nope", paths, Path("/"))
    assert isinstance(result, Err)
    assert "mycustom" in (result.error.hint or "")


# -- per VM -----------------------------------------------------------------------------


def guest(vm: VmPaths, rel: str) -> Path:
    return vm.guest / nixconfig.GUEST_DIR / rel


def test_new_vm_copies_its_preset_and_seeds_both_guest_copies(
    paths: Paths, vm: VmPaths, host_module: Path
) -> None:
    base = preset(paths, "default", "v1")
    assert nixconfig.prepare(vm, None, base, host_module) == Ok(vm.nix_config)
    assert (vm.nix_config / "recipe.nix").read_text() == "v1"
    for copy in ("nix", "reference"):
        assert guest(vm, f"{copy}/recipe.nix").read_text() == "v1"
        assert guest(vm, f"{copy}/host.nix").read_text() == host_module.read_text()
    assert guest(vm, "README.md").read_text().startswith("# This VM")


def test_existing_vm_keeps_its_config_when_the_preset_changes(
    paths: Paths, vm: VmPaths, host_module: Path
) -> None:
    base = preset(paths, "default", "v1")
    nixconfig.prepare(vm, None, base, host_module)
    (base / "recipe.nix").write_text("v2")
    nixconfig.prepare(vm, None, None, host_module)
    assert (vm.nix_config / "recipe.nix").read_text() == "v1"


def test_guest_edits_stay_in_the_guest(paths: Paths, vm: VmPaths, host_module: Path) -> None:
    base = preset(paths, "default", "v1")
    nixconfig.prepare(vm, None, base, host_module)
    guest(vm, "nix/recipe.nix").write_text("edited by the agent")
    guest(vm, "reference/recipe.nix").write_text("edited by the agent")
    nixconfig.prepare(vm, None, None, host_module)
    assert (vm.nix_config / "recipe.nix").read_text() == "v1"  # never read back
    assert guest(vm, "nix/recipe.nix").read_text() == "edited by the agent"  # never overwritten
    assert guest(vm, "reference/recipe.nix").read_text() == "v1"  # always refreshed


def test_host_edits_to_the_vm_config_show_up_in_the_reference(
    paths: Paths, vm: VmPaths, host_module: Path
) -> None:
    nixconfig.prepare(vm, None, preset(paths, "default", "v1"), host_module)
    (vm.nix_config / "recipe.nix").write_text("host edit")
    nixconfig.prepare(vm, None, None, host_module)
    assert guest(vm, "reference/recipe.nix").read_text() == "host edit"
    assert guest(vm, "nix/recipe.nix").read_text() == "v1"


def test_explicit_base_rebases_an_existing_vm(paths: Paths, vm: VmPaths, host_module: Path) -> None:
    nixconfig.prepare(vm, None, preset(paths, "default", "v1"), host_module)
    other = preset(paths, "other", "other")
    (other / "extra.nix").write_text("{ }")
    nixconfig.prepare(vm, other, None, host_module)
    assert (vm.nix_config / "recipe.nix").read_text() == "other"
    assert (vm.nix_config / "extra.nix").exists()
    assert guest(vm, "reference/extra.nix").exists()
    assert guest(vm, "nix/recipe.nix").read_text() == "v1"


def test_deleted_guest_copy_is_reseeded(paths: Paths, vm: VmPaths, host_module: Path) -> None:
    nixconfig.prepare(vm, None, preset(paths, "default", "v1"), host_module)
    shutil.rmtree(guest(vm, "nix"))
    nixconfig.prepare(vm, None, None, host_module)
    assert guest(vm, "nix/recipe.nix").read_text() == "v1"


def test_symlinked_guest_dir_is_refused_not_followed(
    paths: Paths, vm: VmPaths, host_module: Path, tmp_path: Path
) -> None:
    nixconfig.prepare(vm, None, preset(paths, "default", "v1"), host_module)
    outside = tmp_path / "outside"
    outside.mkdir()
    shutil.rmtree(guest(vm, "reference"))
    guest(vm, "reference").symlink_to(outside)
    nixconfig.prepare(vm, None, None, host_module)
    assert list(outside.iterdir()) == []
    assert not guest(vm, "reference").is_symlink()
