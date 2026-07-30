"""Tests for session_conversation endpoint normalization.

D1 fix: the connector RPC returns a flat {sessionId, messages, title} but the
app requires {conversation: {id: UUID, title, updatedAt, messages, ...}}.
"""

from __future__ import annotations

import json
import uuid
from unittest.mock import AsyncMock, MagicMock, patch

import pytest
from starlette.testclient import TestClient

from herald_connector.http_facade import app, get_context, FacadeContext


@pytest.fixture
def client():
    return TestClient(app)


@pytest.fixture
def mock_auth():
    """Mock require_auth to always pass."""
    with patch("herald_connector.http_facade.require_auth", new_callable=AsyncMock):
        yield


def _make_ctx(session_conversation_fn):
    """Create a FacadeContext with the given session_conversation provider."""
    ctx = FacadeContext()
    ctx.session_conversation = session_conversation_fn
    ctx.connector_version = "0.4.1"
    return ctx


class TestSessionConversationWrapsFlatPayload:
    """D1: flat {sessionId, messages, title} must be wrapped in {conversation: {...}}."""

    def test_wraps_flat_payload_with_uuid(self, client, mock_auth):
        """Flat payload with UUID sessionId → conversation.id matches."""
        session_id = str(uuid.uuid4())
        flat = {"sessionId": session_id, "messages": [], "title": None}

        with patch("herald_connector.http_facade.get_context", return_value=_make_ctx(lambda sid: flat)):
            resp = client.get(f"/v1/sessions/{session_id}/conversation")

        assert resp.status_code == 200
        data = resp.json()
        assert "conversation" in data
        assert data["conversation"]["id"] == session_id
        assert data["conversation"]["title"] == "New Chat"
        assert data["conversation"]["messages"] == []

    def test_wraps_flat_payload_with_non_uuid_session_id(self, client, mock_auth):
        """Non-UUID sessionId (e.g. 'api-9af38ce') → fallback UUID generated."""
        flat = {"sessionId": "api-9af38ce", "messages": [], "title": "Test"}

        session_id = str(uuid.uuid4())
        with patch("herald_connector.http_facade.get_context", return_value=_make_ctx(lambda sid: flat)):
            resp = client.get(f"/v1/sessions/{session_id}/conversation")

        assert resp.status_code == 200
        data = resp.json()
        # Should be a valid UUID, not the raw "api-9af38ce"
        parsed = uuid.UUID(data["conversation"]["id"])
        assert parsed is not None

    def test_wraps_empty_provider_result(self, client, mock_auth):
        """Provider returns {} → 200 with decodable conversation."""
        session_id = str(uuid.uuid4())
        with patch("herald_connector.http_facade.get_context", return_value=_make_ctx(lambda sid: {})):
            resp = client.get(f"/v1/sessions/{session_id}/conversation")

        assert resp.status_code == 200
        data = resp.json()
        assert "conversation" in data
        assert "id" in data["conversation"]
        assert "messages" in data["conversation"]

    def test_wraps_none_provider_result(self, client, mock_auth):
        """Provider returns None → 200 with decodable conversation."""
        session_id = str(uuid.uuid4())
        with patch("herald_connector.http_facade.get_context", return_value=_make_ctx(lambda sid: None)):
            resp = client.get(f"/v1/sessions/{session_id}/conversation")

        assert resp.status_code == 200
        data = resp.json()
        assert "conversation" in data


class TestSessionConversationPassesThroughPrewrapped:
    """D1: if provider already returns {conversation: {...}}, don't double-wrap."""

    def test_passes_through_prewrapped(self, client, mock_auth):
        """Prewrapped payload passes through without double-wrapping."""
        session_id = str(uuid.uuid4())
        prewrapped = {
            "conversation": {
                "id": session_id,
                "title": "Existing Chat",
                "updatedAt": "2026-07-29T10:00:00Z",
                "messages": [{"id": "msg-1", "content": "hello"}],
                "latestUsage": None,
                "latestContext": None,
            }
        }

        with patch("herald_connector.http_facade.get_context", return_value=_make_ctx(lambda sid: prewrapped)):
            resp = client.get(f"/v1/sessions/{session_id}/conversation")

        assert resp.status_code == 200
        data = resp.json()
        # Should NOT be double-wrapped (no data.conversation.conversation)
        assert "conversation" not in data["conversation"]
        assert data["conversation"]["id"] == session_id
        assert data["conversation"]["title"] == "Existing Chat"


class TestSessionConversationEdgeCases:
    """Edge cases for session_conversation normalization."""

    def test_session_id_from_path_param_takes_precedence(self, client, mock_auth):
        """Path param UUID is preferred over result.sessionId for conversation.id."""
        path_id = str(uuid.uuid4())
        result_id = str(uuid.uuid4())
        flat = {"sessionId": result_id, "messages": [], "title": None}

        with patch("herald_connector.http_facade.get_context", return_value=_make_ctx(lambda sid: flat)):
            resp = client.get(f"/v1/sessions/{path_id}/conversation")

        assert resp.status_code == 200
        data = resp.json()
        # Path param should take precedence
        assert data["conversation"]["id"] == path_id

    def test_503_when_provider_not_available(self, client, mock_auth):
        """Returns 503 when session_conversation is None."""
        ctx = FacadeContext()
        ctx.session_conversation = None

        with patch("herald_connector.http_facade.get_context", return_value=ctx):
            resp = client.get(f"/v1/sessions/{uuid.uuid4()}/conversation")

        assert resp.status_code == 503
