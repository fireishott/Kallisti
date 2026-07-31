"""Tests for the connector's job handler and API executor.

Note on naming: this file predates Build 28 (f619866), which disabled
streaming on the WebSocket job path — ``_handle_job_streaming`` now
delegates to ``_handle_job_complete``, which makes one ``stream: false``
request through the adapter's synchronous ``send_text_message``.  The job
tests below therefore assert the *complete* contract (one job.started, one
terminal message, no deltas).  Streaming itself is still live and still
tested here, but at its real location: the HeraldAPIRuntimeAdapter
pass-through consumed by the HTTP facade.

A fake adapter for the job path must implement ``send_text_message``.
Implementing only ``send_text_message_streaming`` means the adapter is
never called and the assertions measure an AttributeError instead of the
behaviour under test — which is how ``sends_failed_on_empty_response``
used to pass on the substring "empty" in its own fake's class name.

Covers:
  - _handle_job_complete emits job.started + exactly one terminal message
  - empty response triggers job.failed
  - exceptions during the turn trigger job.failed, transport errors retryable
  - history/sessionId pass through to the adapter
  - git diff capture around the turn
  - heartbeats continue during a long model call
  - HeraldAPIExecutor SSE line parsing (tool progress regex, content deltas)
  - HeraldAPIRuntimeAdapter streaming pass-through
"""

from __future__ import annotations

import asyncio
import httpx
import json
import time
from dataclasses import dataclass
from typing import AsyncIterator

from herald_connector.client import HermesMobileConnector
from herald_connector.hermes_api_executor import (
    StreamEvent,
)
from herald_connector.herald_runner import ConnectorHeraldSettings, HeraldCLIExecutor
from herald_connector.runtime_adapter import (
    HermesAPIRuntimeAdapter,
    RuntimeConversationMessage,
    RuntimeTurnResult,
)
from herald_connector.state import (
    ConnectorState,
    ConnectorStateStore,
)


def make_enrolled_state() -> ConnectorState:
    return ConnectorState(
        relay_url="https://relay.example.com/v1",
        web_socket_url="wss://relay.example.com/v1/hosts/ws",
        user_id="user-123",
        host_id="host-123",
        connector_credential="secret",
    )


def make_executor() -> HeraldCLIExecutor:
    return HeraldCLIExecutor(
        ConnectorHeraldSettings(
            herald_command="hermes",
            herald_workdir=None,
            herald_provider=None,
            herald_model=None,
            herald_toolsets=None,
            herald_source="tool",
            herald_history_limit=20,
        )
    )


# --------------------------------------------------------------------------
# FakeWebSocket for capturing messages
# --------------------------------------------------------------------------


class FakeWebSocket:
    """Minimal websocket mock that captures sent JSON messages."""

    def __init__(self):
        self.sent: list[dict] = []

    async def send(self, data: str) -> None:
        self.sent.append(json.loads(data))


class FakeAdapter:
    """Adapter matching the HostRuntimeAdapter contract used by the job path.

    ``send_text_message`` is synchronous by contract — ``_handle_job_complete``
    calls it through ``asyncio.to_thread``.  ``before_return`` runs inside that
    call, which is where a test can simulate the model touching the filesystem
    or stalling.
    """

    supports_streaming = True

    def __init__(self, text: str = "Hello world!", *, session_id: str | None = None,
                 usage: dict | None = None, raises: Exception | None = None,
                 before_return=None):
        self._text = text
        self._session_id = session_id
        self._usage = usage
        self._raises = raises
        self._before_return = before_return
        self.calls: list[dict] = []

    def send_text_message(self, **kwargs) -> RuntimeTurnResult:
        self.calls.append(kwargs)
        if self._before_return is not None:
            self._before_return()
        if self._raises is not None:
            raise self._raises
        return RuntimeTurnResult(
            text=self._text, session_id=self._session_id, usage=self._usage,
        )


# --------------------------------------------------------------------------
# _handle_job_complete (reached via _handle_job_streaming — see module docstring)
# --------------------------------------------------------------------------


