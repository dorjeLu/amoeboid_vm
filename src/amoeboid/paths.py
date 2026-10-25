"""Where amoeboid keeps things.

Every location derives from $HOME, the XDG base-directory variables, or the temp dir,
and is absolute. Nothing about the invoking user is hardcoded.
"""

import re
from collections.abc import Mapping
from dataclasses import dataclass
from pathlib import Path

from amoeboid.errors import AppError, fail
from amoeboid.result import Ok, Result

BUNDLED_NIX = Path(__file__).resolve().parent / "nix"
"""The NixOS configuration shipped with amoeboid; seeds the `default` preset."""
HOST_TEMPLATE = Path(__file__).resolve().parent / "host.nix.template"

CONFIG_FILE = "run_config.kdl"
DEFAULT_VM = "default"

_VM_NAME = re.compile(r"[A-Za-z0-9][A-Za-z0-9-]{0,62}")


def is_path_spec(spec: str) -> bool:
    """`--base-nix-config` takes a preset name, or a path if it looks like one."""
    return "/" in spec or spec.startswith(("~", "."))


def absolute(path: str | Path, base: Path) -> Path:
    """`path` with `~` expanded, made absolute against `base`, and symlink-free."""
    return (base / Path(path).expanduser()).resolve()


@dataclass(frozen=True, slots=True)
class VmPaths:
    """Files belonging to one named VM."""

    name: str
    state: Path
    """Persistent: survives VM restarts and host reboots."""
    runtime: Path
    """Ephemeral (tmpfs): sockets, pid files, the qemu argument file."""

    @property
    def build_link(self) -> Path:
        return self.state / "vm"

    @property
    def guest(self) -> Path:
        """Shared into the VM at /mnt/amoeboid; the guest can write anything in here."""
        return self.state / "guest"

    @property
    def nix_config(self) -> Path:
        """This VM's own copy of its NixOS configuration (host-owned); what gets built."""
        return self.state / "nix"

    @property
    def last_mounts(self) -> Path:
        """What the last start shared; outside the guest's reach, unlike guest/mounts."""
        return self.state / "mounts"

    @property
    def ssh_dir(self) -> Path:
        return self.state / "ssh"

    @property
    def host_key(self) -> Path:
        return self.ssh_dir / "ssh_host_ed25519_key"

    @property
    def known_hosts(self) -> Path:
        return self.ssh_dir / "known_hosts"

    @property
    def console_log(self) -> Path:
        return self.state / "console.log"

    @property
    def qemu_args(self) -> Path:
        return self.runtime / "amoeboid-qemu-args"

    @property
    def ssh_port(self) -> Path:
        return self.runtime / "ssh-port"

    @property
    def control_socket(self) -> Path:
        """QMP socket; the name must match `microvm.socket` in base.nix."""
        return self.runtime / "control.sock"

    def pid_file(self, role: str) -> Path:
        return self.runtime / f"{role}.pid"


@dataclass(frozen=True, slots=True)
class Paths:
    home: Path
    config_dir: Path
    data_dir: Path
    runtime_dir: Path

    @property
    def config_file(self) -> Path:
        return self.config_dir / CONFIG_FILE

    @property
    def presets_dir(self) -> Path:
        return self.config_dir / "nix_presets"

    def host_module(self, user: str) -> Path:
        return self.config_dir / f"host_{user}.nix"

    def vm(self, name: str) -> VmPaths:
        return VmPaths(name, self.data_dir / name, self.runtime_dir / name)


def resolve_paths(env: Mapping[str, str], uid: int) -> Result[Paths, AppError]:
    """Locate everything from the environment (pure, apart from resolving symlinks)."""
    home_var = env.get("HOME", "")
    if not Path(home_var).is_absolute():
        return fail("$HOME is unset or not an absolute path", "run amoeboid from a login session")
    home = Path(home_var).resolve()

    def xdg(var: str, fallback: Path) -> Path:
        # The XDG spec says relative values are invalid and must be ignored.
        value = env.get(var, "")
        return Path(value).resolve() if Path(value).is_absolute() else fallback

    tmp = xdg("TMPDIR", Path("/tmp"))
    return Ok(
        Paths(
            home=home,
            config_dir=xdg("XDG_CONFIG_HOME", home / ".config") / "amoeboid",
            data_dir=xdg("XDG_DATA_HOME", home / ".local" / "share") / "amoeboid",
            runtime_dir=xdg("XDG_RUNTIME_DIR", tmp / f"amoeboid-{uid}") / "amoeboid",
        )
    )


def vm_name(raw: str) -> Result[str, AppError]:
    """VM names double as guest hostnames, so they follow hostname rules."""
    if _VM_NAME.fullmatch(raw):
        return Ok(raw)
    return fail(
        f"bad VM name {raw!r}",
        "use letters, digits and '-', starting with a letter or digit (max 63 chars)",
    )
