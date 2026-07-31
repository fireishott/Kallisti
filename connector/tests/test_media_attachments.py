"""Tests for inline media attachment extraction and relay shaping.

D6 fix: MEDIA: tags were dead on the /v1/runs path (the default since Build 16).
The extractor, relay-message shaping, and iOS decoder key alignment must all work
together for an image to render inline in Herald chat.
"""

from __future__ import annotations

import base64
from pathlib import Path

from herald_connector.client import _extract_media_from_response
from herald_connector import http_facade


def test_extract_media_reads_png(tmp_path: Path):
    png = tmp_path / "doggo.png"
    png.write_bytes(b"\x89PNG\r\n\x1a\n" + b"0" * 64)
    attachments, cleaned = _extract_media_from_response(
        f"Here it is.\n\nMEDIA: {png}\n\nCorner office energy."
    )
    assert len(attachments) == 1
    assert attachments[0]["type"] == "image"
    assert attachments[0]["mimeType"] == "image/png"
    assert base64.b64decode(attachments[0]["data"]).startswith(b"\x89PNG")
    assert str(png) not in cleaned
    assert "Corner office energy." in cleaned


def test_relay_attachment_carries_the_key_ios_decodes():
    """LiveHeraldClient.RelayAttachment reads `thumbnailData`, not `data`.
    Without this alias the image decodes to nil and renders as a placeholder."""
    out = http_facade._relay_attachments(
        [{"type": "image", "filename": "a.png", "mimeType": "image/png", "data": "AA=="}]
    )
    assert out[0]["thumbnailData"] == "AA=="
    assert out[0]["data"] == "AA=="          # legacy consumers keep working
    assert out[0]["filename"] == "a.png"


def test_relay_message_carries_attachments():
    msg = http_facade._relay_message(
        "herald", "text",
        attachments=[{"type": "image", "filename": "a.png",
                      "mimeType": "image/png", "data": "AA=="}],
    )
    assert msg["attachments"] is not None
    assert msg["attachments"][0]["thumbnailData"] == "AA=="


def test_relay_message_defaults_to_none():
    assert http_facade._relay_message("herald", "text")["attachments"] is None


def test_oversize_attachment_is_dropped_not_truncated(tmp_path: Path):
    big = tmp_path / "huge.png"
    big.write_bytes(b"\x89PNG\r\n\x1a\n" + b"0" * (11 * 1024 * 1024))
    attachments, _ = _extract_media_from_response(f"MEDIA: {big}")
    assert attachments == []