def test_job_heartbeat_awaits_async_sender(tmp_path):
    store = ConnectorStateStore(state_dir=tmp_path / "async-heartbeat")
    connector = HermesMobileConnector(
        state_store=store,
        executor=make_executor(),
        heartbeat_interval_seconds=0.01,
    )
    sent = []

    async def exercise():
        async def send(payload):
            sent.append(payload)

        connector._job_phases["job-heartbeat"] = "thinking"  # noqa: SLF001
        connector._start_job_heartbeat("job-heartbeat", send)  # noqa: SLF001
        await asyncio.sleep(0.025)
        connector._stop_job_heartbeat("job-heartbeat")  # noqa: SLF001
        await asyncio.sleep(0)

    asyncio.run(exercise())

    assert sent
    assert sent[0] == {
        "type": "job.heartbeat",
        "jobId": "job-heartbeat",
        "phase": "thinking",
        "sourceSeq": 1,
    }


def test_job_heartbeat_sequence_increments(tmp_path):
    """Each heartbeat carries a monotonically increasing sourceSeq.

    The relay orders events by sourceSeq; a heartbeat that reused a sequence
    number would let a later event sort ahead of an earlier one.
    """
    store = ConnectorStateStore(state_dir=tmp_path / "heartbeat-seq")
    connector = HermesMobileConnector(
        state_store=store,
        executor=make_executor(),
        heartbeat_interval_seconds=0.01,
    )
    sent = []

    async def exercise():
        async def send(payload):
            sent.append(payload)

        connector._job_phases["job-seq"] = "thinking"  # noqa: SLF001
        connector._start_job_heartbeat("job-seq", send)  # noqa: SLF001
        await asyncio.sleep(0.055)
        connector._stop_job_heartbeat("job-seq")  # noqa: SLF001
        await asyncio.sleep(0)

    asyncio.run(exercise())

    seqs = [m["sourceSeq"] for m in sent]
    assert len(seqs) >= 2
    assert seqs == sorted(seqs)
    assert len(set(seqs)) == len(seqs)


def test_handle_job_sends_started_then_result(tmp_path):
    """job.started then exactly one job.result — no deltas on this path."""
    store = ConnectorStateStore(state_dir=tmp_path / "streaming-happy")
    store.save(make_enrolled_state())
    connector = HermesMobileConnector(state_store=store, executor=make_executor())

    adapter = FakeAdapter(
        "Hello world!",
        session_id="session-abc",
        usage={"prompt_tokens": 100, "completion_tokens": 25, "total_tokens": 125},
    )

    ws = FakeWebSocket()
    job = {
        "id": "job-123",
        "latestUserMessage": "Tell me something",
        "history": [],
        "sessionId": "session-prev",
    }

    asyncio.run(connector._handle_job_streaming(ws, job, adapter))  # noqa: SLF001

    assert [m["type"] for m in ws.sent] == ["job.started", "job.result"]
    assert ws.sent[0]["jobId"] == "job-123"
    assert ws.sent[1]["jobId"] == "job-123"
    assert ws.sent[1]["text"] == "Hello world!"
    assert ws.sent[1]["sessionId"] == "session-abc"
    assert ws.sent[1]["usage"]["total_tokens"] == 125


def test_handle_job_sends_failed_on_empty_response(tmp_path):
    """A turn that returns no text should send job.failed after job.started."""
    store = ConnectorStateStore(state_dir=tmp_path / "streaming-empty")
    store.save(make_enrolled_state())
    connector = HermesMobileConnector(state_store=store, executor=make_executor())

    adapter = FakeAdapter("", session_id="sess-empty")
    ws = FakeWebSocket()
    job = {"id": "job-empty", "latestUserMessage": "Empty", "history": []}

    asyncio.run(connector._handle_job_streaming(ws, job, adapter))  # noqa: SLF001

    assert len(adapter.calls) == 1
    assert len(ws.sent) == 2
    assert ws.sent[0]["type"] == "job.started"
    assert ws.sent[1]["type"] == "job.failed"
    assert ws.sent[1]["jobId"] == "job-empty"
    assert ws.sent[1]["errorCategory"] == "empty_response"


