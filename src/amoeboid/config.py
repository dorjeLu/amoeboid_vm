"""The run configuration: `run_config.kdl` <-> `RunConfig`.

The file is read once per invocation and never hot-reloaded. amoeboid writes it back
only when it changes a setting itself (first run, `start --save-mounts`, accepting the
offer to save prompted mounts), always as a whole, atomically, in canonical form.
"""

import os
import pwd
import re
from collections.abc import Callable
from dataclasses import dataclass, field, replace
from pathlib import Path

from amoeboid.errors import AppError, because, fail
from amoeboid.kdl_serde import Document, KdlSerde, Value
from amoeboid.mounts import Mode, Mount
from amoeboid.paths import BUNDLED_NIX, Paths, absolute
from amoeboid.result import Err, Ok, Result, attempt, railway, traverse

HOST_TEMPLATE = BUNDLED_NIX / "host.nix.template"

HEADER = """\
// amoeboid run configuration (KDL v2). Read once per command, never hot-reloaded.
// amoeboid rewrites this file when it saves a setting itself, keeping only this header
// comment. Run `amoeboid config --help` for what each key means.
"""


@dataclass(frozen=True, slots=True)
class SshConfig:
    port: int = 0
    """Host port forwarded to the guest's sshd, on 127.0.0.1. 0 picks a free one per start."""


@dataclass(frozen=True, slots=True)
class RunConfig:
    user: str
    """Guest user name; also names the rendered host module (host_<user>.nix)."""
    uid: int
    gid: int
    recipe: Path | None = None
    """NixOS module describing the VM's contents; None means the bundled recipe."""
    default_mounts: tuple[Mount, ...] = ()
    """Shared when `start` is given no mounts."""
    home_files: tuple[str, ...] = ()
    """Paths relative to $HOME copied into the guest's home on every start."""
    ssh: SshConfig = field(default_factory=SshConfig)


@dataclass(frozen=True, slots=True)
class Identity:
    """Who is running amoeboid; seeds a fresh configuration."""

    user: str
    uid: int
    gid: int

    @staticmethod
    def of_process() -> Identity:
        uid = os.getuid()
        return Identity(pwd.getpwuid(uid).pw_name, uid, os.getgid())


def initial_config(identity: Identity, home: Path) -> RunConfig:
    """A fresh configuration for `identity`. Mounts are left for the user to choose."""
    candidates = (".gitconfig", ".config/git/config")
    return RunConfig(
        user=identity.user,
        uid=identity.uid,
        gid=identity.gid,
        home_files=tuple(f for f in candidates if (home / f).is_file()),
    )


# -- RunConfig -> Document --------------------------------------------------------------


def to_document(cfg: RunConfig) -> Document:
    mounts: list[Value] = [{"path": str(m.path), "mode": str(m.mode)} for m in cfg.default_mounts]
    home_files: list[Value] = list(cfg.home_files)
    return {
        "user": cfg.user,
        "uid": cfg.uid,
        "gid": cfg.gid,
        "recipe": None if cfg.recipe is None else str(cfg.recipe),
        "default-mounts": mounts,
        "home-files": home_files,
        "ssh": {"port": cfg.ssh.port},
    }


# -- Document -> RunConfig --------------------------------------------------------------

type Decoder[T] = Callable[[Value], Result[T, str]]

_USER = re.compile(r"[a-z_][a-z0-9_-]{0,31}")


def from_document(doc: Document, base: Path) -> Result[RunConfig, AppError]:
    """Decode and validate, reporting every problem at once. Relative paths use `base`."""
    results = (
        _required(doc, "user", _user),
        _required(doc, "uid", _int_in(1, 2**32 - 2)),
        _required(doc, "gid", _int_in(1, 2**32 - 2)),
        _optional(doc, "recipe", _nullable(_path(base)), None),
        _optional(doc, "default-mounts", _list_of(_mount(base)), []),
        _optional(doc, "home-files", _list_of(_home_file), []),
        _optional(doc, "ssh", _ssh, SshConfig()),
        _known_keys(doc, {"user", "uid", "gid", "recipe", "default-mounts", "home-files", "ssh"}),
    )
    match results:
        case (Ok(user), Ok(uid), Ok(gid), Ok(recipe), Ok(mounts), Ok(home_files), Ok(ssh), Ok()):
            return Ok(RunConfig(user, uid, gid, recipe, tuple(mounts), tuple(home_files), ssh))
        case _:
            problems = [r.error for r in results if isinstance(r, Err)]
            return fail("invalid configuration:\n" + "\n".join(f"  {p}" for p in problems))


def _required[T](doc: Document, key: str, decode: Decoder[T]) -> Result[T, str]:
    if key not in doc:
        return Err(f"{key}: missing")
    return decode(doc[key]).map_err(lambda e: f"{key}: {e}")


def _optional[T](doc: Document, key: str, decode: Decoder[T], default: T) -> Result[T, str]:
    return _required(doc, key, decode) if key in doc else Ok(default)


def _known_keys(doc: Document, known: set[str]) -> Result[None, str]:
    unknown = [k for k in doc if k not in known]
    if unknown:
        return Err(f"unknown key(s) {', '.join(unknown)}; known: {', '.join(sorted(known))}")
    return Ok(None)


def _int_in(lo: int, hi: int) -> Decoder[int]:
    def decode(v: Value) -> Result[int, str]:
        match v:
            case bool() | None | float() | str() | list() | dict():
                return Err(f"expected an integer, got {v!r}")
            case int() if lo <= v <= hi:
                return Ok(v)
            case int():
                return Err(f"{v} is outside {lo}..{hi}")

    return decode


