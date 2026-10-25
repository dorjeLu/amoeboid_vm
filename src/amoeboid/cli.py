"""Command-line interface. Long options only; abbreviations are never accepted."""

import argparse
import logging
import os
import signal
import sys
from collections.abc import Sequence
from pathlib import Path
from typing import NoReturn, TypedDict

from amoeboid import config, plan, ssh, vm
from amoeboid.config import Identity, RunConfig
from amoeboid.errors import AppError, fail
from amoeboid.mounts import SPEC_HELP, Mount, parse_spec, validate
from amoeboid.paths import BUNDLED_NIX, DEFAULT_VM, Paths, absolute, resolve_paths, vm_name
from amoeboid.result import Err, Ok, Result, attempt, railway, traverse

log = logging.getLogger("amoeboid")

PROG = "amoeboid"


# -- parser -----------------------------------------------------------------------------


class _HelpFormatter(argparse.RawDescriptionHelpFormatter):
    """Keeps descriptions as written; lists subcommands without a bare COMMAND line."""

    def _format_action(self, action: argparse.Action) -> str:
        text = super()._format_action(action)
        if action.nargs == argparse.PARSER:
            text = text.split("\n", 1)[1]
        return text


class _ParserOptions(TypedDict):
    formatter_class: type[argparse.HelpFormatter]
    add_help: bool
    allow_abbrev: bool
    suggest_on_error: bool


# Every parser: no automatic -h, no prefix matching of long options, typo suggestions.
PARSER_DEFAULTS: _ParserOptions = {
    "formatter_class": _HelpFormatter,
    "add_help": False,
    "allow_abbrev": False,
    "suggest_on_error": True,
}


def _help_option(p: argparse.ArgumentParser) -> None:
    p.add_argument("--help", action="help", help="show this help and exit")


def _name_option(p: argparse.ArgumentParser) -> None:
    p.add_argument(
        "--name",
        default=DEFAULT_VM,
        metavar="NAME",
        help=f"which VM (default: {DEFAULT_VM}); also its hostname. Each name has its own "
        "persistent home directory, and different names can run side by side",
    )