def test_handle_job_sends_failed_on_exception(tmp_path):
    """If the adapter raises, the handler should catch and send job.failed."""
    store = ConnectorStateStore(state_dir=tmp_path / "streaming-error")
    store.save(make_enrolled_state())
    connector = HermesMobileConnector(state_store=store, executor=make_executor())

    adapter = FakeAdapter(raises=RuntimeError("API server gone"))
    ws = FakeWebSocket()
    job = {"id": "job-error", "latestUserMessage": "Crash", "history": []}

    asyncio.run(connector._handle_job_streaming(ws, job, adapter))  # noqa: SLF001

    assert [m["type"] for m in ws.sent] == ["job.started", "job.failed"]
    assert "API server gone" in ws.sent[1]["error"]
    assert ws.sent[1]["retryable"] is False


def test_handle_job_marks_transport_failures_retryable(tmp_path):
    store = ConnectorStateStore(state_dir=tmp_path / "streaming-transport-error")
    store.save(make_enrolled_state())
    connector = HermesMobileConnector(state_store=store, executor=make_executor())

    request = httpx.Request("POST", "http://localhost:8642/v1/chat/completions")
    adapter = FakeAdapter(raises=httpx.ConnectError("connection refused", request=request))

    ws = FakeWebSocket()
    job = {"id": "job-transport", "latestUserMessage": "Retry me", "history": []}

    asyncio.run(connector._handle_job_streaming(ws, job, adapter))  # noqa: SLF001

    assert len(ws.sent) == 2
    assert ws.sent[0]["type"] == "job.started"
    assert ws.sent[1]["type"] == "job.failed"
    assert ws.sent[1]["retryable"] is True


def test_handle_job_passes_history_and_session(tmp_path):
    """History and sessionId from the job are passed through to the adapter."""
    store = ConnectorStateStore(state_dir=tmp_path / "streaming-history")
    store.save(make_enrolled_state())
    connector = HermesMobileConnector(state_store=store, executor=make_executor())

    adapter = FakeAdapter("OK", session_id="sess-new")
    ws = FakeWebSocket()
    job = {
        "id": "job-hist",
        "latestUserMessage": "Follow up question",
        "history": [
            {"role": "user", "text": "First message"},
            {"role": "hermes", "text": "First reply"},
        ],
        "sessionId": "session-prev-123",
    }

    asyncio.run(connector._handle_job_streaming(ws, job, adapter))  # noqa: SLF001

    assert len(adapter.calls) == 1
    captured = adapter.calls[0]
    assert captured["latest_user_message"] == "Follow up question"
    assert captured["session_id"] == "session-prev-123"
    assert len(captured["history"]) == 2
    assert captured["history"][0].role == "user"
    assert captured["history"][0].text == "First message"
    assert captured["history"][1].role == "hermes"
    assert captured["history"][1].text == "First reply"


def test_handle_job_cli_materializes_attachments_for_tool_access(tmp_path):
    store = ConnectorStateStore(state_dir=tmp_path / "cli-attachments")
    store.save(make_enrolled_state())
    connector = HermesMobileConnector(state_store=store, executor=make_executor())

    captured = {}

    class FakeCLIRuntime:
        def send_text_message(self, *, latest_user_message, history, session_id=None):
            captured["latest_user_message"] = latest_user_message
            return type("Result", (), {"text": "Done", "session_id": "sess-cli"})()

    ws = FakeWebSocket()
    job = {
        "id": "job-cli-attachments",
        "latestUserMessage": "",
        "history": [],
        "attachments": [
            {
                "type": "image",
                "filename": "screen.png",
                "mimeType": "image/png",
                "data": "aGVsbG8=",
            },
            {
                "type": "file",
                "filename": "notes.txt",
                "mimeType": "text/plain",
                "data": "aGVsbG8=",
            },
        ],
    }

    asyncio.run(connector._handle_job_cli(ws, job, FakeCLIRuntime()))  # noqa: SLF001

    # job.started precedes job.result
    assert ws.sent[0]["type"] == "job.started"
    result = next(m for m in ws.sent if m["type"] == "job.result")
    assert "vision_analyze" in captured["latest_user_message"]
    assert "read_file" in captured["latest_user_message"]
    assert "screen.png" in captured["latest_user_message"]
    assert "notes.txt" in captured["latest_user_message"]


