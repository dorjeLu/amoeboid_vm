from pathlib import Path

import pytest

from amoeboid import recipe
from amoeboid.paths import VmPaths
from amoeboid.recipe import Requested, parse_packages, render_packages_module
from amoeboid.result import Err, Ok


@pytest.fixture
def vm(tmp_path: Path) -> VmPaths:
    v = VmPaths("t", tmp_path / "state", tmp_path / "run")
    v.guest.mkdir(parents=True)
    return v


def packages_file(vm: VmPaths) -> Path:
    return vm.guest / recipe.GUEST_PACKAGES


# -- parsing what the guest asks for (pure) ----------------------------------------------


def test_valid_names_are_kept_in_order_without_duplicates() -> None:
    text = '(list)packages { - "figlet"; - "python3Packages.requests"; - "figlet"; - "gcc_14" }'
    assert parse_packages(text) == Requested(("figlet", "python3Packages.requests", "gcc_14"), ())


def test_the_seed_template_requests_nothing() -> None:
    assert parse_packages(recipe.PACKAGES_TEMPLATE) == Requested((), ())


@pytest.mark.parametrize(
    "name",
    [
        '"; services.openssh.enable = false; "',
        "${builtins.readFile /etc/shadow}",
        "../etc",
        "a b",
        "pkgs.",
        ".hidden",
        "1password",
        "a.b.c.d.e.f.g.h.i",
    ],
)
def test_anything_but_attribute_names_is_dropped(name: str) -> None:
    text = f'(list)packages {{ - {_kdl_string(name)}; - "ok" }}'
    result = parse_packages(text)
    assert result.packages == ("ok",)
    assert len(result.problems) == 1


def test_non_strings_and_wrong_shapes_are_problems_not_errors() -> None:
    assert parse_packages("(list)packages { - 1 }").packages == ()
    assert parse_packages('packages "figlet"').problems
    assert parse_packages('(list)packages { - "a" }\nservices { x 1 }').problems
    assert parse_packages("this is { not kdl").problems


def test_package_count_is_capped() -> None:
    items = " ".join(f'- "p{i}";' for i in range(recipe.MAX_PACKAGES + 5))
    result = parse_packages(f"(list)packages {{ {items} }}")
    assert len(result.packages) == recipe.MAX_PACKAGES
    assert result.problems


def test_rendered_module_only_adds_packages() -> None:
    module = render_packages_module(("figlet", "python3Packages.requests"))
    assert 'wanted = [ "figlet" "python3Packages.requests" ];' in module
    assert module.count("environment.systemPackages") == 1
    assert "microvm" not in module


def _kdl_string(s: str) -> str:
    return '"' + s.replace("\\", "\\\\").replace('"', '\\"') + '"'


# -- files --------------------------------------------------------------------------------


def test_missing_request_file_is_seeded_and_renders_an_empty_module(vm: VmPaths) -> None:
    assert recipe.prepare_packages(vm) == Ok(recipe.packages_module_path(vm))
    assert packages_file(vm).read_text() == recipe.PACKAGES_TEMPLATE
    assert "wanted = [  ];" in recipe.packages_module_path(vm).read_text()


def test_requests_are_rendered_and_the_guest_file_is_left_alone(vm: VmPaths) -> None:
    packages_file(vm).parent.mkdir(parents=True)
    text = '// mine\n(list)packages { - "figlet"; - "bad name" }\n'
    packages_file(vm).write_text(text)
    recipe.prepare_packages(vm)
    assert '"figlet"' in recipe.packages_module_path(vm).read_text()
    assert packages_file(vm).read_text() == text


def test_symlinked_request_file_is_refused(vm: VmPaths, tmp_path: Path) -> None:
    secret = tmp_path / "secret"
    secret.write_text('(list)packages { - "figlet" }')
    packages_file(vm).parent.mkdir(parents=True)
    packages_file(vm).symlink_to(secret)
    assert isinstance(recipe.prepare_packages(vm), Err)


def test_oversized_request_file_is_refused(vm: VmPaths) -> None:
    packages_file(vm).parent.mkdir(parents=True)
    packages_file(vm).write_bytes(b" " * (recipe.MAX_PACKAGES_BYTES + 1))
    assert isinstance(recipe.prepare_packages(vm), Err)


def test_reference_copies_the_whole_definition(vm: VmPaths, tmp_path: Path) -> None:
    host = tmp_path / "host_u.nix"
    host.write_text("{ }")
    src = tmp_path / "my-recipe"
    (src / "lib").mkdir(parents=True)
    (src / "default.nix").write_text("{ }")
    (src / "lib/extra.nix").write_text("{ }")
    recipe.prepare_packages(vm)
    stale = vm.guest / recipe.GUEST_REFERENCE / "stale.nix"
    stale.parent.mkdir(parents=True)
    stale.write_text("edited in the guest")
    assert recipe.publish_reference(vm, src, host) == Ok(None)
    ref = vm.guest / recipe.GUEST_REFERENCE
    names = sorted(p.relative_to(ref).as_posix() for p in ref.rglob("*") if p.is_file())
    assert names == [
        "base.nix",
        "flake.lock",
        "flake.nix",
        "host.nix",
        "packages.nix",
        "recipe/default.nix",
        "recipe/lib/extra.nix",
    ]
    assert (vm.guest / recipe.GUEST_DIR / "README.md").read_text().startswith("# This VM")


def test_single_file_recipe_is_published_as_default_nix(vm: VmPaths, tmp_path: Path) -> None:
    host = tmp_path / "host_u.nix"
    host.write_text("{ }")
    src = tmp_path / "r.nix"
    src.write_text("{ pkgs, ... }: { }")
    recipe.prepare_packages(vm)
    recipe.publish_reference(vm, src, host)
    copied = vm.guest / recipe.GUEST_REFERENCE / "recipe/default.nix"
    assert copied.read_text() == "{ pkgs, ... }: { }"