def build_parser(paths: Paths | None) -> argparse.ArgumentParser:
    where = _locations(paths)
    root = argparse.ArgumentParser(
        prog=PROG,
        **PARSER_DEFAULTS,
        description=(
            "Boot NixOS microVMs that share chosen host directories at their own paths,\n"
            "so coding agents can work on them without reaching the rest of the machine.\n"
            "\n"
            "A VM's contents come from a recipe (a NixOS module); it gets outbound network,\n"
            "a persistent home directory, ssh on a loopback port, and the shared dirs,\n"
            "each read-write or read-only."
        ),
        epilog=(
            "typical session:\n"
            "  amoeboid start --detach ~/dev/app ~/dev/libs:ro   # build, boot, wait for ssh\n"
            "  amoeboid ssh                                       # work inside\n"
            "  amoeboid stop\n"
            "\n"
            f"files:\n{where}\n"
            "\n"
            f"Run `{PROG} COMMAND --help` for each command's options."
        ),
    )
    _help_option(root)
    commands = root.add_subparsers(dest="subcommand", metavar="COMMAND", title="commands")
    commands.required = True

    def sub(name: str, summary: str, description: str, epilog: str = "") -> argparse.ArgumentParser:
        p = commands.add_parser(
            name,
            help=summary,
            description=description,
            epilog=epilog,
            **PARSER_DEFAULTS,
        )
        _help_option(p)
        return p

    start = sub(
        "start",
        "build and boot a VM",
        description=(
            "Build the VM from its recipe (fast when nothing changed), share the given\n"
            "directories at the same paths inside it, and boot it.\n"
            "\n"
            "By default the VM's serial console takes over this terminal until the guest\n"
            "powers off. With --detach, the boot log is shown until sshd answers, then\n"
            "amoeboid exits and the VM keeps running in the background."
        ),
        epilog=(
            "mounts:\n"
            f"  Each MOUNT is {SPEC_HELP}.\n"
            "  Relative paths are resolved against the current directory. Without any\n"
            "  MOUNT, `default-mounts` from the config file is used; if that is empty too,\n"
            "  amoeboid asks for them (and offers to save them as the default).\n"
            "  Refused: a directory given twice, and /, /home or the home directory itself,\n"
            "  which would hide the VM's own home. Nested shares are fine.\n"
            "\n"
            "examples:\n"
            "  amoeboid start ~/dev/app\n"
            "  amoeboid start --name scratch --detach . ~/dev/shared:ro\n"
            "  amoeboid start --recipe ./my-recipe.nix --save-mounts ~/dev"
        ),
    )
    _name_option(start)
    start.add_argument(
        "--detach",
        action="store_true",
        help="boot in the background; return once ssh works (console log: "
        "<data dir>/NAME/console.log)",
    )
    start.add_argument(
        "--recipe",
        metavar="PATH",
        help="NixOS module (file, or directory with default.nix) for this start; "
        "default: `recipe` from the config, else the bundled recipe",
    )
    start.add_argument(
        "--save-mounts",
        action="store_true",
        help="also store the given MOUNTs as `default-mounts` in the config file",
    )
    start.add_argument(
        "--boot-timeout",
        type=float,
        default=180.0,
        metavar="SECONDS",
        help="with --detach: how long to wait for sshd (default: 180)",
    )
    start.add_argument("mounts", nargs="*", metavar="MOUNT", help="directory to share; see below")

    ssh_cmd = sub(
        "ssh",
        "open a shell (or run a command) in a running VM",
        description=(
            "ssh into a running VM as the guest user. The VM's host key is pinned per VM,\n"
            "so there are no first-connection prompts. No credentials are needed: the port\n"
            "is only reachable from this machine's loopback."
        ),
        epilog=(
            "examples:\n"
            "  amoeboid ssh\n"
            "  amoeboid ssh --name scratch -- cargo test\n"
            "  amoeboid ssh -- -L 8080:localhost:8080   # extra ssh options go after --"
        ),
    )
    _name_option(ssh_cmd)
    ssh_cmd.add_argument(
        "command",
        nargs=argparse.REMAINDER,
        metavar="-- ARGS",
        help="passed to ssh after the destination: a command to run, or ssh options",
    )

    stop = sub(
        "stop",
        "shut a running VM down",
        description=(
            "Ask the guest to power off (ACPI power button), wait for it, then stop the\n"
            "helper processes. If the guest doesn't power off in time, qemu is killed.\n"
            "Running `poweroff` inside the VM has the same effect."
        ),
    )
    _name_option(stop)
    stop.add_argument(
        "--timeout",
        type=float,
        default=60.0,
        metavar="SECONDS",
        help="how long the guest gets to power off (default: 60)",
    )

    sub("list", "show VMs, whether they run, and what they share", description="List known VMs.")

    cfg = sub(
        "config",
        "show the configuration file",
        description=(
            "Print the configuration (creating it on first use) in canonical form.\n"
            "The file is KDL v2. amoeboid reads it once per command and only rewrites it\n"
            "when it saves a setting itself (first run, --save-mounts, saving prompted\n"
            "mounts); comments other than its header are not kept then."
        ),
        epilog=(
            "keys:\n"
            "  user, uid, gid     guest account; seeded from the user running amoeboid\n"
            "  recipe             NixOS module for the VM, or #null for the bundled one\n"
            "  default-mounts     shared when `start` gets no MOUNT; a (list) of\n"
            '                     { path "/abs/dir"; mode "rw" | "ro" }\n'
            "  home-files         files under $HOME copied into the VM's home on every\n"
            '                     start, as a (list) of relative paths, e.g. ".gitconfig"\n'
            "  ssh { port N }     loopback port for ssh; 0 picks a free one each start\n"
            "\n"
            "  Guest user and per-machine NixOS settings (vcpu, memory) live in\n"
            "  host_<user>.nix next to the config file, rendered once from a template."
        ),
    )
    cfg.add_argument("--path", action="store_true", help="print only the file's location")
    return root