def _str(v: Value) -> Result[str, str]:
    return Ok(v) if isinstance(v, str) else Err(f"expected a string, got {v!r}")


def _user(v: Value) -> Result[str, str]:
    def check(name: str) -> Result[str, str]:
        if name == "root" or not _USER.fullmatch(name):
            return Err(f"{name!r} isn't a usable account name")
        return Ok(name)

    return _str(v).bind(check)


def _path(base: Path) -> Decoder[Path]:
    return lambda v: _str(v).map(lambda s: absolute(s, base))


def _nullable[T](decode: Decoder[T]) -> Decoder[T | None]:
    return lambda v: Ok(None) if v is None else decode(v)


def _list_of[T](decode: Decoder[T]) -> Decoder[list[T]]:
    def decode_list(v: Value) -> Result[list[T], str]:
        if not isinstance(v, list):
            return Err(f"expected a (list) node, got {v!r}")
        return traverse(
            enumerate(v), lambda iv: decode(iv[1]).map_err(lambda e: f"item {iv[0]}: {e}")
        )

    return decode_list


def _mount(base: Path) -> Decoder[Mount]:
    def decode(v: Value) -> Result[Mount, str]:
        if not isinstance(v, dict):
            return Err('expected { path "..."; mode "rw"|"ro" }')
        mode = v.get("mode", Mode.RW.value)
        if mode not in tuple(Mode):
            return Err(f"mode must be one of {', '.join(Mode)}, got {mode!r}")
        if extra := set(v) - {"path", "mode"}:
            return Err(f"unknown key(s) {', '.join(sorted(extra))}")
        return _required(v, "path", _path(base)).map(lambda p: Mount(p, Mode(str(mode))))

    return decode


def _home_file(v: Value) -> Result[str, str]:
    def check(s: str) -> Result[str, str]:
        p = Path(s)
        if not s or p.is_absolute() or ".." in p.parts or s.startswith("~"):
            return Err(f"{s!r} must be a path relative to $HOME, without '..'")
        return Ok(p.as_posix())

    return _str(v).bind(check)


def _ssh(v: Value) -> Result[SshConfig, str]:
    if not isinstance(v, dict):
        return Err("expected a block, e.g. ssh { port 0 }")
    match (_optional(v, "port", _port, 0), _known_keys(v, {"port"})):
        case (Ok(port), Ok()):
            return Ok(SshConfig(port))
        case (Err(e), _) | (_, Err(e)):
            return Err(e)


def _port(v: Value) -> Result[int, str]:
    def check(p: int) -> Result[int, str]:
        return Ok(p) if p == 0 or p >= 1024 else Err("use 0 (pick a free port) or 1024..65535")

    return _int_in(0, 65535)(v).bind(check)


# -- text and files ---------------------------------------------------------------------


def render(cfg: RunConfig) -> Result[str, AppError]:
    match KdlSerde.dumps(to_document(cfg)):
        case Ok(text):
            return Ok(HEADER + "\n" + text)
        case Err(error):
            return fail(f"can't render configuration: {error}")


def parse(text: str, base: Path) -> Result[RunConfig, AppError]:
    return KdlSerde.loads(text).map_err(AppError).bind(lambda doc: from_document(doc, base))


@railway
def load(paths: Paths) -> Result[RunConfig, AppError]:
    file = paths.config_file
    text = attempt(file.read_text, because(f"can't read {file}")).or_bail()
    return parse(text, paths.config_dir).map_err(
        lambda e: AppError(f"{file}: {e.message}", "fix the file, or delete it to start over")
    )


@railway
def save(paths: Paths, cfg: RunConfig) -> Result[None, AppError]:
    """Replace the config file atomically: readers see the old file or the new one."""
    text = render(cfg).or_bail()
    file = paths.config_file
    tmp = file.with_name(f".{file.name}.{os.getpid()}.tmp")

    def write() -> None:
        file.parent.mkdir(parents=True, exist_ok=True)
        tmp.write_text(text)
        tmp.replace(file)

    return attempt(write, because(f"can't write {file}"))


@railway
def ensure_host_module(paths: Paths, cfg: RunConfig) -> Result[Path, AppError]:
    """Render host_<user>.nix from the bundled template unless it already exists.

    The rendered file is the user's to edit (e.g. to set microvm.vcpu/mem); amoeboid
    never overwrites it.
    """
    target = paths.host_module(cfg.user)
    if target.exists():
        return Ok(target)
    template = attempt(HOST_TEMPLATE.read_text, because("can't read host template")).or_bail()
    text = (
        template.replace("@user@", cfg.user)
        .replace("@uid@", str(cfg.uid))
        .replace("@gid@", str(cfg.gid))
    )

    def write() -> Path:
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(text)
        return target

    return attempt(write, because(f"can't write {target}"))


@dataclass(frozen=True, slots=True)
class Loaded:
    config: RunConfig
    created: bool


@railway
def load_or_create(paths: Paths, identity: Identity) -> Result[Loaded, AppError]:
    """Load the configuration, writing a fresh one (and the host module) on first run."""
    if paths.config_file.exists():
        cfg = load(paths).or_bail()
        ensure_host_module(paths, cfg).or_bail()
        return Ok(Loaded(cfg, created=False))
    cfg = initial_config(identity, paths.home)
    save(paths, cfg).or_bail()
    ensure_host_module(paths, cfg).or_bail()
    return Ok(Loaded(cfg, created=True))


def with_default_mounts(cfg: RunConfig, mounts: list[Mount]) -> RunConfig:
    return replace(cfg, default_mounts=tuple(mounts))
