"""Contract tests for the opt-in dashboard WebSocket transport."""

from __future__ import annotations

import json

import pytest

from kallisti_connector.tui_gateway_executor import TuiGatewayExecutor


class FakeWS:
    def __init__(self, frames):
        self.frames = iter(frames)
        self.sent = []
        self.closed = False

    async def send(self, value):
        self.sent.append(json.loads(value))

    async def recv(self):
        return next(self.frames)

    def __aiter__(self):
        return self

    async def __anext__(self):
        try:
            return next(self.frames)
        except StopIteration:
            raise StopAsyncIteration

    async def close(self):
        self.closed = True


@pytest.mark.asyncio
async def test_gateway_events_map_to_existing_stream_contract(monkeypatch):
    ws = FakeWS([
        json.dumps({"id": 1, "result": {"session_id": "api-1"}}),
        json.dumps({"method": "event", "params": {"type": "message.delta", "payload": {"text": "Hello"}}}),
        json.dumps({"method": "event", "params": {"type": "reasoning.available", "payload": {"text": "thinking"}}}),
        json.dumps({"method": "event", "params": {"type": "message.complete", "payload": {"session_id": "api-1", "usage": {"total_tokens": 3}}}}),
    ])
    executor = TuiGatewayExecutor(username="u", password="p")

    async def connect():
        return ws

    monkeypatch.setattr(executor, "_connect", connect)
    events = [event async for event in executor.stream_message(latest_user_message="Hi")]

    assert [(event.type, event.data) for event in events[:1]] == [
        ("text_delta", "Hello"),
    ]
    assert not [event for event in events if event.type == "reasoning_delta"]
    assert events[-1].type == "finish"
    assert events[-1].session_id == "api-1"
    assert ws.closed
    assert [frame["method"] for frame in ws.sent] == ["session.create", "prompt.submit"]


@pytest.mark.asyncio
async def test_gateway_drop_is_not_synthesized_as_success(monkeypatch):
    ws = FakeWS([
        json.dumps({"id": 1, "result": {"session_id": "api-1"}}),
    ])
    executor = TuiGatewayExecutor(username="u", password="p")

    async def connect():
        return ws

    monkeypatch.setattr(executor, "_connect", connect)
    events = [event async for event in executor.stream_message(latest_user_message="Hi")]
    assert [event.type for event in events] == ["stream_interrupted"]


@pytest.mark.asyncio
async def test_unsupported_optional_reasoning_rpc_does_not_block_prompt(monkeypatch):
    ws = FakeWS([
        json.dumps({"id": 1, "result": {"session_id": "api-1"}}),
        json.dumps({"id": 2, "error": {"message": "unknown method: agent.reasoning_effort"}}),
        json.dumps({"method": "event", "params": {"type": "message.delta", "payload": {"text": "Hello"}}}),
        json.dumps({"method": "event", "params": {"type": "message.complete", "payload": {"session_id": "api-1"}}}),
    ])
    executor = TuiGatewayExecutor(username="u", password="p")

    async def connect():
        return ws

    monkeypatch.setattr(executor, "_connect", connect)
    events = [event async for event in executor.stream_message(
        latest_user_message="Hi", reasoning_effort="medium"
    )]

    assert [event.type for event in events] == ["text_delta", "finish"]
    assert [frame["method"] for frame in ws.sent] == [
        "session.create", "agent.reasoning_effort", "prompt.submit"
    ]
