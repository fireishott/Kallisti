"""Tests for pre-flight context validation and structured job errors.

The WebSocket job path is NON-STREAMING by design.  Build 28 (f619866,
"disable streaming") made ``_handle_job_streaming`` delegate straight to
``_handle_job_complete``, which issues one ``stream: false`` request via the
adapter's synchronous ``send_text_message`` and emits exactly one terminal
message.  Two consequences this file encodes:

  - There are no ``job.progress``/``kind: text_delta`` events on this path.
    Deltas live on the HTTP facade (``_handle_http_message``), not here.
  - A fake adapter must implement ``send_text_message``.  Implementing only
    ``send_text_message_streaming`` means the adapter is never called and
    every assertion below measures an AttributeError instead of the
    behaviour under test.

Covers:
  - _estimate_payload_tokens returns plausible estimates
  - Pre-flight check blocks over-limit jobs with job.failed + errorCategory
  - Near-limit jobs are NOT blocked and emit no context warning (B4/D4)
  - job.result carries no fabricated context block (D4, 8ccd9c6)
  - StructuredJobError carries category/detail through to the WebSocket
  - Empty response raises StructuredJobError with empty_response category
"""

from __future__ import annotations

import asyncio
import json
from unittest.mock import patch

from herald_connector.client import (
    HermesMobileConnector,
    StructuredJobError,
    _estimate_payload_tokens,
)
from herald_connector.hermes_api_executor import StreamEvent
from herald_connector.runtime_adapter import RuntimeTurnResult
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


