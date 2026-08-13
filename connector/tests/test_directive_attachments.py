"""Tests for Electron desktop attachment unification.

The Hermes Electron desktop app does not upload attachment bytes — it writes
`@image:` / `@file:` directives into the message `content` column of state.db
and renders them locally.  Kallisti renders `Message.attachments[]` only, so
those messages used to show a raw path string instead of an attachment.

These tests pin the unification contract: directives resolve to real
attachment entries addressable at
`GET /v1/messages/{id}/attachments/{index}`, using the same keys
(`type`/`filename`/`mimeType`/`remoteIndex`) LiveHeraldClient already decodes.
"""

from __future__ import annotations

import base64
import sqlite3
from pathlib import Path

import pytest

from kallisti_connector import session_store
from kallisti_connector.session_store import (
    _extract_directive_attachments,
    _message_to_dict,
    _resolve_directive_path,
    get_attachment,
)


@pytest.fixture
def hermes_home(tmp_path, monkeypatch):
    """An isolated HERMES_HOME + connector home, so nothing touches real state."""
    home = tmp_path / ".hermes"
    (home / "images").mkdir(parents=True)
    (home / "attachments").mkdir(parents=True)
    monkeypatch.setenv("HERMES_HOME", str(home))
    monkeypatch.setenv("HERMES_MOBILE_CONNECTOR_HOME", str(tmp_path / ".hermes-mobile"))
    monkeypatch.setattr(Path, "home", classmethod(lambda cls: tmp_path))
    return home


def _png(path: Path) -> Path:
    path.write_bytes(b"\x89PNG\r\n\x1a\n" + b"0" * 128)
    return path


def _row(content: str, msg_id: int = 480336, role: str = "user") -> sqlite3.Row:
    """A state.db message row, built through sqlite3 so it is a real Row."""
    conn = sqlite3.connect(":memory:")
    conn.row_factory = sqlite3.Row
    conn.execute(
        "CREATE TABLE m (id INTEGER, role TEXT, content TEXT, "
        "timestamp REAL, reasoning_content TEXT)"
    )
    conn.execute(
        "INSERT INTO m VALUES (?,?,?,?,?)",
        (msg_id, role, content, 1786640567.0, ""),
    )
    return conn.execute("SELECT * FROM m").fetchone()


# ── Directive parsing ──────────────────────────────────────────────────────


def test_absolute_image_directive_becomes_attachment(hermes_home):
    """The exact shape seen live in state.db message 480336."""
    img = _png(hermes_home / "images" / "upload_20260813_095802_4.jpg")
    text = f"Here are the plays.\n@image:{img}"

    attachments, cleaned = _extract_directive_attachments(text)

    assert len(attachments) == 1
    assert attachments[0]["type"] == "image"
    assert attachments[0]["filename"] == "upload_20260813_095802_4.jpg"
    assert attachments[0]["mimeType"] == "image/jpeg"
    assert attachments[0]["sourcePath"] == str(img)
    # The directive text is stripped; the user's prose survives.
    assert "@image:" not in cleaned
    assert "Here are the plays." in cleaned


def test_hermes_home_relative_file_directive(hermes_home):
    """Desktop emits `.hermes/attachments/...` relative refs — seen live."""
    target = _png(hermes_home / "attachments" / "IMG_4129.png")
    attachments, cleaned = _extract_directive_attachments(
        "@file:.hermes/attachments/IMG_4129.png\n\nAlso the menu is wrong."
    )

    assert len(attachments) == 1
    assert attachments[0]["sourcePath"] == str(target)
    assert "Also the menu is wrong." in cleaned


def test_backtick_wrapped_path_with_spaces(hermes_home):
    """Live example: @file:`.hermes/attachments/ScreenRecording_... 09-26-43_1.mp4`"""
    name = "ScreenRecording_08-13-2026 09-26-43_1.mp4"
    target = hermes_home / "attachments" / name
    target.write_bytes(b"\x00" * 64)

    attachments, _ = _extract_directive_attachments(f"@file:`.hermes/attachments/{name}`")

    assert len(attachments) == 1
    assert attachments[0]["filename"] == name
    assert attachments[0]["mimeType"] == "video/mp4"
    assert attachments[0]["type"] == "file"


def test_image_dropped_as_file_directive_still_renders_as_image(hermes_home):
    """The upstream drop-handler bug emits `@file:` for images.

    Kallisti should still show a picture, so kind follows the real extension
    rather than the directive verb.
    """
    img = _png(hermes_home / "images" / "logo.png")
    attachments, _ = _extract_directive_attachments(f"@file:{img}")

    assert attachments[0]["type"] == "image"
    assert attachments[0]["mimeType"] == "image/png"


