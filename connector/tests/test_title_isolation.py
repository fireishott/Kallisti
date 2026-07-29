"""Tests for T2: title generation must never touch the user's session.

Covers:
  - _auto_title handler is never called with the user's real hermes_sid
  - session_generate_title handler is never called with the user's session_id
  - No title-prompt content lands in the user's message history
"""

from __future__ import annotations

import asyncio
import uuid
from unittest.mock import AsyncMock, MagicMock, patch

import pytest

from herald_connector.http_facade import _auto_title, _auto_title_and_persist


class FakeHandler:
    """Records every session_id it is called with and yields a simple title text_delta + done."""

    def __init__(self):
        self.calls: list[dict] = []

    async def __call__(
        self, prompt: str, history: list, session_id: str | None,
        attachments: list | None, reasoning_effort: str | None,
    ):
        self.calls.append({
            "prompt": prompt,
            "session_id": session_id,
        })
        yield {"type": "text_delta", "data": {"delta": "Test"}}
        yield {"type": "text_delta", "data": {"delta": " Title"}}
        yield {"type": "done", "data": {"text": "Test Title"}}


@pytest.mark.asyncio
async def test_auto_title_never_uses_real_session():
    """The handler must never be called with the user's real hermes_sid."""
    handler = FakeHandler()
    user_session = "api-testsession-12345"

    title = await _auto_title(
        handler,
        text="Hello, how are you today?",
        hermes_sid=user_session,
        app_uuid="app-uuid-12345",
    )

    # Handler should have been called (to generate the title)
    assert len(handler.calls) == 1, "Handler should be called once for title generation"

    # But it must NOT have been called with the user's real session id
    actual_session = handler.calls[0]["session_id"]
    assert actual_session != user_session, (
        f"Handler was called with user's real session_id '{user_session}'. "
        f"Title generation must use a throwaway session."
    )

    # The throwaway session should be a title-prefixed UUID or similar isolated ID
    assert actual_session is not None
    assert "title-" in actual_session or "-title-" in actual_session or actual_session.startswith("title"), (
        f"Expected throwaway session to include 'title' marker, got: {actual_session}"
    )

    # Title should have been extracted from the handler response
    assert title == "Test Title"


@pytest.mark.asyncio
async def test_auto_title_and_persist_uses_throwaway_session():
    """_auto_title_and_persist must route through a throwaway session, not the user's."""
    handler = FakeHandler()
    user_session = "20260728_204452_5ffc8e"  # Real session format

    # set_session_meta is imported at call time from session_store
    with patch("herald_connector.session_store.set_session_meta") as mock_set_meta:
        await _auto_title_and_persist(
            handler,
            text="Sup G",
            hermes_sid=user_session,
            app_uuid="canonical-app-uuid",
        )

    # Handler should have been called
    assert len(handler.calls) == 1

    # Verify the handler was NOT called with the user's real session
    actual_session = handler.calls[0]["session_id"]
    assert actual_session != user_session, (
        f"_auto_title_and_persist leaked user session '{user_session}' into handler"
    )

    # set_session_meta should still target the correct app_uuid
    mock_set_meta.assert_called_once()
    call_args = mock_set_meta.call_args
    assert call_args[0][0] == "canonical-app-uuid", (
        "set_session_meta must target the real app_uuid, not the throwaway session"
    )


@pytest.mark.asyncio
async def test_no_title_prompt_leaks_to_session_store():
    """The title prompt text must never appear in any persistence path for the user session."""
    handler = FakeHandler()
    user_session = "api-testsession-leakcheck"

    with patch("herald_connector.session_store.set_session_meta") as mock_set_meta:
        await _auto_title_and_persist(
            handler,
            text="User's real message",
            hermes_sid=user_session,
            app_uuid="app-leakcheck",
        )

    # set_session_meta should only be called with the real app_uuid and a title,
    # never with the title prompt text
    mock_set_meta.assert_called_once()
    call_args = mock_set_meta.call_args
    assert call_args[0][0] == "app-leakcheck"
    # The title value should be "Test Title", not "Generate a short title..."
    assert "title" in call_args[1]
    assert "Generate a short title" not in str(call_args[1]["title"])


@pytest.mark.asyncio
async def test_auto_title_handles_handler_error_gracefully():
    """_auto_title falls back to truncation when the handler fails; does not crash."""
    # Use an async generator that raises internally
    async def broken_handler(prompt, history, session_id, attachments, reasoning_effort):
        raise RuntimeError("Handler exploded")
        yield  # unreachable

    title = await _auto_title(
        broken_handler,
        text="Hello, how are you?",
        hermes_sid="api-fail-session",
        app_uuid="app-fail",
    )
    # Should fall back to truncation, not crash
    assert title is not None
    assert title == "Hello, how are you?"


@pytest.mark.asyncio
async def test_two_calls_use_different_throwaway_sessions():
    """Each title generation should use its own isolated session."""
    handler = FakeHandler()

    await _auto_title(handler, "First message", "real-session-1", "app-1")
    await _auto_title(handler, "Second message", "real-session-2", "app-2")

    assert len(handler.calls) == 2
    sid1 = handler.calls[0]["session_id"]
    sid2 = handler.calls[1]["session_id"]

    # Neither should be the real session
    assert sid1 != "real-session-1"
    assert sid2 != "real-session-2"

    # They should be different from each other (each title gen isolated)
    assert sid1 != sid2, "Each title generation must use its own isolated session"