def test_handle_job_stages_attachments_then_uses_api_runtime(tmp_path, monkeypatch):
    """Attachment jobs should stage files to disk, clear raw attachments, then go
    through the API runtime — not the CLI path.

    Staging happens in ``_handle_job`` before runtime selection, so it applies
    on the complete path that Build 28 made the default.
    """
    store = ConnectorStateStore(state_dir=tmp_path / "attachment-routing")
    store.save(make_enrolled_state())
    connector = HermesMobileConnector(state_store=store, executor=make_executor())

    class FakeStreamingRuntime:
        supports_streaming = True

    async def fake_runtime_adapter_async(state):  # noqa: ANN001
        return FakeStreamingRuntime()

    captured: dict = {}

    async def fake_handle_job_complete(websocket, job, runtime, workdir=None):  # noqa: ANN001
        captured["api"] = True
        captured["attachments"] = job.get("attachments")
        captured["user_message"] = job.get("latestUserMessage", "")

    async def fake_handle_job_cli(websocket, job, runtime):  # noqa: ANN001
        captured["cli"] = True

    monkeypatch.setattr(connector, "runtime_adapter_for_state_async", fake_runtime_adapter_async)
    monkeypatch.setattr(connector, "_handle_job_complete", fake_handle_job_complete)
    monkeypatch.setattr(connector, "_handle_job_cli", fake_handle_job_cli)

    job = {
        "id": "job-attachments",
        "latestUserMessage": "What is in this image?",
        "history": [],
        "attachments": [
            {
                "type": "image",
                "filename": "photo.jpg",
                "mimeType": "image/jpeg",
                "data": "aGVsbG8=",
            }
        ],
    }

    asyncio.run(connector._handle_job(FakeWebSocket(), job))  # noqa: SLF001

    assert captured.get("api") is True
    assert captured.get("cli") is None
    assert captured["attachments"] is None  # raw data cleared after staging
    assert "vision_analyze" in captured["user_message"]
    assert "photo.jpg" in captured["user_message"]


# --------------------------------------------------------------------------
# HermesAPIRuntimeAdapter streaming pass-through
# --------------------------------------------------------------------------


def test_api_runtime_adapter_streaming_yields_all_events():
    """The adapter's send_text_message_streaming should faithfully yield all
    events from the executor's stream_message."""
    emitted_events = [
        StreamEvent(type="tool_activity", label="🔧 Building"),
        StreamEvent(type="text_delta", data="Result: "),
        StreamEvent(type="text_delta", data="42"),
        StreamEvent(type="finish", session_id="sess-42", usage={"total_tokens": 50}),
    ]

    class FakeExecutor:
        async def stream_message(self, *, latest_user_message, history=None, session_id=None, attachments=None, reasoning_effort=None):
            for event in emitted_events:
                yield event

    adapter = HermesAPIRuntimeAdapter(FakeExecutor())

    collected = []

    async def collect():
        async for event in adapter.send_text_message_streaming(
            latest_user_message="What is 6*7?",
            history=[RuntimeConversationMessage(role="user", text="Hello")],
            session_id="sess-prev",
        ):
            collected.append(event)

    asyncio.run(collect())

    assert len(collected) == 4
    assert collected[0].type == "tool_activity"
    assert collected[0].label == "🔧 Building"
    assert collected[1].type == "text_delta"
    assert collected[1].data == "Result: "
    assert collected[2].type == "text_delta"
    assert collected[2].data == "42"
    assert collected[3].type == "finish"
    assert collected[3].session_id == "sess-42"
    assert collected[3].usage == {"total_tokens": 50}


def test_api_runtime_adapter_streaming_preserves_session_with_history():
    """When history is provided, the adapter should still pass session_id through
    to preserve session continuity and prefix caching."""
    captured = {}

    class FakeExecutor:
        async def stream_message(self, *, latest_user_message, history=None, session_id=None, attachments=None, reasoning_effort=None):
            captured["session_id"] = session_id
            captured["history"] = history
            yield StreamEvent(type="text_delta", data="ok")
            yield StreamEvent(type="finish")

    adapter = HermesAPIRuntimeAdapter(FakeExecutor())

    async def run():
        async for _ in adapter.send_text_message_streaming(
            latest_user_message="test",
            history=[RuntimeConversationMessage(role="user", text="prior")],
            session_id="should-be-dropped",
        ):
            pass

    asyncio.run(run())

    assert captured["session_id"] == "should-be-dropped"
    assert len(captured["history"]) == 1