class FakeAdapter:
    """Adapter matching the HostRuntimeAdapter contract used by the WS job path.

    ``send_text_message`` is synchronous by contract — ``_handle_job_complete``
    invokes it through ``asyncio.to_thread``.  For the streaming path (Build 22+),
    ``send_text_message_streaming`` yields progressive deltas.

    Pass ``raises`` to exercise the error-classification branches.
    """

    supports_streaming = True

    def __init__(self, text: str = "Hello world!", *, session_id: str | None = None,
                 usage: dict | None = None, raises: Exception | None = None):
        self._text = text
        self._session_id = session_id
        self._usage = usage
        self._raises = raises
        self.calls: list[dict] = []

    def send_text_message(self, **kwargs) -> RuntimeTurnResult:
        self.calls.append(kwargs)
        if self._raises is not None:
            raise self._raises
        return RuntimeTurnResult(
            text=self._text, session_id=self._session_id, usage=self._usage,
        )

    async def send_text_message_streaming(self, **kwargs):
        """Progressive streaming generator for the Build 22 WS job path."""
        self.calls.append(kwargs)
        if self._raises is not None:
            raise self._raises
        if self._text:
            yield StreamEvent(type="text_delta", data=self._text)
        yield StreamEvent(type="finish", session_id=self._session_id, usage=self._usage)


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
    with errorCategory=context_exceeded before the upstream request."""
    store = ConnectorStateStore(state_dir=tmp_path / "preflight-over")
    store.save(make_enrolled_state())
    connector = HermesMobileConnector(state_store=store)

    adapter = FakeAdapter("Should not reach here")
    ws = FakeWebSocket()
    job = {
        "id": "job-over-limit",
        "latestUserMessage": "Hello",
        "history": [],
        "contextWindow": 100,  # Very small limit
    }

    # Mock _estimate_payload_tokens to return a large number
    with patch("herald_connector.client._estimate_payload_tokens", return_value=200):
        asyncio.run(connector._handle_job_streaming(ws, job, adapter))

    # Should have: job.started + job.failed, and the model is never called
    assert adapter.calls == []
    assert len(ws.sent) == 2
    assert ws.sent[0]["type"] == "job.started"
    assert ws.sent[1]["type"] == "job.failed"
    assert ws.sent[1]["errorCategory"] == "context_exceeded"
    assert ws.sent[1]["errorDetail"]["estimatedTokens"] == 200
    assert ws.sent[1]["errorDetail"]["contextLimit"] == 100
    assert ws.sent[1]["errorDetail"]["action"] == "new_session"
    assert ws.sent[1]["retryable"] is False


def test_preflight_does_not_warn_near_limit(tmp_path):
    """A near-limit job runs normally and emits no context warning.

    The old ``context_warning`` job.progress event was removed with the WS
    streaming path in Build 28 (f619866); D4 (8ccd9c6) then deleted the iOS
    contextWarningBanner that consumed it, because the percentage driving it
    was fabricated.  The context ring is the only surface now.  A job at 95%
    of the window must still run — only a genuinely over-limit job is blocked.
    """
    store = ConnectorStateStore(state_dir=tmp_path / "preflight-warn")
    store.save(make_enrolled_state())
    connector = HermesMobileConnector(state_store=store)

    adapter = FakeAdapter("OK", session_id="sess-warn")
    ws = FakeWebSocket()
    job = {
        "id": "job-warn",
        "latestUserMessage": "Hello",
        "history": [],
        "contextWindow": 1000,
    }

    # 950 tokens = 95% of 1000: near the limit but not over it.
    with patch("herald_connector.client._estimate_payload_tokens", return_value=950):
        asyncio.run(connector._handle_job_streaming(ws, job, adapter))

    assert len(adapter.calls) == 1
    types = [m["type"] for m in ws.sent]
    assert types[0] == "job.started"
    assert "job.result" in types
    assert not [m for m in ws.sent if m.get("kind") == "context_warning"]


def test_streaming_path_emits_text_deltas(tmp_path):
    """Build 22: the WS streaming path emits job.progress text_delta events."""
    store = ConnectorStateStore(state_dir=tmp_path / "preflight-normal")
    store.save(make_enrolled_state())
    connector = HermesMobileConnector(state_store=store)

    adapter = FakeAdapter("Hello world!", session_id="sess-normal")
    ws = FakeWebSocket()
    job = {
        "id": "job-normal",
        "latestUserMessage": "Hello",
        "history": [],
        "contextWindow": 100000,
    }

    with patch("herald_connector.client._estimate_payload_tokens", return_value=100):
        asyncio.run(connector._handle_job_streaming(ws, job, adapter))

    types = [m["type"] for m in ws.sent]
    assert types[0] == "job.started"
    # Build 22: streaming path emits progress deltas
    progress_events = [m for m in ws.sent if m["type"] == "job.progress"]
    assert len(progress_events) >= 1
    assert any(m["kind"] == "text_delta" for m in progress_events)
    result = next(m for m in ws.sent if m["type"] == "job.result")
    assert result["text"] == "Hello world!"
    assert result["sessionId"] == "sess-normal"


def test_handle_job_streaming_uses_streaming_adapter(tmp_path):
    """Build 22: _handle_job_streaming drives the adapter's streaming method."""
    store = ConnectorStateStore(state_dir=tmp_path / "delegates")
    store.save(make_enrolled_state())
    connector = HermesMobileConnector(state_store=store)

    adapter = FakeAdapter("done")
    ws = FakeWebSocket()
    job = {"id": "job-delegate", "latestUserMessage": "Hello", "history": []}

    with patch("herald_connector.client._estimate_payload_tokens", return_value=10):
        asyncio.run(connector._handle_job_streaming(ws, job, adapter))

    assert len(adapter.calls) == 1
    assert ws.sent[-1]["type"] == "job.result"


