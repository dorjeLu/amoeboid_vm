from pathlib import Path

from amoeboid import plan
from amoeboid.mounts import Mode, Mount

MOUNTS = [Mount(Path("/home/u/dev"), Mode.RW), Mount(Path("/srv/ref"), Mode.RO)]


def test_mounts_table_numbers_tags_in_mount_order() -> None:
    assert plan.mounts_table(MOUNTS) == "d1\trw\t/home/u/dev\nd2\tro\t/srv/ref\n"


def test_qemu_args_forward_ssh_on_loopback_only_and_add_one_device_per_mount() -> None:
    args = plan.qemu_args(MOUNTS, 40022).split()
    assert "user,id=net0,hostfwd=tcp:127.0.0.1:40022-:22" in args
    assert "socket,id=amo2,path=d2.sock" in args
    assert "vhost-user-fs-pci,chardev=amo2,tag=d2" in args
    assert args.count("-device") == 3


def test_shares_cover_store_state_and_mounts_with_matching_sockets() -> None:
    shares = plan.shares(Path("/state/guest"), MOUNTS)
    assert [(s.role, s.socket, s.mode) for s in shares] == [
        ("virtiofsd-ro-store", "ro-store.sock", Mode.RO),
        ("virtiofsd-state", "state.sock", Mode.RW),
        ("virtiofsd-d1", "d1.sock", Mode.RW),
        ("virtiofsd-d2", "d2.sock", Mode.RO),
    ]


def test_virtiofsd_maps_guest_ids_onto_the_host_user() -> None:
    share = plan.shares(Path("/g"), MOUNTS)[3]
    argv = plan.virtiofsd_argv(
        Path("/bin/virtiofsd"),
        share,
        Path("/run/x"),
        guest_uid=1000,
        guest_gid=100,
        host_uid=1234,
        host_gid=1234,
    )
    assert "--uid-map=:1000:1234:1:" in argv
    assert "--gid-map=:100:1234:1:" in argv
    assert "--socket-path=/run/x/d2.sock" in argv
    assert argv[-1] == "--readonly"


def test_nix_build_uses_the_vm_config_and_overrides_the_host_module() -> None:
    argv = plan.nix_build_argv(Path("/vm/nix"), Path("/h.nix"), Path("/out"))
    assert argv[:4] == ["nix", "--extra-experimental-features", "nix-command flakes", "build"]
    assert "path:/vm/nix" in argv
    assert argv[argv.index("host") + 1] == "path:/h.nix"
    assert argv.count("--override-input") == 1


def test_ssh_pins_the_host_key_under_an_alias() -> None:
    argv = plan.ssh_argv(
        user="u", port=2222, alias="amoeboid-x", known_hosts=Path("/k"), command=["ls", "-la"]
    )
    assert argv[-3:] == ["u@127.0.0.1", "ls", "-la"]
    assert "HostKeyAlias=amoeboid-x" in argv
    assert "StrictHostKeyChecking=yes" in argv
    for option in ["ForwardAgent=no", "ForwardX11=no", "GSSAPIDelegateCredentials=no"]:
        assert option in argv
        assert argv.index(option) < argv.index("u@127.0.0.1")  # before user-supplied args


def test_console_output_keeps_colour_but_no_other_control_sequences() -> None:
    chunk = (
        b"\x1b[0;32m  OK  \x1b[0m] boot\r\n"
        b"\x1b]52;c;ZXZpbA==\x07"  # clipboard write
        b"\x1b]0;title\x1b\\"  # window title
        b"\x1b[6n\x1b[?1049h"  # cursor report, alternate screen
        b"\x1bPq696e\x1b\\"  # DCS query
        b"x\x07y\tz\x1b"
    )
    assert plan.printable_console(chunk) == b"\x1b[0;32m  OK  \x1b[0m] boot\nxy\tz"