def test_api_runtime_adapter_streaming_keeps_session_when_no_history():
    """When no history is provided, the adapter should pass the session_id through."""
    captured = {}

    class FakeExecutor:
        async def stream_message(self, *, latest_user_message, history=None, session_id=None, attachments=None, reasoning_effort=None):
            captured["session_id"] = session_id
            yield StreamEvent(type="text_delta", data="ok")
            yield StreamEvent(type="finish")

    adapter = HermesAPIRuntimeAdapter(FakeExecutor())

    async def run():
        async for _ in adapter.send_text_message_streaming(
            latest_user_message="test",
            history=[],
            session_id="keep-this",
        ):
            pass

    asyncio.run(run())

    assert captured["session_id"] == "keep-this"


# --------------------------------------------------------------------------
# HermesAPIExecutor._messages_payload builds correct OpenAI format
# --------------------------------------------------------------------------


def test_messages_payload_builds_openai_format():
    """The executor should build messages with 'assistant' role for 'hermes' entries."""
    from herald_connector.hermes_api_executor import HermesAPIExecutor
    from herald_connector.herald_runner import HeraldConversationMessage

    executor = HermesAPIExecutor()
    history = [
        HeraldConversationMessage(role="user", text="Hello"),
        HeraldConversationMessage(role="hermes", text="Hi there"),
        HeraldConversationMessage(role="user", text="How are you?"),
    ]

    messages = executor._messages_payload(  # noqa: SLF001
        latest_user_message="What's up?",
        history=history,
    )

    assert len(messages) == 4
    assert messages[0] == {"role": "user", "content": "Hello"}
    assert messages[1] == {"role": "assistant", "content": "Hi there"}
    assert messages[2] == {"role": "user", "content": "How are you?"}
    assert messages[3] == {"role": "user", "content": "What's up?"}


def test_messages_payload_skips_empty_history_entries():
    """Empty/whitespace-only history entries should be filtered out."""
    from herald_connector.hermes_api_executor import HermesAPIExecutor
    from herald_connector.herald_runner import HeraldConversationMessage

    executor = HermesAPIExecutor()
    history = [
        HeraldConversationMessage(role="user", text="Real message"),
        HeraldConversationMessage(role="hermes", text="   "),
        HeraldConversationMessage(role="user", text=""),
    ]

    messages = executor._messages_payload(  # noqa: SLF001
        latest_user_message="Final",
        history=history,
    )

    assert len(messages) == 2
    assert messages[0] == {"role": "user", "content": "Real message"}
    assert messages[1] == {"role": "user", "content": "Final"}


# --------------------------------------------------------------------------
# Git diff integration in _handle_job_streaming
# --------------------------------------------------------------------------

import subprocess


def _init_git_repo(path):
    subprocess.run(["git", "init"], cwd=str(path), capture_output=True, check=True)
    subprocess.run(["git", "config", "user.email", "t@t.com"], cwd=str(path), capture_output=True, check=True)
    subprocess.run(["git", "config", "user.name", "T"], cwd=str(path), capture_output=True, check=True)
    (path / "main.py").write_text("pass\n")
    subprocess.run(["git", "add", "."], cwd=str(path), capture_output=True, check=True)
    subprocess.run(["git", "commit", "-m", "init"], cwd=str(path), capture_output=True, check=True)


def test_handle_job_includes_diff_when_files_change(tmp_path):
    """If Hermes modifies files during the turn, job.result should include diff data."""
    repo_dir = tmp_path / "repo"
    repo_dir.mkdir()
    _init_git_repo(repo_dir)

    store = ConnectorStateStore(state_dir=tmp_path / "streaming-diff")
    store.save(make_enrolled_state())
    connector = HermesMobileConnector(state_store=store, executor=make_executor())

    adapter = FakeAdapter(
        "Done!",
        session_id="sess-diff",
        # Simulate Hermes modifying a file while the turn is in flight.
        before_return=lambda: (repo_dir / "main.py").write_text("print('hello world')\n"),
    )

    ws = FakeWebSocket()
    job = {"id": "job-diff", "latestUserMessage": "Fix the code", "history": []}

    asyncio.run(
        connector._handle_job_streaming(  # noqa: SLF001
            ws, job, adapter, workdir=str(repo_dir),
        )
    )

    result = next(m for m in ws.sent if m["type"] == "job.result")
    assert "diff" in result
    assert len(result["diff"]["files"]) == 1
    assert result["diff"]["files"][0]["path"] == "main.py"
    assert result["diff"]["files"][0]["status"] == "modified"
    assert "1 file changed" in result["diff"]["summary"]


