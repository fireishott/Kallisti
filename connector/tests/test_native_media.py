from pathlib import Path
from types import SimpleNamespace
from unittest.mock import AsyncMock, patch

from starlette.testclient import TestClient

from kallisti_connector.http_facade import app


def test_native_media_serves_generated_image_and_blocks_arbitrary_file(tmp_path, monkeypatch):
    hermes_home = tmp_path / ".hermes"
    generated = hermes_home / "profiles" / "ignyte" / "cache" / "images" / "funny.png"
    generated.parent.mkdir(parents=True)
    generated.write_bytes(b"\x89PNG\r\n\x1a\nimage-bytes")
    blocked = tmp_path / "secret.png"
    blocked.write_bytes(b"not-for-mobile")
    monkeypatch.setenv("HERMES_HOME", str(hermes_home))

    with patch(
        "kallisti_connector.http_facade.require_native_or_paired_auth",
        new=AsyncMock(return_value="token"),
    ):
        with TestClient(app) as client:
            response = client.get("/v1/native/media", params={"path": str(generated)})
            assert response.status_code == 200
            assert response.content == generated.read_bytes()
            assert response.headers["content-type"] == "image/png"

            denied = client.get("/v1/native/media", params={"path": str(blocked)})
            assert denied.status_code == 403


def test_native_media_resolves_stable_and_removed_legacy_profile_paths(tmp_path, monkeypatch):
    hermes_home = tmp_path / ".hermes"
    current = hermes_home / "images" / "old-photo.jpg"
    current.parent.mkdir(parents=True)
    current.write_bytes(b"jpeg-bytes")
    monkeypatch.setenv("HERMES_HOME", str(hermes_home))
    removed_legacy = hermes_home / "profiles" / "ignyte" / "images" / current.name

    with patch(
        "kallisti_connector.http_facade.require_native_or_paired_auth",
        new=AsyncMock(return_value="token"),
    ):
        with TestClient(app) as client:
            stable = client.get("/v1/native/media", params={"path": "images/old-photo.jpg"})
            legacy = client.get("/v1/native/media", params={"path": str(removed_legacy)})
            traversal = client.get("/v1/native/media", params={"path": "images/../secret.jpg"})
            untyped = client.get("/v1/native/media", params={"path": "old-photo.jpg"})

    assert stable.status_code == 200
    assert stable.content == current.read_bytes()
    assert legacy.status_code == 200
    assert legacy.content == current.read_bytes()
    assert traversal.status_code == 403
    assert untyped.status_code in {403, 415}


def test_native_media_profile_overlay_falls_back_to_consolidated_images(tmp_path, monkeypatch):
    base_home = tmp_path / ".hermes"
    profile_home = base_home / "profiles" / "ignyte"
    profile_home.mkdir(parents=True)
    image = base_home / "images" / "mark_stoned_cartoon.png"
    image.parent.mkdir(parents=True)
    image.write_bytes(b"\x89PNG\r\n\x1a\nimage-bytes")
    monkeypatch.setenv("HERMES_HOME", str(profile_home))

    with patch(
        "kallisti_connector.http_facade.require_native_or_paired_auth",
        new=AsyncMock(return_value="token"),
    ):
        with TestClient(app) as client:
            response = client.get(
                "/v1/native/media",
                params={"path": "images/mark_stoned_cartoon.png"},
            )

    assert response.status_code == 200
    assert response.content == image.read_bytes()
    assert response.headers["content-type"] == "image/png"


def test_native_auth_forwards_gateway_session_cookie():
    from kallisti_connector.http_facade import require_native_or_paired_auth

    request = SimpleNamespace(
        headers={"cookie": "hermes_session=session-value"},
    )
    response = SimpleNamespace(status_code=200)
    mock_client = AsyncMock()
    mock_client.get.return_value = response
    mock_context = AsyncMock()
    mock_context.__aenter__.return_value = mock_client

    with patch("httpx.AsyncClient", return_value=mock_context):
        token = __import__("asyncio").run(require_native_or_paired_auth(request))

    assert token == "gateway-cookie-session"
    mock_client.get.assert_awaited_once_with(
        "http://127.0.0.1:9119/api/auth/me",
        headers={"Cookie": "hermes_session=session-value"},
    )


def test_push_register_uses_native_or_paired_auth_not_bare_require_auth(tmp_path, monkeypatch):
    """Build 51 native clients send the gateway OAuth bearer token.

    push/register must accept it via require_native_or_paired_auth (same dual
    path as media) instead of bare require_auth (connector tokens only),
    otherwise the phone 401s even with Caddy routing fixed. Regression guard:
    if the route ever reverts to require_auth, this test fails because the
    mocked require_auth raises 401.
    """
    import kallisti_connector.http_facade as facade

    async def boom(request):
        raise Exception("route used bare require_auth - regression")

    with patch(
        "kallisti_connector.http_facade.require_native_or_paired_auth",
        new=AsyncMock(return_value="native-bearer"),
    ), patch(
        "kallisti_connector.http_facade.require_auth",
        new=boom,
    ), patch(
        "kallisti_connector.http_facade.get_context",
        new=lambda: SimpleNamespace(
            push_register=AsyncMock(return_value={"registered": True})
        ),
    ):
        with TestClient(app) as client:
            response = client.post(
                "/v1/push/register",
                json={
                    "apnsToken": "apns-test-token",
                    "pushEnvironment": "production",
                    "tokenKind": "device",
                },
            )
            assert response.status_code == 200
            assert response.json() == {"registered": True, "environment": "production"}