def test_preflight_uses_context_window_from_job(tmp_path):
    """A job-supplied contextWindow is used instead of _context_window_for."""
    store = ConnectorStateStore(state_dir=tmp_path / "preflight-override")
    store.save(make_enrolled_state())
    connector = HermesMobileConnector(state_store=store)

    adapter = FakeAdapter("OK")
    ws = FakeWebSocket()
    job = {
        "id": "job-override",
        "latestUserMessage": "Hello",
        "history": [],
        "contextWindow": 500,
    }

    # 400 is under the job's 500 limit, but over the resolver's 300 — the
    # job value must win, so the request goes through.
    with patch("herald_connector.client._estimate_payload_tokens", return_value=400), \
            patch("herald_connector.client._context_window_for", return_value=300) as resolver:
        asyncio.run(connector._handle_job_streaming(ws, job, adapter))

    resolver.assert_not_called()
    assert ws.sent[-1]["type"] == "job.result"


def test_preflight_proceeds_when_window_unresolvable(tmp_path):
    """A None context window must not block the job (D4).

    _context_window_for returns None instead of a fabricated 256K when the
    agent subprocess cannot be reached; the pre-flight check has to treat
    that as "unknown", not "zero".
    """
    store = ConnectorStateStore(state_dir=tmp_path / "preflight-none")
    store.save(make_enrolled_state())
    connector = HermesMobileConnector(state_store=store)

    adapter = FakeAdapter("OK")
    ws = FakeWebSocket()
    job = {"id": "job-nowindow", "latestUserMessage": "Hello", "history": []}

    with patch("herald_connector.client._estimate_payload_tokens", return_value=999_999), \
            patch("herald_connector.client._context_window_for", return_value=None):
        asyncio.run(connector._handle_job_streaming(ws, job, adapter))

    assert len(adapter.calls) == 1
    assert ws.sent[-1]["type"] == "job.result"


def test_preflight_logs_estimate(tmp_path, caplog):
    """Pre-flight should log the token estimate and context window."""
    import logging

    store = ConnectorStateStore(state_dir=tmp_path / "preflight-log")
    store.save(make_enrolled_state())
    connector = HermesMobileConnector(state_store=store)

    ws = FakeWebSocket()
    job = {
        "id": "job-log",
        "latestUserMessage": "Hello",
        "history": [],
        "contextWindow": 100000,
    }

    with caplog.at_level(logging.INFO, logger="herald.connector"):
        with patch("herald_connector.client._estimate_payload_tokens", return_value=100):
            asyncio.run(connector._handle_job_streaming(ws, job, FakeAdapter("OK")))

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

    adapter = FakeAdapter(raises=StructuredJobError(
        "Session too long",
        category="context_exceeded",
        detail={"estimatedTokens": 200000, "contextLimit": 192000},
    ))

    ws = FakeWebSocket()
    job = {"id": "job-structured-error", "latestUserMessage": "Hello", "history": []}

    asyncio.run(connector._handle_job_streaming(ws, job, adapter))

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

    adapter = FakeAdapter("", session_id="sess-empty")

    ws = FakeWebSocket()
    job = {"id": "job-empty", "latestUserMessage": "Hello", "history": []}

    asyncio.run(connector._handle_job_streaming(ws, job, adapter))

    assert len(ws.sent) == 2
    assert ws.sent[0]["type"] == "job.started"
    assert ws.sent[1]["type"] == "job.failed"
    assert ws.sent[1]["errorCategory"] == "empty_response"
    assert ws.sent[1]["errorDetail"]["action"] == "retry_or_new_session"
    assert ws.sent[1]["retryable"] is False


def test_regular_exception_classified_as_internal_error(tmp_path):
    """Regular exceptions should be classified as internal_error with retry action."""
    store = ConnectorStateStore(state_dir=tmp_path / "regular-error")
    store.save(make_enrolled_state())
    connector = HermesMobileConnector(state_store=store)

    adapter = FakeAdapter(raises=RuntimeError("Something went wrong"))

    ws = FakeWebSocket()
    job = {"id": "job-regular", "latestUserMessage": "Hello", "history": []}

    asyncio.run(connector._handle_job_streaming(ws, job, adapter))

    assert len(ws.sent) == 2
    assert ws.sent[1]["type"] == "job.failed"
    assert ws.sent[1]["errorCategory"] == "internal_error"
    assert ws.sent[1]["errorAction"] == "retry"
    assert "errorDetail" not in ws.sent[1]


