"""Just enough QMP (qemu's JSON control protocol) to ask a guest to power off."""

import json
import socket
from collections.abc import Callable
from pathlib import Path

from amoeboid.errors import AppError, because, fail
from amoeboid.result import Ok, Result, attempt, railway


@railway
def powerdown(control_socket: Path) -> Result[None, AppError]:
    """Press the virtual power button; the guest shuts down cleanly and qemu exits."""

    def talk() -> list[dict[str, object]]:
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as sock:
            sock.settimeout(5)
            sock.connect(str(control_socket))
            stream = sock.makefile("rwb")
            replies: list[dict[str, object]] = [json.loads(stream.readline())]  # greeting
            for command in ("qmp_capabilities", "system_powerdown"):
                stream.write(json.dumps({"execute": command}).encode() + b"\n")
                stream.flush()
                replies.append(_reply(stream.readline, command))
            return replies

    replies = attempt(
        talk, because(f"can't talk to qemu on {control_socket}"), (OSError, ValueError)
    ).or_bail()
    if errors := [r["error"] for r in replies if "error" in r]:
        return fail(f"qemu refused to power down: {errors}")
    return Ok(None)


def _reply(readline: Callable[[], bytes], command: str) -> dict[str, object]:
    """Skip asynchronous events until the command's own reply arrives."""
    while True:
        line = readline()
        if not line:
            raise ValueError(f"qemu closed the connection waiting for {command}")
        message: dict[str, object] = json.loads(line)
        if "event" not in message:
            return message
