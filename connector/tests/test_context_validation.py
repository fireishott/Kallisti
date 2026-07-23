"""Tests for pre-flight context validation and structured SSE errors.

Covers:
  - _estimate_payload_tokens returns plausible estimates
  - Pre-flight check blocks over-limit jobs with job.failed + errorCategory
  - Pre-flight check warns at 90%+ with job.progress + context_warning
  - Normal jobs under limit stream normally (no regression)
  - StructuredJobError carries category/detail through to WebSocket
  - Empty response raises StructuredJobError with empty_response category
"""

from __future__ import annotations

import asyncio
import json
from pathlib import Path
from unittest.mock import patch

from herald_connector.client import (
    HermesMobileConnector,
    StructuredJobError,
    _estimate_payload_tokens,
)
from herald_connector.herald_api_executor import StreamEvent
from herald_connector.herald_runner import HeraldCLIExecutor
from herald_connector.runtime_adapter import RuntimeConversationMessage
from herald_connector.state import ConnectorState, ConnectorStateStore


def make_enrolled_state() -> ConnectorState:
    return ConnectorState(
        relay_url="https://relay.example.com/v1",
        web_socket_url="wss://relay.example.com/v1/hosts/ws",
        user_id="user-123",
        host_id="host-123",
        connector_credential="secret",
    )


class FakeWebSocket:
    """Minimal websocket mock that captures sent JSON messages."""

    def __init__(self):
        self.sent: list[dict] = []

    async def send(self, data: str) -> None:
        self.sent.append(json.loads(data))


# --------------------------------------------------------------------------
# _estimate_payload_tokens
# --------------------------------------------------------------------------


def test_estimate_payload_tokens_short_message():
    """A short message should return a small token count."""
    result = _estimate_payload_tokens(user_message="Hello", history=[])
    assert 1 <= result <= 10


def test_estimate_payload_tokens_longer_message():
    """A longer message should return proportionally more tokens."""
    long_message = "This is a test message with enough words to be meaningful. " * 10
    result = _estimate_payload_tokens(user_message=long_message, history=[])
    assert result > 20


def test_estimate_payload_tokens_includes_history():
    """History messages should be included in the token estimate."""
    history = [
        {"role": "user", "text": "First message from user"},
        {"role": "hermes", "text": "Response from assistant"},
    ]
    result_with = _estimate_payload_tokens(user_message="Hello", history=history)
    result_without = _estimate_payload_tokens(user_message="Hello", history=[])
    assert result_with > result_without


def test_estimate_payload_tokens_includes_attachments():
    """Attachments with extracted_text should be included in the estimate."""
    attachments = [
        {"extracted_text": "This is extracted text from a document."},
    ]
    result_with = _estimate_payload_tokens(
        user_message="Hello", history=[], attachments=attachments,
    )
    result_without = _estimate_payload_tokens(user_message="Hello", history=[])
    assert result_with > result_without


