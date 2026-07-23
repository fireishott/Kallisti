from __future__ import annotations

import asyncio

import httpx

from herald_connector.herald_api_executor import HeraldAPIExecutor


def test_health_check_hits_v1_health(monkeypatch):
    """health_check() should GET /v1/health, not /health."""
    captured_urls: list[str] = []

    class FakeAsyncClient:
        def __init__(self, **kwargs):
            pass

        async def __aenter__(self):
            return self

        async def __aexit__(self, *args):
            pass

        async def get(self, url, headers=None):
            captured_urls.append(url)

            class FakeResponse:
                status_code = 200

                def json(self):
                    return {"data": {"status": "ok", "database": True}, "meta": {}}

            return FakeResponse()

    monkeypatch.setattr("herald_connector.herald_api_executor.httpx.AsyncClient", FakeAsyncClient)

    executor = HeraldAPIExecutor(api_server_url="http://localhost:8642", api_server_key="test-key")
    result = asyncio.run(executor.health_check())

    assert result is True
    assert len(captured_urls) == 1
    assert captured_urls[0].endswith("/v1/health"), (
        f"Expected URL ending with /v1/health, got: {captured_urls[0]}"
    )
