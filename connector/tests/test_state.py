"""Tests for connector/src/herald_connector/state.py."""

from __future__ import annotations

import json
import os
import tempfile
from pathlib import Path
from unittest.mock import patch

import pytest

from herald_connector.state import ConnectorStateStore, _default_state_dir


class TestDefaultStateDir:
    """Tests for _default_state_dir() fallback path."""

    def test_uses_env_var_when_set(self, tmp_path):
        """HERMES_MOBILE_CONNECTOR_HOME takes precedence."""
        with patch.dict(os.environ, {"HERMES_MOBILE_CONNECTOR_HOME": str(tmp_path)}):
            assert _default_state_dir() == tmp_path

    def test_falls_back_to_hermes_mobile(self):
        """Without env var, falls back to ~/.hermes-mobile (not ~/.herald)."""
        with patch.dict(os.environ, {}, clear=True):
            # Remove the env var if it exists
            os.environ.pop("HERMES_MOBILE_CONNECTOR_HOME", None)
            result = _default_state_dir()
            assert result == Path.home() / ".hermes-mobile"
            assert ".herald" not in str(result)


class TestConnectorStateStoreLoad:
    """Tests for ConnectorStateStore.load() error handling."""

    def test_load_raises_when_state_missing(self, tmp_path):
        """load() raises RuntimeError when state.json doesn't exist."""
        store = ConnectorStateStore(state_dir=tmp_path)
        with pytest.raises(RuntimeError, match="Connector is not set up yet"):
            store.load()

    def test_load_logs_path_on_missing(self, tmp_path, caplog):
        """load() logs the resolved path at ERROR before raising."""
        import logging
        store = ConnectorStateStore(state_dir=tmp_path)
        with caplog.at_level(logging.ERROR, logger="herald_connector.state"):
            with pytest.raises(RuntimeError):
                store.load()
        assert str(tmp_path / "state.json") in caplog.text

    def test_load_succeeds_with_valid_state(self, tmp_path):
        """load() returns ConnectorState when state.json exists and is valid."""
        state_data = {
            "relay_url": "http://localhost:8010",
            "web_socket_url": "ws://localhost:8765",
            "host_id": "test-host-123",
            "connector_credential": "test-cred",
            "device_token": "test-token",
            "user_id": "test-user",
            "runtime_config": {
                "python_executable": "/usr/bin/python3",
                "state_dir": str(tmp_path),
                "relay_url": "http://localhost:8010",
                "hermes_command": "/usr/bin/hermes",
                "hermes_workdir": None,
                "hermes_provider": None,
                "hermes_model": None,
                "hermes_toolsets": None,
                "hermes_source": "test",
                "hermes_history_limit": 100,
            },
        }
        (tmp_path / "state.json").write_text(json.dumps(state_data))
        store = ConnectorStateStore(state_dir=tmp_path)
        state = store.load()
        assert state.connector_credential == "test-cred"
        assert state.relay_url == "http://localhost:8010"
