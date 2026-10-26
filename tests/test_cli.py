import argparse
from pathlib import Path

import pytest

from amoeboid import cli
from amoeboid.paths import Paths

PATHS = Paths(
    Path("/h"), Path("/h/.config/amoeboid"), Path("/h/.local/share/amoeboid"), Path("/run/amoeboid")
)
COMMANDS = ["start", "restart", "ssh", "stop", "presets", "list", "config"]


def parse(*argv: str) -> argparse.Namespace:
    return cli.build_parser(PATHS).parse_args(list(argv))


@pytest.mark.parametrize(
    "argv", [["-h"], ["start", "-h"], ["start", "-n", "x"], ["stop", "-t", "1"]]
)
def test_no_short_options(argv: list[str], capsys: pytest.CaptureFixture[str]) -> None:
    with pytest.raises(SystemExit) as exit_:
        parse(*argv)
    assert exit_.value.code == 2
    assert "show this help" not in capsys.readouterr().out


@pytest.mark.parametrize(
    "argv", [["start", "--nam", "x"], ["start", "--det"], ["stop", "--time", "1"]]
)
def test_no_abbreviated_options(argv: list[str]) -> None:
    with pytest.raises(SystemExit) as exit_:
        parse(*argv)
    assert exit_.value.code == 2


@pytest.mark.parametrize("command", [[], *([c] for c in COMMANDS)])
def test_every_parser_has_long_help(command: list[str], capsys: pytest.CaptureFixture[str]) -> None:
    with pytest.raises(SystemExit) as exit_:
        parse(*command, "--help")
    assert exit_.value.code == 0
    assert "--help" in capsys.readouterr().out


def test_top_level_help_shows_this_users_file_locations(capsys: pytest.CaptureFixture[str]) -> None:
    with pytest.raises(SystemExit):
        parse("--help")
    out = capsys.readouterr().out
    assert "/h/.config/amoeboid/run_config.kdl" in out
    assert "/run/amoeboid/NAME/" in out
    for command in COMMANDS:
        assert f"    {command} " in out


def test_start_arguments() -> None:
    args = parse("start", "--name", "box", "--detach", "a", "b:ro")
    assert (args.name, args.detach, args.mounts) == ("box", True, ["a", "b:ro"])


def test_ssh_passes_everything_after_the_name_through() -> None:
    args = parse("ssh", "--name", "box", "--", "ls", "-la")
    assert args.subcommand == "ssh"
    assert args.command == ["--", "ls", "-la"]


@pytest.mark.parametrize("command", COMMANDS)
def test_every_command_dispatches(command: str) -> None:
    assert parse(command).subcommand in cli.COMMANDS


def test_bare_invocation_prints_help(capsys: pytest.CaptureFixture[str]) -> None:
    assert cli.main([]) == 0
    assert "commands:" in capsys.readouterr().out


@pytest.mark.parametrize(
    ("argv", "attr"),
    [
        (["start"], "boot_timeout"),
        (["stop"], "timeout"),
        (["restart"], "timeout"),
        (["restart"], "boot_timeout"),
    ],
)
def test_timeouts_default_to_the_config(argv: list[str], attr: str) -> None:
    assert getattr(parse(*argv), attr) is None
