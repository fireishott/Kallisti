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


# ── Build 23: delivery-status and terminal-message contract ──────────────


def test_relay_message_respects_explicit_delivery_status():
    """Build 23: _relay_message accepts an explicit delivery_status."""
    msg = http_facade._relay_message(
        "user", "hello", delivery_status="sent",
    )
    assert msg["deliveryStatus"] == "sent"
    assert msg["role"] == "user"


def test_relay_message_defaults_to_delivered():
    """Default deliveryStatus is 'delivered' for backward compatibility."""
    msg = http_facade._relay_message("herald", "reply")
    assert msg["deliveryStatus"] == "delivered"


def test_relay_message_preserves_message_id():
    """Build 23: _relay_message accepts a stable message_id."""
    msg = http_facade._relay_message(
        "herald", "text", message_id="preserved-id",
    )
    assert msg["id"] == "preserved-id"


def test_relay_attachments_none_is_none():
    """None attachments produce None, not an empty list."""
    assert http_facade._relay_attachments(None) is None


def test_terminal_event_includes_message_when_present():
    """Build 23: terminal done event carries the serialized message object."""
    msg = http_facade._relay_message(
        "herald", "image reply",
        attachments=[{"type": "image", "filename": "a.png",
                      "mimeType": "image/png", "data": "AA=="}],
    )
    terminal = {
        "type": "done",
        "data": {
            "jobId": "test-job",
            "status": "completed",
            "text": "image reply",
            "message": msg,
        },
    }
    assert terminal["data"]["message"] is not None
    assert terminal["data"]["message"]["attachments"] is not None
    assert terminal["data"]["message"]["attachments"][0]["thumbnailData"] == "AA=="


# ── Build 31: attachment message identity ─────────────────────────────


def test_relay_message_and_attachment_store_share_message_id():
    """Build 31 (fix): the terminal relay message id must match the
    deterministic UUID used as the attachment-store key.

    Before the fix, _run_http_job called _relay_message without a
    message_id (random UUID), then persisted attachments under the
    deterministic Hermes-row UUID — so the live thumbnail rendered
    (base64 in the event), but the full-resolution fetch 404'd.

    The contract: the same canonical assistant message UUID must be used
    in the terminal SSE event, history reconciliation, and blob-store key.
    """
    canonical_id = "00000000-0000-0000-0000-000000000001"
    msg = http_facade._relay_message(
        "herald", "text with image",
        attachments=[{"type": "image", "filename": "a.png",
                      "mimeType": "image/png", "data": "AA=="}],
        message_id=canonical_id,
    )
    assert msg["id"] == canonical_id
    # The terminal done event carries this message — iOS reads msg["id"]
    # and uses it for GET /v1/messages/{id}/attachments/{index}.
    terminal = {
        "type": "done",
        "data": {
            "jobId": "test-job",
            "status": "completed",
            "text": "text with image",
            "message": msg,
        },
    }
    assert terminal["data"]["message"]["id"] == canonical_id


def test_relay_message_without_explicit_id_still_gets_one():
    """When no message_id is supplied (e.g. no Hermes session bound),
    _relay_message assigns a random UUID — backward compatible."""
    msg = http_facade._relay_message("herald", "text")
    assert "id" in msg
    assert msg["id"]  # non-empty
    # Must be a valid UUID string
    import uuid
    uuid.UUID(msg["id"])  # does not raise
