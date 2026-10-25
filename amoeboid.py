#!/usr/bin/env python3
"""Run amoeboid straight from this checkout; safe to symlink into $PATH.

Outside the project's venv this re-executes itself through `uv run`, which creates or
refreshes the venv as needed. `uv tool install --editable .` (or `mise run install`)
gives a regular `amoeboid` command instead.
"""

import os
import shutil
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent


def main() -> int:
    if Path(sys.prefix).resolve() != (ROOT / ".venv").resolve():
        uv = shutil.which("uv")
        if uv is None:
            print("amoeboid: uv not found on PATH (https://docs.astral.sh/uv/)", file=sys.stderr)
            return 127
        argv = [uv, "run", "--quiet", "--project", str(ROOT), "python", str(ROOT / "amoeboid.py")]
        os.execv(uv, [*argv, *sys.argv[1:]])

    # This file shadows the `amoeboid` package while its directory is on sys.path.
    sys.path[:] = [p for p in sys.path if Path(p or ".").resolve() != ROOT]
    from amoeboid.cli import main as cli_main  # noqa: PLC0415

    return cli_main()


if __name__ == "__main__":
    raise SystemExit(main())
