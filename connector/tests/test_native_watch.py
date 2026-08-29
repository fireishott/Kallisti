"""Tests for the native-completion to push bridge (Task 12)."""

import json
import asyncio
from pathlib import Path
from unittest.mock import AsyncMock, MagicMock, patch

import pytest

from kallisti_connector.native_watch import (
    NativeWatchRegistry,
    NativeWatchTokens,
    CONFIG_PATH,
)


# ---------------------------------------------------------------------------
# NativeWatchRegistry
# ---------------------------------------------------------------------------

def test_registry_maps_session_to_device():
    registry = NativeWatchRegistry()
    registry.watch(session_id="s1", device_token="dev-abc", token_kind="alert")
    assert registry.watchers_for("s1") == [("dev-abc", "alert")]


def test_registry_returns_empty_for_unwatched_session():
    registry = NativeWatchRegistry()
    assert registry.watchers_for("unknown") == []


def test_registry_supports_multiple_devices_per_session():
    registry = NativeWatchRegistry()
    registry.watch(session_id="s1", device_token="dev-a", token_kind="alert")
    registry.watch(session_id="s1", device_token="dev-b", token_kind="liveActivity")
    assert set(registry.watchers_for("s1")) == {("dev-a", "alert"), ("dev-b", "liveActivity")}


def test_registry_deduplicates_identical_entries():
    registry = NativeWatchRegistry()
    registry.watch(session_id="s1", device_token="dev-a", token_kind="alert")
    registry.watch(session_id="s1", device_token="dev-a", token_kind="alert")
    assert registry.watchers_for("s1") == [("dev-a", "alert")]


# --- unwatch ---------------------------------------------------------------

def test_unwatch_removes_specific_entry():
    registry = NativeWatchRegistry()
    registry.watch(session_id="s1", device_token="dev-a", token_kind="alert")
    registry.watch(session_id="s1", device_token="dev-a", token_kind="liveActivity")
    registry.unwatch(session_id="s1", device_token="dev-a", token_kind="alert")
    assert registry.watchers_for("s1") == [("dev-a", "liveActivity")]


def test_unwatch_cleans_up_session_key_when_last_entry_removed():
    registry = NativeWatchRegistry()
    registry.watch(session_id="s1", device_token="dev-a", token_kind="alert")
    registry.unwatch(session_id="s1", device_token="dev-a", token_kind="alert")
    assert registry.watchers_for("s1") == []
    # session key should be fully removed, not just empty-listed
    assert "s1" not in registry._watchers


def test_unwatch_noops_on_unknown_session():
    registry = NativeWatchRegistry()
    # should not raise
    registry.unwatch(session_id="nope", device_token="dev", token_kind="alert")


def test_unwatch_noops_on_nonexistent_entry():
    registry = NativeWatchRegistry()
    registry.watch(session_id="s1", device_token="dev-a", token_kind="alert")
    registry.unwatch(session_id="s1", device_token="dev-a", token_kind="liveActivity")
    # original entry still intact
    assert registry.watchers_for("s1") == [("dev-a", "alert")]


# ---------------------------------------------------------------------------
# NativeWatchTokens -- graceful missing-config handling
# ---------------------------------------------------------------------------

def test_tokens_load_survives_missing_config(tmp_path, monkeypatch):
    """_load() should not crash when the config file does not exist."""
    missing = tmp_path / "nonexistent.json"
    monkeypatch.setattr(
        "kallisti_connector.native_watch.CONFIG_PATH", missing
    )
    session = MagicMock()
    tokens = NativeWatchTokens(session=session)
    assert tokens._refresh_token is None


def test_tokens_load_reads_refresh_token(tmp_path, monkeypatch):
    """_load() should read the refresh_token from a valid config file."""
    cfg = tmp_path / "kallisti-native-watch.json"
    cfg.write_text(json.dumps({"refresh_token": "tok-123"}))
    monkeypatch.setattr("kallisti_connector.native_watch.CONFIG_PATH", cfg)
    session = MagicMock()
    tokens = NativeWatchTokens(session=session)
    assert tokens._refresh_token == "tok-123"


@pytest.mark.asyncio
async def test_tokens_access_token_raises_when_no_refresh_token(tmp_path, monkeypatch):
    """access_token() should raise a clear RuntimeError when no refresh token
    was loaded (config missing)."""
    missing = tmp_path / "nonexistent.json"
    monkeypatch.setattr("kallisti_connector.native_watch.CONFIG_PATH", missing)
    session = MagicMock()
    tokens = NativeWatchTokens(session=session)

    with pytest.raises(RuntimeError, match="no refresh token loaded"):
        await tokens.access_token()


@pytest.mark.asyncio
async def test_tokens_access_token_refreshes_on_first_call(tmp_path, monkeypatch):
    """access_token() should call _refresh() when _access_token is None."""
    cfg = tmp_path / "kallisti-native-watch.json"
    cfg.write_text(json.dumps({"refresh_token": "tok-123"}))
    monkeypatch.setattr("kallisti_connector.native_watch.CONFIG_PATH", cfg)

    mock_resp = MagicMock()
    mock_resp.status_code = 200
    mock_resp.json.return_value = {
        "access_token": "access-456",
        "refresh_token": "refresh-new",
    }

    # Production uses httpx: post() is awaited and returns a Response.
    session = MagicMock()
    session.post = AsyncMock(return_value=mock_resp)

    tokens = NativeWatchTokens(session=session)
    token = await tokens.access_token()
    assert token == "access-456"
    # second call should return cached value without another refresh
    token2 = await tokens.access_token()
    assert token2 == "access-456"
    # session.post should have been called exactly once
    assert session.post.call_count == 1
