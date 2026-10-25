"""Pure construction of everything the launcher writes or runs; no effects here."""

import re
from collections.abc import Sequence
from dataclasses import dataclass
from pathlib import Path

from amoeboid.mounts import Mode, Mount

GUEST_MAC = "02:00:00:a0:0b:01"
GUEST_SSH_PORT = 22


@dataclass(frozen=True, slots=True)
class Share:
    """One virtiofsd instance: a host dir served on a socket in the runtime dir."""

    role: str
    socket: str
    source: Path
    mode: Mode


def nix_build_argv(flake: Path, host_module: Path, out_link: Path) -> list[str]:
    return [
        "nix",
        "--extra-experimental-features",
        "nix-command flakes",
        "build",
        f"path:{flake}",
        "--override-input",
        "host",
        f"path:{host_module}",
        "--no-write-lock-file",
        "--out-link",
        str(out_link),
    ]


def tag(index: int) -> str:
    return f"d{index}"


def shares(guest_state: Path, mounts: Sequence[Mount]) -> list[Share]:
    """Shares in socket order; names and tags must agree with base.nix."""
    fixed = [
        Share("virtiofsd-ro-store", "ro-store.sock", Path("/nix/store"), Mode.RO),
        Share("virtiofsd-state", "state.sock", guest_state, Mode.RW),
    ]
    user = [
        Share(f"virtiofsd-{tag(i)}", f"{tag(i)}.sock", m.path, m.mode)
        for i, m in enumerate(mounts, start=1)
    ]
    return fixed + user


def mounts_table(mounts: Sequence[Mount]) -> str:
    """The guest's `mounts` file: <tag>\\t<mode>\\t<path>, in mount order."""
    return "".join(f"{tag(i)}\t{m.mode}\t{m.path}\n" for i, m in enumerate(mounts, start=1))


def qemu_args(mounts: Sequence[Mount], ssh_port: int) -> str:
    """Extra qemu arguments, word-split by the runner script (so: no spaces inside)."""
    args = [
        "-netdev",
        f"user,id=net0,hostfwd=tcp:127.0.0.1:{ssh_port}-:{GUEST_SSH_PORT}",
        "-device",
        f"virtio-net-pci,netdev=net0,mac={GUEST_MAC}",
    ]
    for i in range(1, len(mounts) + 1):
        t = tag(i)
        args += [
            "-chardev",
            f"socket,id=amo{i},path={t}.sock",
            "-device",
            f"vhost-user-fs-pci,chardev=amo{i},tag={t}",
        ]
    return " ".join(args) + "\n"


def virtiofsd_argv(
    binary: Path,
    share: Share,
    runtime: Path,
    *,
    guest_uid: int,
    guest_gid: int,
    host_uid: int,
    host_gid: int,
) -> list[str]:
    """virtiofsd runs as the host user; the guest user's ids map onto it.

    So files the guest creates (even as root) are owned by the host user, and the host
    user's files appear in the guest as owned by the guest user.
    """
    argv = [
        str(binary),
        f"--socket-path={runtime / share.socket}",
        f"--shared-dir={share.source}",
        "--sandbox=namespace",
        f"--uid-map=:{guest_uid}:{host_uid}:1:",
        f"--gid-map=:{guest_gid}:{host_gid}:1:",
        "--log-level=error",
    ]
    if share.mode is Mode.RO:
        argv.append("--readonly")
    return argv


def ssh_argv(
    *, user: str, port: int, alias: str, known_hosts: Path, command: Sequence[str]
) -> list[str]:
    return [
        "ssh",
        "-p",
        str(port),
        "-o",
        f"HostKeyAlias={alias}",
        "-o",
        f"UserKnownHostsFile={known_hosts}",
        "-o",
        "StrictHostKeyChecking=yes",
        "-o",
        "LogLevel=ERROR",
        # Whatever ~/.ssh/config says: nothing of the host's reaches the VM through ssh.
        "-o",
        "ForwardAgent=no",
        "-o",
        "ForwardX11=no",
        "-o",
        "GSSAPIDelegateCredentials=no",
        "-o",
        "PermitLocalCommand=no",
        f"{user}@127.0.0.1",
        *command,
    ]


# Escape sequences a guest could use against the host terminal (OSC 52 clipboard writes,
# title changes, device queries, ...). Colour/style (CSI ... m) is all that is kept.
_ESCAPES = re.compile(
    rb"\x1b\[[0-?]*[ -/]*[@-~]"  # CSI
    rb"|\x1b[\]P^_X][^\x07\x1b]*(?:\x07|\x1b\\)?"  # OSC, DCS, PM, APC, SOS
    rb"|\x1b[ -/]*[0-~]?"  # other ESC sequences, or a stray ESC
)
_CONTROLS = re.compile(rb"[\x00-\x08\x0b-\x1f\x7f]")  # all C0 but \t and \n


def printable_console(chunk: bytes) -> bytes:
    """Guest console output made safe to copy to the host terminal."""
    kept = _ESCAPES.sub(
        lambda m: m[0] if m[0].startswith(b"\x1b[") and m[0].endswith(b"m") else b"", chunk
    )
    return _CONTROLS.sub(lambda m: m[0] if m[0] == b"\x1b" else b"", kept)
