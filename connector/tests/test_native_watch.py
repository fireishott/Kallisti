"""Tests for the native-completion to push bridge (Task 12)."""

from kallisti_connector.native_watch import NativeWatchRegistry


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
