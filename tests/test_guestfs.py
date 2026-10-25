import os
from pathlib import Path

from amoeboid import guestfs
from amoeboid.result import Err, Ok


def test_creates_parents_and_sets_mode(tmp_path: Path) -> None:
    assert guestfs.replace_file(tmp_path, "home/.config/git/config", b"x", 0o600) == Ok(None)
    target = tmp_path / "home/.config/git/config"
    assert target.read_bytes() == b"x"
    assert target.stat().st_mode & 0o777 == 0o600


def test_replaces_rather_than_writes_through_existing_files(tmp_path: Path) -> None:
    outside = tmp_path / "outside"
    outside.write_text("precious")
    (tmp_path / "share").mkdir()
    hardlink = tmp_path / "share/file"
    hardlink.hardlink_to(outside)
    assert guestfs.replace_file(tmp_path / "share", "file", b"new") == Ok(None)
    assert hardlink.read_text() == "new"
    assert outside.read_text() == "precious"


def test_refuses_symlinked_file(tmp_path: Path) -> None:
    outside = tmp_path / "outside"
    outside.write_text("precious")
    share = tmp_path / "share"
    share.mkdir()
    (share / "file").symlink_to(outside)
    assert guestfs.replace_file(share, "file", b"new") == Ok(None)
    assert outside.read_text() == "precious"
    assert not (share / "file").is_symlink()


def test_refuses_symlinked_directory(tmp_path: Path) -> None:
    outside = tmp_path / "outside"
    outside.mkdir()
    share = tmp_path / "share"
    share.mkdir()
    (share / "home").symlink_to(outside)
    result = guestfs.replace_file(share, "home/.gitconfig", b"x")
    assert isinstance(result, Err)
    assert "symlink" in (result.error.hint or "")
    assert list(outside.iterdir()) == []


def test_refuses_dotdot(tmp_path: Path) -> None:
    (tmp_path / "share").mkdir()
    assert isinstance(guestfs.replace_file(tmp_path / "share", "../escape", b"x"), Err)
    assert not (tmp_path / "escape").exists()


def test_remove_file_is_idempotent(tmp_path: Path) -> None:
    (tmp_path / "term").write_text("rows 1 cols 1")
    assert guestfs.remove_file(tmp_path, "term") == Ok(None)
    assert guestfs.remove_file(tmp_path, "term") == Ok(None)
    assert guestfs.remove_file(tmp_path, "missing/term") == Ok(None)
    assert not (tmp_path / "term").exists()


def test_ensure_dir(tmp_path: Path) -> None:
    assert guestfs.ensure_dir(tmp_path, "home") == Ok(None)
    assert guestfs.ensure_dir(tmp_path, "home") == Ok(None)
    assert (tmp_path / "home").is_dir()


def test_read_file(tmp_path: Path) -> None:
    (tmp_path / "a").mkdir()
    (tmp_path / "a/f").write_bytes(b"data")
    assert guestfs.read_file(tmp_path, "a/f", 100) == Ok(b"data")
    assert guestfs.read_file(tmp_path, "a/missing", 100) == Ok(None)
    assert isinstance(guestfs.read_file(tmp_path, "a/f", 3), Err)


def test_read_file_refuses_links_and_specials(tmp_path: Path) -> None:
    secret = tmp_path / "secret"
    secret.write_text("host secret")
    share = tmp_path / "share"
    (share / "d").mkdir(parents=True)
    (share / "link").symlink_to(secret)
    (share / "dirlink").symlink_to(tmp_path)
    os.mkfifo(share / "fifo")
    assert isinstance(guestfs.read_file(share, "link", 100), Err)
    assert isinstance(guestfs.read_file(share, "dirlink/secret", 100), Err)
    assert isinstance(guestfs.read_file(share, "fifo", 100), Err)
    assert isinstance(guestfs.read_file(share, "d", 100), Err)