def test_timeout_exception_classified(tmp_path):
    """Timeout errors should be classified as timeout with retry action."""
    store = ConnectorStateStore(state_dir=tmp_path / "timeout-error")
    store.save(make_enrolled_state())
    connector = HermesMobileConnector(state_store=store)

    adapter = FakeAdapter(raises=TimeoutError("Connection timed out"))

    ws = FakeWebSocket()
    job = {"id": "job-timeout", "latestUserMessage": "Hello", "history": []}

    asyncio.run(connector._handle_job_streaming(ws, job, adapter))

    assert len(adapter.calls) == 1
    assert ws.sent[1]["errorCategory"] == "timeout"
    assert ws.sent[1]["errorAction"] == "retry"
    assert ws.sent[1]["retryable"] is True


def test_rate_limit_exception_classified(tmp_path):
    """Rate limit errors should be classified as rate_limited with wait action."""
    store = ConnectorStateStore(state_dir=tmp_path / "ratelimit-error")
    store.save(make_enrolled_state())
    connector = HermesMobileConnector(state_store=store)

    adapter = FakeAdapter(raises=RuntimeError("Rate limit exceeded (429)"))

    ws = FakeWebSocket()
    job = {"id": "job-ratelimit", "latestUserMessage": "Hello", "history": []}

    asyncio.run(connector._handle_job_streaming(ws, job, adapter))

    assert len(adapter.calls) == 1
    assert ws.sent[1]["errorCategory"] == "rate_limited"
    assert ws.sent[1]["errorAction"] == "wait"


def test_structured_error_action_preserved(tmp_path):
    """StructuredJobError with custom action should preserve it in errorAction."""
    store = ConnectorStateStore(state_dir=tmp_path / "structured-action")
    store.save(make_enrolled_state())
    connector = HermesMobileConnector(state_store=store)

    adapter = FakeAdapter(raises=StructuredJobError(
        "Context overflow",
        category="context_exceeded",
        detail={"action": "new_session", "estimatedTokens": 200000},
    ))

    ws = FakeWebSocket()
    job = {"id": "job-action", "latestUserMessage": "Hello", "history": []}

    asyncio.run(connector._handle_job_streaming(ws, job, adapter))

    assert ws.sent[1]["errorCategory"] == "context_exceeded"
    assert ws.sent[1]["errorAction"] == "new_session"


def test_job_result_omits_fabricated_context_block(tmp_path):
    """job.result must NOT carry a context block (D4, 8ccd9c6).

    The old block reported ``used`` as cumulative billing tokens and
    ``window`` as a 256K fallback — neither was a context measurement, and a
    2%-full session rendered as 90%.  D4 removed it from this payload and
    deleted the iOS banner that displayed it.  Real context data is to come
    from the agent's ContextCompressor once exposed upstream; until then the
    field must stay absent rather than wrong, because
    LiveHeraldClient only computes a ring when both window and used decode.
    """
    store = ConnectorStateStore(state_dir=tmp_path / "context-info")
    store.save(make_enrolled_state())
    connector = HermesMobileConnector(state_store=store)

    adapter = FakeAdapter(
        "Hello world!", session_id="sess-ctx", usage={"total_tokens": 4321},
    )
    ws = FakeWebSocket()
    job = {
        "id": "job-ctx",
        "latestUserMessage": "Hello",
        "history": [],
        "contextWindow": 200000,
    }

    with patch("herald_connector.client._estimate_payload_tokens", return_value=100):
        asyncio.run(connector._handle_job_streaming(ws, job, adapter))

    result_messages = [m for m in ws.sent if m.get("type") == "job.result"]
    assert len(result_messages) == 1
    result = result_messages[0]

    assert "context" not in result
    # Raw usage is still forwarded — it is billing data, not a context gauge.
    assert result["usage"] == {"total_tokens": 4321}
