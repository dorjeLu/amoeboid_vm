# AGENTS.md

amoeboid: host-side Python 3.14 CLI that builds and runs NixOS microVMs (microvm.nix, qemu, one virtiofsd per shared dir) for running coding agents against chosen host directories. Operator behaviour: `README.adoc`. Architecture and rationale: `DEVELOPING.adoc` (read the relevant section before non-trivial changes).

## Commands

```fish
mise run test           # pytest + hypothesis (200 examples)
mise run test-thorough  # 5000 examples
mise run lint           # ruff check, ruff format --check, basedpyright strict — must be clean
mise run fmt
uv run amoeboid --help  # run from checkout
```

## Layout

`src/amoeboid/`, dependencies point downward only:

- core: `result` (Ok/Err, `@railway` + `.or_bail()`, `attempt`, `traverse[_all]`, `ensure`), `errors` (`AppError(message, hint)`, `fail`, `because`)
- pure: `kdl_serde` (canonical KDL v2 <-> Value bijection), `paths` (`Paths`/`VmPaths`, all locations), `mounts` (parse/validate shares), `plan` (argv, qemu args, mount table, console sanitising)
- `config`: `run_config.kdl` <-> frozen `RunConfig`; decoders report all problems at once; atomic whole-file save
- effects: `guestfs` (no-follow writes into guest-writable dirs), `procs` (pid files, /proc exe liveness), `ssh` (host key pin, port, banner probe), `qmp` (powerdown), `nixconfig` (presets, per-VM config copy, guest copies, `host.nix`)
- `vm`: start/stop/status orchestration, `ExitStack` process bracketing
- `cli`: argparse tree, `cmd_*(args, paths) -> Result[int, AppError]` registered in `COMMANDS`, only place errors are printed
- `nix/`: bundled flake. `base.nix` = plumbing coupled to Python; `recipe.nix` = packages/sizing; `host` input overridden per start with `<data>/NAME/host.nix`

## Invariants (do not break)

- Guest is untrusted. Everything under `<data>/NAME/guest/` is guest-writable: write there only via `guestfs` (never `Path.write_text`), never read it back to decide anything. Keep host-owned records instead (e.g. `<data>/NAME/mounts`).
- Nothing that varies per start goes into the Nix build; per-start data goes through `amoeboid-qemu-args` (word-split: no whitespace in args) and state-share files.
- Python <-> Nix names must match by hand: `control.sock`, `ro-store`/`state` shares and sockets, `dN` tags/`dN.sock`, `amoeboid-qemu-args`, `/mnt/amoeboid`, `mounts`/`hostname`/`term`/`ssh_host_ed25519_key` files, linkFarm `runner`/`virtiofsd`. Table in DEVELOPING.adoc.
- Signal a pid only if `procs.alive` (checks `/proc/PID/exe`). Every spawned process registers cleanup in the `ExitStack` immediately.
- ssh argv only via `plan.ssh_argv` (pinned key, forwarding disabled). Guest console bytes shown to the host terminal go through `plan.printable_console`.
- Anything interpolated into generated Nix is validated or escaped (`_nix_string`).
- Read-only shares are enforced by host `virtiofsd --readonly`, not the guest.
- No hardcoded users, uids, home or host paths; derive from `Paths`, `RunConfig`, process identity.
- Bundled `nix/` changes don't reach existing presets/VMs; note that in the commit.

## Style

- `@dataclass(frozen=True, slots=True)`, `StrEnum`, PEP 695 generics/`type` aliases, full annotations (tests too), `match` over Ok/Err and shapes, early returns, `except A, B:`.
- No raising inside the rails: wrap expected exceptions at the source with `attempt(..., because("can't X {path}"))`; `.or_bail()` only inside `@railway`. Inner decoders return `Result[T, str]`, caller adds context.
- Pure construction in `plan`/pure modules, effects in their owning module; inject effects tests must control (`is_dir`, `env`).
- Error messages: lowercase, no trailing period, action + object (`can't write {path}: {strerror}`); hint is an imperative remedy.
- Comments say why or name a coupling; docstrings state contract in one line where possible; match surrounding density. Names: short domain words (`vm`, `share`, `mount`, `guest`, `state`, `preset`); private `_helpers`.
- No `# type: ignore`; `noqa` only with evident reason. Ruff line length 100.
- CLI: long options only, no abbreviations (`PARSER_DEFAULTS`, `_help_option` on every parser); flags overriding config default to `None`; help text written for the operator.
- User-facing text (README, `--help`, log lines, `nixconfig.GUEST_README`) is factual and must stay consistent; update all that mention a changed behaviour.

## Tests

- Test names are sentences stating the property. Hypothesis for bijections/validators, examples for policies.
- Effects tested for real on `tmp_path` (planted symlinks, renamed `sleep` processes); nothing runs Nix or qemu.
- New config key: extend the `configs` strategy in `tests/test_config.py` and the rendered example. Security property: its own named test.
- Nix/VM changes need manual verification in a throwaway VM, then remove its state:
  ```fish
  uv run amoeboid start --detach --name scratch --base-nix-config src/amoeboid/nix /some/dir
  uv run amoeboid ssh --name scratch -- systemctl --failed
  uv run amoeboid stop --name scratch; rm -rf ~/.local/share/amoeboid/scratch
  ```
  Foreground console can be driven via `pty.fork()`.

## Commits

Imperative subject ≤ ~72 chars; body is technical (what changed, why, affected keys/flags/files, compatibility), no narration. One logical change per commit; docs ship with the code they describe.