def test_handle_job_no_diff_when_no_workdir(tmp_path):
    """When workdir is None (non-git context), no diff should be included."""
    store = ConnectorStateStore(state_dir=tmp_path / "streaming-no-workdir")
    store.save(make_enrolled_state())
    connector = HermesMobileConnector(state_store=store, executor=make_executor())

    ws = FakeWebSocket()
    job = {"id": "job-nodiff", "latestUserMessage": "Hello", "history": []}

    asyncio.run(connector._handle_job_streaming(ws, job, FakeAdapter("Result")))  # noqa: SLF001

    result = next(m for m in ws.sent if m["type"] == "job.result")
    assert "diff" not in result
    # Verify job.started was sent first
    assert ws.sent[0]["type"] == "job.started"


def test_handle_job_no_diff_when_no_changes(tmp_path):
    """When Hermes doesn't modify any files, no diff should be included."""
    repo_dir = tmp_path / "clean-repo"
    repo_dir.mkdir()
    _init_git_repo(repo_dir)

    store = ConnectorStateStore(state_dir=tmp_path / "streaming-clean")
    store.save(make_enrolled_state())
    connector = HermesMobileConnector(state_store=store, executor=make_executor())

    ws = FakeWebSocket()
    job = {"id": "job-clean", "latestUserMessage": "Check the code", "history": []}

    asyncio.run(
        connector._handle_job_streaming(  # noqa: SLF001
            ws, job, FakeAdapter("No changes needed"), workdir=str(repo_dir),
        )
    )

    result = next(m for m in ws.sent if m["type"] == "job.result")
    assert "diff" not in result


# ---------------------------------------------------------------------------
# InlineThinkParser tests
# ---------------------------------------------------------------------------


def test_inline_think_parser_no_think_tags():
    from herald_connector.herald_api_executor import InlineThinkParser

    parser = InlineThinkParser()
    text, reasoning = parser.feed("Hello world")
    assert text == "Hello world"
    assert reasoning is None


def test_inline_think_parser_simple_think_block():
    from herald_connector.herald_api_executor import InlineThinkParser

    parser = InlineThinkParser()
    text, reasoning = parser.feed("Before <think>reasoning here</think> After")
    assert text == "Before  After"
    assert reasoning == "reasoning here"


def test_inline_think_parser_think_at_start():
    from herald_connector.herald_api_executor import InlineThinkParser

    parser = InlineThinkParser()
    text, reasoning = parser.feed("<think>internal thoughts</think>Answer here")
    assert text == "Answer here"
    assert reasoning == "internal thoughts"


def test_inline_think_parser_split_open_marker():
    from herald_connector.herald_api_executor import InlineThinkParser

    parser = InlineThinkParser()
    t1, r1 = parser.feed("Hello <th")
    assert t1 == "Hello "
    assert r1 is None

    # Open tag completes — content enters think mode, accumulated until close
    t2, r2 = parser.feed("ink>thinking...")
    assert t2 is None
    assert r2 is None  # no close tag yet

    # Close tag arrives — full reasoning flushed
    t3, r3 = parser.feed("</think>Answer")
    assert t3 == "Answer"
    assert r3 == "thinking..."


def test_inline_think_parser_split_close_marker():
    from herald_connector.herald_api_executor import InlineThinkParser

    parser = InlineThinkParser()
    t0, r0 = parser.feed("<think>deep")
    # No close tag yet — reasoning accumulated, not returned
    assert t0 is None
    assert r0 is None

    # Close tag completes — full reasoning flushed
    t1, r1 = parser.feed(" thought</think>")
    assert r1 == "deep thought"
    assert t1 is None

    t2, r2 = parser.feed(" Answer")
    assert t2 == " Answer"
    assert r2 is None


def test_inline_think_parser_missing_close_marker():
    from herald_connector.herald_api_executor import InlineThinkParser

    parser = InlineThinkParser()
    t1, r1 = parser.feed("Hello <think>unclosed reasoning")
    assert t1 == "Hello "
    # No close tag yet — reasoning accumulated
    assert r1 is None

    t2, r2 = parser.feed(" more")
    assert t2 is None
    assert r2 is None

    # Flush returns all accumulated reasoning
    remaining = parser.flush()
    assert remaining == "unclosed reasoning more"