def test_estimate_payload_tokens_fallback_without_tiktoken():
    """Without tiktoken, should fall back to char/4 heuristic."""
    with patch.dict("sys.modules", {"tiktoken": None}):
        # Force import error for tiktoken
        message = "Hello world"  # 11 chars -> ~2 tokens
        result = _estimate_payload_tokens(user_message=message, history=[])
        # char/4: 11 // 4 = 2
        assert result == max(1, len(message) // 4)


def test_estimate_payload_tokens_plausible_range():
    """Token estimate for a realistic prompt should be in a plausible range."""
    # ~500 char message -> ~125 tokens with char/4, or ~50-100 with tiktoken
    message = "Please help me write a Python function that sorts a list. " * 10
    result = _estimate_payload_tokens(user_message=message, history=[])
    # Should be somewhere between 50 and 500
    assert 50 <= result <= 500


# --------------------------------------------------------------------------
# StructuredJobError
# --------------------------------------------------------------------------


def test_structured_job_error_has_category():
    error = StructuredJobError("test", category="context_exceeded")
    assert error.category == "context_exceeded"
    assert str(error) == "test"


def test_structured_job_error_has_detail():
    detail = {"estimatedTokens": 200000, "contextLimit": 192000}
    error = StructuredJobError("test", category="context_exceeded", detail=detail)
    assert error.detail == detail


def test_structured_job_error_default_detail():
    error = StructuredJobError("test", category="empty_response")
    assert error.detail == {}


# --------------------------------------------------------------------------
# Pre-flight context check in _handle_job_streaming
# --------------------------------------------------------------------------


def test_preflight_blocks_over_limit_job(tmp_path):
    """When estimated tokens exceed context window, job.failed should be sent
    with errorCategory=context_exceeded before streaming begins."""
    store = ConnectorStateStore(state_dir=tmp_path / "preflight-over")
    store.save(make_enrolled_state())
    connector = HermesMobileConnector(state_store=store)

    class FakeStreamingAdapter:
        supports_streaming = True

        async def send_text_message_streaming(self, **kwargs):
            yield StreamEvent(type="text_delta", data="Should not reach here")
            yield StreamEvent(type="finish")

    ws = FakeWebSocket()
    job = {
        "id": "job-over-limit",
        "latestUserMessage": "Hello",
        "history": [],
        "contextWindow": 100,  # Very small limit
    }

    # Mock _estimate_payload_tokens to return a large number
    with patch("herald_connector.client._estimate_payload_tokens", return_value=200):
        asyncio.run(connector._handle_job_streaming(ws, job, FakeStreamingAdapter()))

    # Should have: job.started + job.failed (no streaming events)
    assert len(ws.sent) == 2
    assert ws.sent[0]["type"] == "job.started"
    assert ws.sent[1]["type"] == "job.failed"
    assert ws.sent[1]["errorCategory"] == "context_exceeded"
    assert ws.sent[1]["errorDetail"]["estimatedTokens"] == 200
    assert ws.sent[1]["errorDetail"]["contextLimit"] == 100
    assert ws.sent[1]["errorDetail"]["action"] == "new_session"
    assert ws.sent[1]["retryable"] is False


def test_preflight_warns_at_90_percent(tmp_path):
    """When estimated tokens are >90% of context window, a context_warning
    progress event should be sent before streaming begins."""
    store = ConnectorStateStore(state_dir=tmp_path / "preflight-warn")
    store.save(make_enrolled_state())
    connector = HermesMobileConnector(state_store=store)

    class FakeStreamingAdapter:
        supports_streaming = True

        async def send_text_message_streaming(self, **kwargs):
            yield StreamEvent(type="text_delta", data="OK")
            yield StreamEvent(type="finish", session_id="sess-warn")

    ws = FakeWebSocket()
    job = {
        "id": "job-warn",
        "latestUserMessage": "Hello",
        "history": [],
        "contextWindow": 1000,
    }

    # Mock to return 950 tokens (95% of 1000)
    with patch("herald_connector.client._estimate_payload_tokens", return_value=950):
        asyncio.run(connector._handle_job_streaming(ws, job, FakeStreamingAdapter()))

    # Should have: job.started, context_warning, text_delta, job.result
    assert len(ws.sent) == 4
    assert ws.sent[0]["type"] == "job.started"
    assert ws.sent[1]["type"] == "job.progress"
    assert ws.sent[1]["kind"] == "context_warning"
    assert ws.sent[1]["estimatedTokens"] == 950
    assert ws.sent[1]["contextLimit"] == 1000
    assert ws.sent[2]["type"] == "job.progress"
    assert ws.sent[2]["kind"] == "text_delta"
    assert ws.sent[3]["type"] == "job.result"


def test_preflight_normal_job_streams_normally(tmp_path):
    """Normal jobs under the limit should stream without any context warnings."""
    store = ConnectorStateStore(state_dir=tmp_path / "preflight-normal")
    store.save(make_enrolled_state())
    connector = HermesMobileConnector(state_store=store)

    class FakeStreamingAdapter:
        supports_streaming = True

        async def send_text_message_streaming(self, **kwargs):
            yield StreamEvent(type="text_delta", data="Hello ")
            yield StreamEvent(type="text_delta", data="world!")
            yield StreamEvent(type="finish", session_id="sess-normal")

    ws = FakeWebSocket()
    job = {
        "id": "job-normal",
        "latestUserMessage": "Hello",
        "history": [],
        "contextWindow": 100000,
    }

    # Mock to return 100 tokens (well under limit)
    with patch("herald_connector.client._estimate_payload_tokens", return_value=100):
        asyncio.run(connector._handle_job_streaming(ws, job, FakeStreamingAdapter()))

    # Should have: job.started, two text_deltas, job.result
    assert len(ws.sent) == 4
    assert ws.sent[0]["type"] == "job.started"
    assert ws.sent[1]["type"] == "job.progress"
    assert ws.sent[1]["kind"] == "text_delta"
    assert ws.sent[2]["type"] == "job.progress"
    assert ws.sent[2]["kind"] == "text_delta"
    assert ws.sent[3]["type"] == "job.result"


def test_preflight_uses_context_window_from_job(tmp_path):
    """When job has contextWindow, it should be used instead of _context_window_for."""
    store = ConnectorStateStore(state_dir=tmp_path / "preflight-override")
    store.save(make_enrolled_state())
    connector = HermesMobileConnector(state_store=store)

    class FakeStreamingAdapter:
        supports_streaming = True

        async def send_text_message_streaming(self, **kwargs):
            yield StreamEvent(type="text_delta", data="OK")
            yield StreamEvent(type="finish")

    ws = FakeWebSocket()
    job = {
        "id": "job-override",
        "latestUserMessage": "Hello",
        "history": [],
        "contextWindow": 500,
    }

    # Mock _estimate_payload_tokens to return 400 (80% of 500, no warning)
    with patch("herald_connector.client._estimate_payload_tokens", return_value=400):
        asyncio.run(connector._handle_job_streaming(ws, job, FakeStreamingAdapter()))

    # Should not have context_warning (400 < 500 * 0.9 = 450)
    warning_events = [m for m in ws.sent if m.get("kind") == "context_warning"]
    assert len(warning_events) == 0


def test_preflight_logs_estimate(caplog):
    """Pre-flight should log the token estimate and context window."""
    import logging

    store = ConnectorStateStore(state_dir=Path("/tmp/preflight-log"))
    store.save(make_enrolled_state())
    connector = HermesMobileConnector(state_store=store)

    class FakeStreamingAdapter:
        supports_streaming = True

        async def send_text_message_streaming(self, **kwargs):
            yield StreamEvent(type="text_delta", data="OK")
            yield StreamEvent(type="finish")

    ws = FakeWebSocket()
    job = {
        "id": "job-log",
        "latestUserMessage": "Hello",
        "history": [],
        "contextWindow": 100000,
    }

    with caplog.at_level(logging.INFO, logger="herald.connector"):
        with patch("herald_connector.client._estimate_payload_tokens", return_value=100):
            asyncio.run(connector._handle_job_streaming(ws, job, FakeStreamingAdapter()))

    assert any("Pre-flight estimate" in record.message for record in caplog.records)


# --------------------------------------------------------------------------
# Structured error propagation in exception handler
# --------------------------------------------------------------------------


def test_structured_job_error_propagates_category(tmp_path):
    """When a StructuredJobError is raised, errorCategory and errorDetail
    should be included in the job.failed WebSocket message."""
    store = ConnectorStateStore(state_dir=tmp_path / "structured-error")
    store.save(make_enrolled_state())
    connector = HermesMobileConnector(state_store=store)

    class FakeErrorAdapter:
        supports_streaming = True

        async def send_text_message_streaming(self, **kwargs):
            raise StructuredJobError(
                "Session too long",
                category="context_exceeded",
                detail={"estimatedTokens": 200000, "contextLimit": 192000},
            )
            yield  # pragma: no cover

    ws = FakeWebSocket()
    job = {"id": "job-structured-error", "latestUserMessage": "Hello", "history": []}

    asyncio.run(connector._handle_job_streaming(ws, job, FakeErrorAdapter()))

    assert len(ws.sent) == 2
    assert ws.sent[0]["type"] == "job.started"
    assert ws.sent[1]["type"] == "job.failed"
    assert ws.sent[1]["errorCategory"] == "context_exceeded"
    assert ws.sent[1]["errorDetail"]["estimatedTokens"] == 200000
    assert ws.sent[1]["errorDetail"]["contextLimit"] == 192000


def test_structured_job_error_empty_response(tmp_path):
    """Empty response should raise StructuredJobError with empty_response category."""
    store = ConnectorStateStore(state_dir=tmp_path / "empty-structured")
    store.save(make_enrolled_state())
    connector = HermesMobileConnector(state_store=store)

    class FakeEmptyAdapter:
        supports_streaming = True

        async def send_text_message_streaming(self, **kwargs):
            yield StreamEvent(type="finish", session_id="sess-empty")

    ws = FakeWebSocket()
    job = {"id": "job-empty", "latestUserMessage": "Hello", "history": []}

    asyncio.run(connector._handle_job_streaming(ws, job, FakeEmptyAdapter()))

    assert len(ws.sent) == 2
    assert ws.sent[0]["type"] == "job.started"
    assert ws.sent[1]["type"] == "job.failed"
    assert ws.sent[1]["errorCategory"] == "empty_response"
    assert ws.sent[1]["errorDetail"]["action"] == "retry_or_new_session"
    assert ws.sent[1]["retryable"] is False


def test_regular_exception_no_category(tmp_path):
    """Regular exceptions should not have errorCategory/errorDetail."""
    store = ConnectorStateStore(state_dir=tmp_path / "regular-error")
    store.save(make_enrolled_state())
    connector = HermesMobileConnector(state_store=store)

    class FakeErrorAdapter:
        supports_streaming = True

        async def send_text_message_streaming(self, **kwargs):
            raise RuntimeError("Something went wrong")
            yield  # pragma: no cover

    ws = FakeWebSocket()
    job = {"id": "job-regular", "latestUserMessage": "Hello", "history": []}

    asyncio.run(connector._handle_job_streaming(ws, job, FakeErrorAdapter()))

    assert len(ws.sent) == 2
    assert ws.sent[1]["type"] == "job.failed"
    assert "errorCategory" not in ws.sent[1]
    assert "errorDetail" not in ws.sent[1]
