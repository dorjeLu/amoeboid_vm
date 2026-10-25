"""The guest's ssh identity, as pinned by the host, and talking to its sshd."""

import socket
import subprocess
import time
from collections.abc import Callable

from amoeboid.errors import AppError, because, fail
from amoeboid.paths import VmPaths
from amoeboid.result import Ok, Result, attempt, railway


def alias(vm: VmPaths) -> str:
    """Name the host key is pinned under, whatever port the VM gets."""
    return f"amoeboid-{vm.name}"


@railway
def ensure_host_key(vm: VmPaths) -> Result[None, AppError]:
    """Generate the guest's host key once per VM and pin it in the VM's known_hosts.

    The key lives outside the guest-writable share and is copied in on every start,
    so the guest's identity survives its tmpfs root and can't be swapped by the guest.
    """
    if not vm.host_key.exists():
        attempt(
            lambda: vm.ssh_dir.mkdir(mode=0o700, parents=True, exist_ok=True),
            because(f"can't create {vm.ssh_dir}"),
        ).or_bail()
        keygen = ["ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-C", alias(vm)]
        run = attempt(
            lambda: subprocess.run(
                [*keygen, "-f", str(vm.host_key)], capture_output=True, text=True, check=False
            ),
            because("can't run ssh-keygen", "install OpenSSH on the host"),
        ).or_bail()
        if run.returncode != 0:
            return fail(f"ssh-keygen failed: {run.stderr.strip()}")
    public = attempt(
        vm.host_key.with_suffix(".pub").read_text, because("can't read the VM's public host key")
    ).or_bail()
    key_type, key = public.split()[:2]
    line = f"{alias(vm)} {key_type} {key}\n"

    def pin() -> None:
        vm.known_hosts.write_text(line)

    return attempt(pin, because(f"can't write {vm.known_hosts}"))


def free_port() -> Result[int, AppError]:
    """A currently unused loopback port (the kernel's pick; racy but fine locally)."""

    def pick() -> int:
        with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
            s.bind(("127.0.0.1", 0))
            port: int = s.getsockname()[1]
            return port

    return attempt(pick, because("can't find a free port"))


def sshd_ready(port: int) -> bool:
    """True once the guest's sshd answers with its banner.

    qemu accepts on the forwarded port as soon as it starts, so a successful connect
    alone means nothing; only the banner proves sshd is up.
    """
    try:
        with socket.create_connection(("127.0.0.1", port), timeout=2) as conn:
            conn.settimeout(2)
            return conn.recv(8).startswith(b"SSH-")
    except OSError:
        return False


def wait_for_sshd(
    port: int, timeout: float, still_booting: Callable[[], bool], tick: Callable[[], None]
) -> Result[None, AppError]:
    """Poll until sshd answers; give up if the VM dies or `timeout` passes."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        tick()
        if sshd_ready(port):
            return Ok(None)
        if not still_booting():
            return fail("the VM exited before sshd came up")
        time.sleep(0.5)
    return fail(f"sshd didn't come up within {timeout:.0f}s", "see the console log for why")