def test_multiple_directives_get_sequential_indices(hermes_home):
    a = _png(hermes_home / "images" / "a.png")
    b = _png(hermes_home / "images" / "b.png")
    attachments, cleaned = _extract_directive_attachments(f"@image:{a}\n@image:{b}")

    assert [x["filename"] for x in attachments] == ["a.png", "b.png"]
    assert cleaned == ""


def test_duplicate_reference_emits_one_attachment(hermes_home):
    img = _png(hermes_home / "images" / "same.png")
    attachments, _ = _extract_directive_attachments(f"@image:{img} and again @image:{img}")

    assert len(attachments) == 1


def test_unresolvable_directive_is_left_in_text(hermes_home):
    """A broken ref must stay visible rather than silently vanishing."""
    text = "@image:/home/nope/missing.png"
    attachments, cleaned = _extract_directive_attachments(text)

    assert attachments == []
    assert cleaned == text


def test_text_without_directives_is_untouched(hermes_home):
    text = "No attachments here, just an email: someone@example.com"
    attachments, cleaned = _extract_directive_attachments(text)

    assert attachments == []
    assert cleaned == text


# ── Path resolution / security boundary ────────────────────────────────────


def test_path_outside_hermes_home_is_refused(hermes_home, tmp_path):
    """Message content is untrusted input — it must not become a file read."""
    secret = tmp_path / "secret.png"
    _png(secret)

    assert _resolve_directive_path(str(secret)) is None


def test_traversal_escape_is_refused(hermes_home, tmp_path):
    _png(tmp_path / "outside.png")

    assert _resolve_directive_path("../outside.png") is None
    assert _resolve_directive_path(f"{hermes_home}/../outside.png") is None


def test_directory_is_not_an_attachment(hermes_home):
    assert _resolve_directive_path(str(hermes_home / "images")) is None


# ── End-to-end through _message_to_dict + serving ──────────────────────────


def test_message_to_dict_exposes_attachments_and_clean_text(hermes_home):
    img = _png(hermes_home / "images" / "upload_1.jpg")
    row = _row(f"Locked in for the day.\n@image:{img}")

    out = _message_to_dict(row)

    assert out["text"] == "Locked in for the day."
    assert out["attachments"] is not None
    att = out["attachments"][0]
    assert att["type"] == "image"
    assert att["mimeType"] == "image/jpeg"
    assert att["remoteIndex"] == 0
    # messageID + remoteIndex are what AttachmentService.swift fetches with.
    assert att["messageID"] == out["id"]


def test_attachment_bytes_are_fetchable_after_serialisation(hermes_home):
    """The full round trip the iOS app performs: poll history, then GET bytes."""
    img = _png(hermes_home / "images" / "upload_2.jpg")
    row = _row(f"@image:{img}")

    out = _message_to_dict(row)
    att = out["attachments"][0]

    fetched = get_attachment(att["messageID"], att["remoteIndex"])

    assert fetched is not None
    assert fetched["mimeType"] == "image/jpeg"
    assert base64.b64decode(fetched["data"]) == img.read_bytes()


def test_missing_index_returns_none(hermes_home):
    img = _png(hermes_home / "images" / "upload_3.jpg")
    out = _message_to_dict(_row(f"@image:{img}"))

    assert get_attachment(out["id"], 7) is None


def test_deleted_source_file_stops_serving(hermes_home):
    """Revalidation on read: the index is not a capability."""
    img = _png(hermes_home / "images" / "ephemeral.png")
    out = _message_to_dict(_row(f"@image:{img}"))
    att = out["attachments"][0]
    assert get_attachment(att["messageID"], 0) is not None

    img.unlink()

    assert get_attachment(att["messageID"], 0) is None


def test_message_without_directives_keeps_null_attachments(hermes_home):
    out = _message_to_dict(_row("Just a normal message."))

    assert out["attachments"] is None
    assert out["text"] == "Just a normal message."


def test_directive_attachments_append_after_sidecar_attachments(hermes_home):
    """Sidecar indices must stay stable when a directive is also present."""
    png_bytes = b"\x89PNG\r\n\x1a\n" + b"0" * 64
    session_store.set_message_attachments(
        session_store._deterministic_uuid("msg", 999),
        [{
            "type": "image",
            "filename": "uploaded.png",
            "mimeType": "image/png",
            "data": base64.b64encode(png_bytes).decode(),
        }],
    )
    img = _png(hermes_home / "images" / "directive.png")
    out = _message_to_dict(_row(f"@image:{img}", msg_id=999))

    assert [a["filename"] for a in out["attachments"]] == ["uploaded.png", "directive.png"]
    assert [a["remoteIndex"] for a in out["attachments"]] == [0, 1]
    # Both are retrievable at their advertised index.
    assert get_attachment(out["id"], 0)["filename"] == "uploaded.png"
    assert get_attachment(out["id"], 1)["filename"] == "directive.png"