def _locations(paths: Paths | None) -> str:
    if paths is None:
        return "  (unavailable: $HOME is not set)"
    return (
        f"  config      {paths.config_file}\n"
        f"  host module {paths.config_dir}/host_<user>.nix\n"
        f"  VM state    {paths.data_dir}/NAME/  (guest/home is the VM's home)\n"
        f"  runtime     {paths.runtime_dir}/NAME/  (sockets, pid files)\n"
        f"  recipe      {BUNDLED_NIX / 'recipe.nix'}  (bundled default)"
    )


# -- commands ---------------------------------------------------------------------------


@railway
def cmd_start(args: argparse.Namespace, paths: Paths) -> Result[int, AppError]:
    name = vm_name(args.name).or_bail()
    cfg = _load(paths).or_bail()
    cwd = Path.cwd()
    protected = [Path("/home") / cfg.user, paths.home]

    specs: list[str] = args.mounts
    prompted = False
    if specs:
        mounts = traverse(specs, lambda s: parse_spec(s, cwd)).or_bail()
    elif cfg.default_mounts:
        mounts = list(cfg.default_mounts)
    else:
        mounts = prompt_mounts(paths, cwd, protected).or_bail()
        prompted = True
    mounts = validate(mounts, protected, Path.is_dir).or_bail()

    if args.save_mounts or (
        prompted and _confirm(f"Save these as default-mounts in {paths.config_file}?")
    ):
        config.save(paths, config.with_default_mounts(cfg, mounts)).or_bail()
        log.info("saved default-mounts to %s", paths.config_file)

    recipe = _recipe(args.recipe, cfg, cwd).or_bail()
    request = vm.StartRequest(
        vm=paths.vm(name),
        home=paths.home,
        config=cfg,
        host_module=paths.host_module(cfg.user),
        recipe=recipe,
        mounts=tuple(mounts),
        detach=args.detach,
        boot_timeout=args.boot_timeout,
    )
    return vm.start(request)


@railway
def cmd_ssh(args: argparse.Namespace, paths: Paths) -> Result[int, AppError]:
    name = vm_name(args.name).or_bail()
    cfg = _load(paths).or_bail()
    target = paths.vm(name)
    port = vm.ssh_port(target).or_bail()
    extra: list[str] = args.command
    if extra[:1] == ["--"]:
        extra = extra[1:]
    argv = plan.ssh_argv(
        user=cfg.user,
        port=port,
        alias=ssh.alias(target),
        known_hosts=target.known_hosts,
        command=extra,
    )
    return attempt(lambda: _exec(argv), lambda e: AppError(f"can't run ssh: {e}"))


@railway
def cmd_stop(args: argparse.Namespace, paths: Paths) -> Result[int, AppError]:
    name = vm_name(args.name).or_bail()
    return vm.stop(paths.vm(name), args.timeout)


def cmd_list(_args: argparse.Namespace, paths: Paths) -> Result[int, AppError]:
    rows = vm.statuses(paths)
    if not rows:
        log.info("no VMs yet; create one with `amoeboid start`")
        return Ok(0)
    width = max(len(r.name) for r in rows)
    for r in rows:
        state = f"running  ssh 127.0.0.1:{r.ssh_port}" if r.running else "stopped"
        shares = ", ".join(m.spec() for m in r.mounts) or "-"
        print(f"{r.name:<{width}}  {state:<28}  {shares}")
    return Ok(0)


@railway
def cmd_config(args: argparse.Namespace, paths: Paths) -> Result[int, AppError]:
    cfg = _load(paths).or_bail()
    if args.path:
        print(paths.config_file)
        return Ok(0)
    print(f"// {paths.config_file}")
    print(config.render(cfg).or_bail(), end="")
    return Ok(0)