def test_inline_think_parser_ordinary_angle_brackets():
    from herald_connector.herald_api_executor import InlineThinkParser

    parser = InlineThinkParser()
    text, reasoning = parser.feed("Use <b>bold</b> and <i>italic</i>")
    assert text == "Use <b>bold</b> and <i>italic</i>"
    assert reasoning is None


def test_inline_think_parser_empty_think_block():
    from herald_connector.herald_api_executor import InlineThinkParser

    parser = InlineThinkParser()
    text, reasoning = parser.feed("Before <think></think>After")
    assert text == "Before After"
    assert reasoning is None


# ---------------------------------------------------------------------------
# Heartbeat continuation during long tool execution
# ---------------------------------------------------------------------------


def test_heartbeat_during_long_turn(tmp_path):
    """Heartbeats continue during a long-running model turn.

    _handle_job_complete starts the heartbeat before the upstream request and
    the request itself is a blocking call on a worker thread, so the relay's
    lease has to be renewed by the heartbeat alone. This simulates a slow turn
    and checks the heartbeats keep flowing while it is in flight.
    """
    store = ConnectorStateStore(state_dir=tmp_path / "heartbeat-long")
    store.save(make_enrolled_state())
    connector = HermesMobileConnector(
        state_store=store,
        executor=make_executor(),
        heartbeat_interval_seconds=0.05,  # Fast for testing
    )
    heartbeat_messages = []

    async def exercise():
        ws = FakeWebSocket()

        # Blocking sleep: send_text_message is sync and runs on a worker
        # thread, so this holds the turn open without blocking the loop.
        adapter = FakeAdapter(
            "Build complete",
            session_id="sess-slow",
            before_return=lambda: time.sleep(0.2),
        )

        job = {
            "id": "job-slow-tool",
            "latestUserMessage": "Build the project",
            "history": [],
        }

        await connector._handle_job_streaming(ws, job, adapter)

        for msg in ws.sent:
            if msg.get("type") == "job.heartbeat":
                heartbeat_messages.append(msg)

    asyncio.run(exercise())

    # At least one heartbeat should have been sent during the 200ms turn
    assert len(heartbeat_messages) >= 1, (
        f"Expected at least 1 heartbeat during the turn, got {len(heartbeat_messages)}"
    )
    assert heartbeat_messages[0]["jobId"] == "job-slow-tool"


def test_heartbeat_does_not_fabricate_semantic_events(tmp_path):
    """During heartbeat-only periods, no text_delta or tool_activity events
    should be fabricated. Only job.heartbeat messages should be sent."""
    store = ConnectorStateStore(state_dir=tmp_path / "heartbeat-no-fabricate")
    store.save(make_enrolled_state())
    connector = HermesMobileConnector(
        state_store=store,
        executor=make_executor(),
        heartbeat_interval_seconds=0.05,
    )

    async def exercise():
        ws = FakeWebSocket()

        adapter = FakeAdapter(
            "Finally!",
            session_id="sess-silent",
            before_return=lambda: time.sleep(0.2),
        )

        job = {
            "id": "job-silent",
            "latestUserMessage": "Wait for it",
            "history": [],
        }

        await connector._handle_job_streaming(ws, job, adapter)

        # Everything before the terminal message must be a heartbeat: the
        # connector must never invent progress it did not receive.
        pre_terminal = []
        for msg in ws.sent:
            if msg["type"] in ("job.result", "job.failed"):
                break
            pre_terminal.append(msg)

        assert any(m["type"] == "job.heartbeat" for m in pre_terminal)
        for msg in pre_terminal:
            assert msg["type"] in ("job.started", "job.heartbeat"), (
                f"Unexpected message type during silence: {msg['type']}"
            )

    asyncio.run(exercise())


# ---------------------------------------------------------------------------
# SSE comment handling in API executor
# ---------------------------------------------------------------------------


