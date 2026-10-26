from pathlib import Path

import pytest
from hypothesis import given
from hypothesis import strategies as st

from amoeboid import config
from amoeboid.config import Identity, RunConfig, SshConfig, TimeoutsConfig
from amoeboid.errors import AppError
from amoeboid.mounts import Mode, Mount
from amoeboid.paths import Paths, resolve_paths
from amoeboid.result import Err, Ok, Result

BASE = Path("/cfg")


def ok[T](result: Result[T, AppError]) -> T:
    match result:
        case Ok(value):
            return value
        case Err(error):
            pytest.fail(error.render())


def err(result: Result[object, AppError]) -> str:
    match result:
        case Err(error):
            return error.message
        case Ok(value):
            pytest.fail(f"unexpectedly valid: {value!r}")


# -- RunConfig <-> text is a bijection on valid configs ------------------------------------

segments = st.text("abcxyz019-_.", min_size=1, max_size=8).filter(lambda s: s not in {".", ".."})
abs_paths = st.lists(segments, min_size=1, max_size=4).map(lambda ps: Path("/", *ps))
rel_files = st.lists(segments, min_size=1, max_size=3).map("/".join)
configs = st.builds(
    RunConfig,
    user=st.from_regex(r"[a-z_][a-z0-9_-]{0,31}", fullmatch=True).filter(lambda u: u != "root"),
    uid=st.integers(1, 2**32 - 2),
    gid=st.integers(1, 2**32 - 2),
    base_nix_config=st.from_regex(r"[A-Za-z0-9][A-Za-z0-9._-]{0,15}", fullmatch=True)
    | abs_paths.map(str),
    default_mounts=st.lists(st.builds(Mount, abs_paths, st.sampled_from(Mode))).map(tuple),
    home_files=st.lists(rel_files).map(tuple),
    ssh=st.builds(SshConfig, st.just(0) | st.integers(1024, 65535)),
    timeouts=st.builds(TimeoutsConfig, st.integers(1, 86400), st.integers(1, 86400)),
)


@given(configs)
def test_config_survives_a_trip_through_kdl(cfg: RunConfig) -> None:
    assert ok(config.parse(ok(config.render(cfg)), BASE)) == cfg


@given(configs)
def test_rendering_is_stable(cfg: RunConfig) -> None:
    text = ok(config.render(cfg))
    assert ok(config.render(ok(config.parse(text, BASE)))) == text


def test_rendered_example() -> None:
    cfg = RunConfig(
        user="alice",
        uid=1000,
        gid=100,
        default_mounts=(Mount(Path("/home/alice/dev"), Mode.RW), Mount(Path("/srv"), Mode.RO)),
        home_files=(".gitconfig",),
    )
    assert ok(config.render(cfg)) == config.HEADER + (
        "\n"
        'user "alice"\n'
        "uid 1000\n"
        "gid 100\n"
        'base-nix-config "default"\n'
        "(list)default-mounts {\n"
        "    - {\n"
        '        path "/home/alice/dev"\n'
        '        mode "rw"\n'
        "    }\n"
        "    - {\n"
        '        path "/srv"\n'
        '        mode "ro"\n'
        "    }\n"
        "}\n"
        "(list)home-files {\n"
        '    - ".gitconfig"\n'
        "}\n"
        "ssh {\n"
        "    port 0\n"
        "}\n"
        "timeouts {\n"
        "    boot 180\n"
        "    stop 60\n"
        "}\n"
    )


# -- decoding: defaults, leniency where harmless, and errors ------------------------------


def test_minimal_file_gets_defaults() -> None:
    cfg = ok(config.parse('user "bob"\nuid 1001\ngid 1001\n', BASE))
    assert cfg == RunConfig("bob", 1001, 1001)


def test_relative_paths_resolve_against_the_config_dir() -> None:
    text = 'user "bob"; uid 1; gid 1; base-nix-config "./presets/mine"\n'
    text += '(list)default-mounts { - { path "../dev" } }'
    cfg = ok(config.parse(text, BASE))
    assert cfg.base_nix_config == "/cfg/presets/mine"
    assert cfg.default_mounts == (Mount(Path("/dev"), Mode.RW),)


def test_every_problem_is_reported() -> None:
    text = """
        user "root"
        uid #true
        base-nix-config 3
        (list)default-mounts { - { path "/x"; mode "rx" }; - "/y" }
        (list)home-files { - "../escape"; - "/abs" }
        ssh { port 22; colour "red" }
        timeouts { boot 0; stop 1.5; kill 3 }
        typo 1
    """
    message = err(config.parse(text, BASE))
    for expected in [
        "user: 'root' isn't a usable account name",
        "uid: expected an integer, got True",
        "gid: missing",
        "base-nix-config: expected a string, got 3",
        "default-mounts: item 0: mode must be one of rw, ro, got 'rx'",
        "home-files: item 0: '../escape' must be a path relative to $HOME",
        "ssh: port: use 0 (pick a free port) or 1024..65535",
        "timeouts: boot: 0 is outside 1..86400",
        "unknown key(s) typo",
    ]:
        assert expected in message


def test_timeouts_may_be_given_in_part() -> None:
    cfg = ok(config.parse('user "bob"; uid 1; gid 1; timeouts { boot 600 }', BASE))
    assert cfg.timeouts == TimeoutsConfig(boot=600, stop=60)


def test_invalid_kdl_is_an_error_not_an_exception() -> None:
    assert "invalid KDL" in err(config.parse("user {", BASE))


# -- files ------------------------------------------------------------------------------


@pytest.fixture
def paths(tmp_path: Path) -> Paths:
    home = tmp_path / "home"
    (home / ".config/git").mkdir(parents=True)
    (home / ".config/git/config").write_text("[user]\n")
    return ok(resolve_paths({"HOME": str(home)}, uid=1000))


ME = Identity("alice", 1000, 1000)


def test_first_run_writes_only_the_config(paths: Paths) -> None:
    loaded = ok(config.load_or_create(paths, ME))
    assert loaded.created
    assert loaded.config == RunConfig("alice", 1000, 1000, home_files=(".config/git/config",))
    assert paths.config_file.read_text().startswith(config.HEADER)
    assert [p.name for p in paths.config_dir.iterdir()] == ["run_config.kdl"]


def test_later_runs_load_without_rewriting(paths: Paths) -> None:
    ok(config.load_or_create(paths, ME))
    paths.config_file.write_text('// hand edited\nuser "alice"; uid 1000; gid 1000\n')
    loaded = ok(config.load_or_create(paths, Identity("someone-else", 5, 5)))
    assert not loaded.created
    assert loaded.config == RunConfig("alice", 1000, 1000)
    assert paths.config_file.read_text().startswith("// hand edited")


def test_save_replaces_the_file_and_leaves_no_temp_files(paths: Paths) -> None:
    cfg = ok(config.load_or_create(paths, ME)).config
    updated = config.with_default_mounts(cfg, [Mount(Path("/srv"), Mode.RO)])
    ok(config.save(paths, updated))
    assert ok(config.load(paths)) == updated
    assert [p.name for p in paths.config_dir.iterdir()] == ["run_config.kdl"]


def test_broken_file_points_at_itself(paths: Paths) -> None:
    paths.config_dir.mkdir(parents=True)
    paths.config_file.write_text("uid 1\n")
    match config.load(paths):
        case Err(error):
            assert str(paths.config_file) in error.message
            assert error.hint is not None
        case Ok():
            pytest.fail("accepted")