COMMANDS = {
    "start": cmd_start,
    "ssh": cmd_ssh,
    "stop": cmd_stop,
    "list": cmd_list,
    "config": cmd_config,
}


# -- helpers ----------------------------------------------------------------------------


@railway
def _load(paths: Paths) -> Result[RunConfig, AppError]:
    loaded = config.load_or_create(paths, Identity.of_process()).or_bail()
    if loaded.created:
        log.info("first run: wrote %s", paths.config_file)
        log.info("           and %s", paths.host_module(loaded.config.user))
    return Ok(loaded.config)


def _recipe(flag: str | None, cfg: RunConfig, cwd: Path) -> Result[Path, AppError]:
    recipe = absolute(flag, cwd) if flag else (cfg.recipe or BUNDLED_NIX / "recipe.nix")
    if not recipe.exists():
        return fail(f"no such recipe: {recipe}")
    return Ok(recipe)


def prompt_mounts(paths: Paths, cwd: Path, protected: list[Path]) -> Result[list[Mount], AppError]:
    """Ask for mounts interactively, checking each entry as it is typed."""
    if not sys.stdin.isatty():
        return fail(
            "no directories to share",
            f"pass them as MOUNT arguments, or set default-mounts in {paths.config_file}",
        )
    print(
        "No MOUNT given and no default-mounts configured.\n"
        f"Directories to share, one per line, as {SPEC_HELP}.\n"
        "Finish with an empty line.",
        file=sys.stderr,
    )
    chosen: list[Mount] = []
    while True:
        try:
            line = input("  mount> ").strip()
        except EOFError:
            line = ""
        except KeyboardInterrupt:
            print(file=sys.stderr)
            return fail("cancelled")
        if not line:
            if chosen:
                return Ok(chosen)
            return fail("nothing to share", "give at least one directory")
        match _accept(line, chosen, cwd, protected):
            case Ok(mount):
                chosen.append(mount)
            case Err(error):
                print(f"  {error.message.splitlines()[-1].strip()}", file=sys.stderr)


@railway
def _accept(
    line: str, chosen: list[Mount], cwd: Path, protected: list[Path]
) -> Result[Mount, AppError]:
    mount = parse_spec(line, cwd).or_bail()
    validate([*chosen, mount], protected, Path.is_dir).or_bail()
    return Ok(mount)


def _confirm(question: str) -> bool:
    try:
        return input(f"{question} [y/N] ").strip().lower() in {"y", "yes"}
    except EOFError, KeyboardInterrupt:
        return False


def _exec(argv: list[str]) -> NoReturn:
    sys.stdout.flush()
    sys.stderr.flush()
    os.execvp(argv[0], argv)


class _Formatter(logging.Formatter):
    def format(self, record: logging.LogRecord) -> str:
        level = "" if record.levelno < logging.WARNING else f"{record.levelname.lower()}: "
        return f"{PROG}: {level}{record.getMessage()}"


def _setup_logging() -> None:
    handler = logging.StreamHandler(sys.stderr)
    handler.setFormatter(_Formatter())
    root = logging.getLogger("amoeboid")
    root.handlers[:] = [handler]
    root.setLevel(logging.INFO)
    root.propagate = False


def _exit_on_signal(signum: int, _frame: object) -> None:
    # Unwinds like any exception, so started helpers are cleaned up.
    raise SystemExit(128 + signum)


def main(argv: Sequence[str] | None = None) -> int:
    _setup_logging()
    for sig in (signal.SIGTERM, signal.SIGHUP):
        signal.signal(sig, _exit_on_signal)
    resolved = resolve_paths(os.environ, os.getuid())
    parser = build_parser(resolved.unwrap_or(None))
    if not (sys.argv[1:] if argv is None else argv):
        parser.print_help()
        return 0
    args = parser.parse_args(argv)
    command = COMMANDS[args.subcommand]
    match resolved.bind(lambda paths: command(args, paths)):
        case Ok(code):
            return code
        case Err(error):
            log.error("%s", error.render())
            return 1