def test_sse_comments_skipped_without_creating_events():
    """SSE comment lines (starting with ':') from the /v1/runs events endpoint
    should be skipped without producing StreamEvents.

    Build 16: /v1/runs is now the canonical streaming path. This test
    exercises the real HeraldAPIExecutor.stream_message() → runs SSE parser
    with a mocked httpx transport so SSE comment lines flow through
    _parse_runs_sse.
    """
    from unittest.mock import patch

    from herald_connector.herald_api_executor import HeraldAPIExecutor

    # Runs SSE events with interspersed comments/id lines
    sse_lines = [
        ": keepalive",
        'event: assistant.delta',
        'data: {"text": "Hello"}',
        "",
        ": heartbeat",
        'event: run.completed',
        'data: {"session_id": "s1"}',
        "",
    ]

    class MockEventResponse:
        status_code = 200
        headers: dict = {}

        def raise_for_status(self):
            pass

        async def aiter_lines(self):
            for line in sse_lines:
                yield line

    class MockEventStreamContext:
        async def __aenter__(self):
            return MockEventResponse()

        async def __aexit__(self, *args):
            return False

    class MockRunsResponse:
        status_code = 200
        headers: dict = {}

        def raise_for_status(self):
            pass

        def json(self):
            return {"run_id": "run-1"}

    class MockClient:
        async def __aenter__(self):
            return self

        async def __aexit__(self, *args):
            return False

        async def get(self, *args, **kwargs):
            # _runs_available probe
            r = type("Resp", (), {"status_code": 200, "json": lambda s: {"features": {"run_events_sse": True}}})()
            return r

        async def post(self, *args, **kwargs):
            return MockRunsResponse()

        def stream(self, *args, **kwargs):
            return MockEventStreamContext()

    mock_client_instance = MockClient()

    async def exercise():
        with patch("httpx.AsyncClient", return_value=mock_client_instance):
            executor = HeraldAPIExecutor()
            events = []
            async for event in executor.stream_message(
                latest_user_message="test",
            ):
                events.append(event)

        # Only the assistant.delta and run.completed produce StreamEvents;
        # comments and blank lines are skipped.
        assert len(events) == 2
        assert events[0].type == "text_delta"
        assert events[0].data == "Hello"
        assert events[1].type == "finish"

    asyncio.run(exercise())


def test_sse_comment_lines_do_not_yield_keepalive():
    """SSE comments interspersed between /v1/runs events data chunks should
    not generate keepalive StreamEvents — only actual data lines produce events.

    Build 16: exercises the runs SSE parser via stream_message() with a mocked
    httpx transport so SSE comment lines flow through _parse_runs_sse.
    """
    from unittest.mock import patch

    from herald_connector.herald_api_executor import HeraldAPIExecutor

    sse_lines = [
        ": keepalive",
        'event: assistant.delta',
        'data: {"text": "Hello"}',
        "",
        ": heartbeat",
        ": ping",
        'event: run.completed',
        'data: {"session_id": "s1"}',
        "",
    ]

    class MockEventResponse:
        status_code = 200
        headers: dict = {}

        def raise_for_status(self):
            pass

        async def aiter_lines(self):
            for line in sse_lines:
                yield line

    class MockEventStreamContext:
        async def __aenter__(self):
            return MockEventResponse()

        async def __aexit__(self, *args):
            return False

    class MockRunsResponse:
        status_code = 200
        headers: dict = {}

        def raise_for_status(self):
            pass

        def json(self):
            return {"run_id": "run-1"}

    class MockClient:
        async def __aenter__(self):
            return self

        async def __aexit__(self, *args):
            return False

        async def get(self, *args, **kwargs):
            r = type("Resp", (), {"status_code": 200, "json": lambda s: {"features": {"run_events_sse": True}}})()
            return r

        async def post(self, *args, **kwargs):
            return MockRunsResponse()

        def stream(self, *args, **kwargs):
            return MockEventStreamContext()

    mock_client_instance = MockClient()

    async def exercise():
        with patch("httpx.AsyncClient", return_value=mock_client_instance):
            executor = HeraldAPIExecutor()
            events = []
            async for event in executor.stream_message(
                latest_user_message="test",
            ):
                events.append(event)

        assert len(events) == 2
        assert events[0].type == "text_delta"
        assert events[0].data == "Hello"
        assert events[1].type == "finish"
        # No keepalive events should appear — comments are silently skipped
        assert all(e.type != "keepalive" for e in events)

    asyncio.run(exercise())
