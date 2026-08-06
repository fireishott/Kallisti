"""Capture sanitized SSE frames and snapshot JSON for Phase 3A evidence.

Run as ``python -m tests.capture_sse_frames`` (with PYTHONPATH pointing
at ``connector/src``).  Writes:

  * ``captured-sse-frames.sse`` — raw SSE byte stream
  * ``captured-sse-frames.json`` — parsed JSON envelopes (one per line)
  * ``captured-snapshot.json`` — one canonical snapshot envelope

This script is the source of truth for the cross-language fixtures
under ``HeraldTests/Fixtures/StreamContractV3/``; do NOT hand-write
fixtures — the brief mandates that fixtures come from captured frames.

All fixtures use sanitized UUIDs (no real user/device/host ids) so the
git history is safe to publish.
"""

from __future__ import annotations

import asyncio
import json
import os
import sqlite3
import sys
import tempfile
import uuid
from pathlib import Path
from unittest.mock import AsyncMock, patch

# Ensure we can import the connector regardless of CWD.
HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent / "src"))

from starlette.testclient import TestClient  # noqa: E402
import httpx  # noqa: E402

import kallisti_connector.http_facade as facade  # noqa: E402
from kallisti_connector.delivery_store import (  # noqa: E402
    CanonicalSnapshotIncomplete,
    DeliveryStore,
    reset_delivery_store,
)
from kallisti_connector.stream_contract import (  # noqa: E402
    build_envelope,
)


EVIDENCE_ROOT = Path(
    "/Users/curtisfreeman/Hermes-iOS-Builds/evidence/"
    "20260802T020221Z-build108-phase3/contracts"
)

# Sanitized fixture UUIDs — DO NOT use real user / device / host ids.
JOB_ID = "11111111-2222-3333-4444-555555555555"
CONV_ID = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"
HERMES_SID = "api-1111111111111111"


def _seed_terminal_envelopes(num_events: int) -> list[dict]:
    """Build a deterministic v3 event sequence for the replay buffer."""
    envelopes: list[dict] = []
    for seq in range(1, num_events):
        env_dict = build_envelope(
            job_id=JOB_ID,
            conversation_id=CONV_ID,
            attempt=1,
            seq=seq,
            conversation_revision=1,
            event_type="text.delta",
            timestamp="2026-08-01T18:00:00Z",
            payload={"delta": f"sanitized-chunk-{seq}", "segmentId": "seg-1"},
        )
        envelopes.append(env_dict)
    terminal = build_envelope(
        job_id=JOB_ID,
        conversation_id=CONV_ID,
        attempt=1,
        seq=num_events,
        conversation_revision=1,
        event_type="run.completed",
        timestamp="2026-08-01T18:00:01Z",
        payload={
            "messageId": "99999999-aaaa-bbbb-cccc-dddddddddddd",
            "text": "sanitized reply",
            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2},
        },
    )
    envelopes.append(terminal)
    return envelopes


def _capture_sse_frames() -> str:
    """Hit /v1/jobs/{id}/events and return the raw byte stream."""
    facade._http_jobs.clear()
    facade._http_job_tasks.clear()
    reset_delivery_store()
    envelopes = _seed_terminal_envelopes(num_events=4)
    facade._http_jobs[JOB_ID] = {
        "status": "completed",
        "events": envelopes,
        "subscribers": [],
        "updatedAt": 0.0,
        "conversationId": CONV_ID,
        "message": None,
        "error": None,
        "errorCategory": None,
        "errorAction": None,
        "usage": None,
        "cleanText": None,
        "clientMessageId": None,
        "attempt": 1,
    }
    transport = httpx.ASGITransport(app=facade.app)
    async def _drain() -> str:
        with patch(
            "kallisti_connector.http_facade.require_auth",
            new_callable=AsyncMock,
        ):
            async with httpx.AsyncClient(
                transport=transport, base_url="http://t"
            ) as ac:
                chunks: list[str] = []
                async with ac.stream(
                    "GET",
                    f"http://t/v1/jobs/{JOB_ID}/events",
                    headers={"Authorization": "Bearer test"},
                ) as resp:
                    if resp.status_code != 200:
                        body = await resp.aread()
                        raise RuntimeError(
                            f"SSE endpoint returned {resp.status_code}: {body!r}"
                        )
                    async for chunk in resp.aiter_text():
                        if chunk:
                            chunks.append(chunk)
                return "".join(chunks)
    return asyncio.new_event_loop().run_until_complete(_drain())


def _capture_snapshot_json() -> dict:
    """Build one canonical snapshot via the real /v1/conversations/current
    route and return the envelope dict."""
    facade._http_jobs.clear()
    facade._http_job_tasks.clear()
    reset_delivery_store()
    with tempfile.TemporaryDirectory() as tmp:
        os.environ["HERMES_MOBILE_CONNECTOR_HOME"] = tmp
        store = DeliveryStore(Path(tmp) / "delivery.sqlite3")
        store.get_or_create_binding(
            CONV_ID, HERMES_SID, "test-account", "test-device",
        )
        store.create_user_message_atomically(
            CONV_ID, str(uuid.uuid4()),
            "Hi", "Hi",
        )
        store.create_canonical_message(
            CONV_ID, "assistant", "Hello there", "Hello there",
        )
        ctx = facade.FacadeContext()
        ctx.current_conversation = lambda: {"sessionId": CONV_ID}
        ctx.connector_version = "0.7.0"
        with patch("kallisti_connector.http_facade.get_context", return_value=ctx):
            with patch(
                "kallisti_connector.http_facade.require_auth",
                new_callable=AsyncMock,
            ):
                with TestClient(facade.app) as c:
                    resp = c.get("/v1/conversations/current")
        assert resp.status_code == 200, resp.text
        return resp.json()


def main() -> None:
    EVIDENCE_ROOT.mkdir(parents=True, exist_ok=True)

    sse_text = _capture_sse_frames()
    sse_path = EVIDENCE_ROOT / "captured-sse-frames.sse"
    sse_path.write_text(sse_text)
    print(f"wrote {sse_path} ({len(sse_text)} bytes)")

    # Parse to JSON Lines for review.
    parsed: list[dict] = []
    for chunk in sse_text.split("\n\n"):
        for line in chunk.splitlines():
            if line.startswith("data:"):
                payload = line[len("data:"):].lstrip()
                try:
                    parsed.append(json.loads(payload))
                except json.JSONDecodeError:
                    continue
    jsonl_path = EVIDENCE_ROOT / "captured-sse-frames.json"
    jsonl_path.write_text(
        "\n".join(json.dumps(e) for e in parsed) + "\n"
    )
    print(f"wrote {jsonl_path} ({len(parsed)} envelopes)")

    snapshot = _capture_snapshot_json()
    snap_path = EVIDENCE_ROOT / "captured-snapshot.json"
    snap_path.write_text(json.dumps(snapshot, indent=2))
    print(f"wrote {snap_path}")

    # Emit a tiny v3 fixture derived from the captured SSE so the
    # iOS test suite always decodes a frame the connector actually
    # emitted (not a hand-rolled example).
    fixture_path = (
        Path(__file__).resolve().parent
        / "fixtures" / "hermes" / "stream_v3_captured.json"
    )
    fixture_path.write_text(json.dumps(parsed, indent=2))
    print(f"wrote {fixture_path} ({len(parsed)} envelopes)")


if __name__ == "__main__":
    main()
