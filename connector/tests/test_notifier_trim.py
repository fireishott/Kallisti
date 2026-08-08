"""Verify the connector is trimmed to push/Live-Activity-only notifier.

Chat-facing REST+SSE routes (/v1/runs, /v1/messages, /v1/jobs/*) must
return 404.  Push/Live-Activity routes (/v1/push/register) must still work.
"""
from unittest.mock import AsyncMock, patch

import pytest
from starlette.testclient import TestClient

from kallisti_connector.http_facade import FacadeContext, app


@pytest.fixture
def client():
    with TestClient(app) as c:
        yield c


@pytest.fixture(autouse=True)
def auth():
    with patch("kallisti_connector.http_facade.require_auth", new_callable=AsyncMock):
        yield


@pytest.fixture(autouse=True)
def ctx():
    facade_ctx = FacadeContext()
    facade_ctx.connector_version = "0.1.0"
    with patch("kallisti_connector.http_facade.get_context", return_value=facade_ctx):
        yield facade_ctx


# ── Chat routes must be gone ────────────────────────────────────────────────


def test_v1_runs_returns_404(client):
    resp = client.post("/v1/runs", json={"session_id": "x", "text": "hi"})
    assert resp.status_code == 404


def test_v1_messages_returns_404(client):
    resp = client.post("/v1/messages", json={"text": "hi"})
    assert resp.status_code == 404


def test_v1_jobs_returns_404(client):
    resp = client.get("/v1/jobs/some-id")
    assert resp.status_code == 404


def test_v1_jobs_events_returns_404(client):
    resp = client.get("/v1/jobs/some-id/events")
    assert resp.status_code == 404


def test_v1_jobs_cancel_returns_404(client):
    resp = client.post("/v1/jobs/some-id/cancel")
    assert resp.status_code == 404


def test_v1_conversations_current_returns_404(client):
    resp = client.get("/v1/conversations/current")
    assert resp.status_code == 404


def test_v1_conversations_ensure_returns_404(client):
    resp = client.post("/v1/conversations/ensure", json={})
    assert resp.status_code == 404


def test_v1_sessions_conversation_returns_404(client):
    resp = client.get("/v1/sessions/some-id/conversation")
    assert resp.status_code == 404


# ── Push / Live-Activity routes must still work ─────────────────────────────


def test_push_register_still_works(client):
    """push_register endpoint must NOT be removed."""
    resp = client.post("/v1/push/register", json={"deviceToken": "abc", "tokenKind": "alert"})
    # Should not be 404 — it's a valid route.
    assert resp.status_code != 404


# ── Infrastructure routes must still work ────────────────────────────────────


def test_health_still_works(client):
    resp = client.get("/v1/health")
    assert resp.status_code == 200


def test_version_still_works(client):
    resp = client.get("/v1/version")
    assert resp.status_code == 200


def test_models_still_works(client):
    resp = client.get("/v1/models")
    assert resp.status_code == 200


def test_gateway_status_still_works(client):
    resp = client.get("/gw/status")
    assert resp.status_code == 200
