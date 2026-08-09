from __future__ import annotations

from fastapi.testclient import TestClient

from app.config import Settings
from app.main import create_app


def build_client(tmp_path):
    settings = Settings(
        environment="test",
        public_base_url="http://testserver/v1",
        database_url=f"sqlite:///{tmp_path / 'relay.db'}",
        internal_api_key="test-internal-key",
    )
    app = create_app(settings)
    return TestClient(app)


def test_v1_health_returns_ok(tmp_path):
    """GET /v1/health should return 200 with status 'ok'."""
    with build_client(tmp_path) as client:
        response = client.get("/v1/health")
        assert response.status_code == 200
        body = response.json()
        assert body["data"]["status"] == "ok"
        assert body["data"]["database"] is True


def test_health_alias_returns_ok(tmp_path):
    """GET /health should return 200 with status 'ok' (alias for /v1/health)."""
    with build_client(tmp_path) as client:
        response = client.get("/health")
        assert response.status_code == 200
        body = response.json()
        assert body["data"]["status"] == "ok"
        assert body["data"]["database"] is True


def test_health_alias_matches_v1_health(tmp_path):
    """Both endpoints should return the same data payload."""
    with build_client(tmp_path) as client:
        v1 = client.get("/v1/health").json()
        alias = client.get("/health").json()
        assert v1["data"] == alias["data"]
