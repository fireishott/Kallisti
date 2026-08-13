from __future__ import annotations

from dataclasses import asdict, dataclass, field
from datetime import datetime
import json
import logging
import os
from pathlib import Path

logger = logging.getLogger(__name__)


def _default_state_dir() -> Path:
    configured = os.getenv("HERMES_MOBILE_CONNECTOR_HOME")
    if configured:
        return Path(configured).expanduser()
    return Path.home() / ".hermes-mobile"


@dataclass
class ConnectorRuntimeConfig:
    python_executable: str
    state_dir: str
    relay_url: str
    hermes_command: str
    hermes_workdir: str | None
    hermes_provider: str | None
    hermes_model: str | None
    hermes_toolsets: str | None
    hermes_source: str
    hermes_history_limit: int
    hermes_home: str | None = None
    api_server_url: str | None = None
    api_server_key: str | None = None


@dataclass
class RealtimeTalkConfig:
    enabled: bool = False
    preferred_models: list[str] = field(default_factory=lambda: ["gpt-realtime-1.5", "gpt-realtime"])
    voice: str = "ballad"
    turn_detection_type: str = "semantic_vad"
    create_response: bool = True
    interrupt_response: bool = True
    last_validated_at: str | None = None
    last_validation_error: str | None = None
    last_selected_model: str | None = None


@dataclass
class VoiceContextSnapshot:
    system_prompt: str
    memory_summary: str
    user_summary: str
    sensor_summary: str
    readiness_summary: str
    updated_at: str
    memory_provider_summary: str = "Memory provider status unavailable."


@dataclass
class ConnectorState:
    relay_url: str
    web_socket_url: str
    host_id: str
    connector_credential: str
    user_id: str | None = None
    device_token: str | None = None
    device_token_environment: str | None = None
    # Separate from device_token: ActivityKit's pushToken: .token gives each
    # Live Activity its own APNs token, requiring the liveactivity push type
    # and a ContentState-shaped payload — never a plain alert. Registering it
    # into device_token would silently break regular alert pushes the next
    # time a Live Activity token rotates (which happens on every chat turn).
    live_activity_push_token: str | None = None
    live_activity_push_token_environment: str | None = None
    # Build 97: every ActivityKit token the app registered for Live
    # Activities, newest first.  ActivityKit rotates the push token each
    # time a NEW activity starts, and a single-slot store could only ever
    # hold the latest — older activities' tokens were silently overwritten,
    # so their end-push never fired and the lock screen stayed stuck on
    # "Thinking..." (the "never ending live notification").  The end-push
    # now fans out to every token in this list; invalid (410) tokens are
    # pruned as they're encountered.  live_activity_push_token remains the
    # newest entry for backward compat with any code reading the single
    # slot.
    live_activity_push_tokens: list[str] = field(default_factory=list)
    connector_display_name: str | None = None
    enrolled_at: str | None = None
    last_connected_at: str | None = None
    last_error: str | None = None
    mcp_server_name: str = "hermes_mobile"
    mcp_configured: bool = False
    mcp_command_path: str | None = None
    mcp_registered_at: str | None = None
    mcp_last_test_at: str | None = None
    mcp_last_test_error: str | None = None
    runtime_config: ConnectorRuntimeConfig | None = None
    realtime_talk: RealtimeTalkConfig | None = None
    voice_context_snapshot: VoiceContextSnapshot | None = None

    @property
    def enrolled_datetime(self) -> datetime | None:
        return datetime.fromisoformat(self.enrolled_at) if self.enrolled_at else None


@dataclass
class ConnectorSecrets:
    openai_api_key: str | None = None


class ConnectorStateStore:
    def __init__(self, state_dir: Path | None = None) -> None:
        self.state_dir = (state_dir or _default_state_dir()).expanduser()
        self.state_path = self.state_dir / "state.json"
        self.secrets_path = self.state_dir / "secrets.json"

    def load(self) -> ConnectorState:
        if not self.state_path.exists():
            logger.error(
                "Connector state file not found at %s (HERMES_MOBILE_CONNECTOR_HOME=%s)",
                self.state_path,
                os.getenv("HERMES_MOBILE_CONNECTOR_HOME", "<unset>"),
            )
            raise RuntimeError(
                "Connector is not set up yet. Run `herald setup` first "
                "or use the legacy `herald enroll --code ...` flow."
            )
        data = json.loads(self.state_path.read_text(encoding="utf-8"))
        runtime_config = data.get("runtime_config")
        if isinstance(runtime_config, dict):
            data["runtime_config"] = ConnectorRuntimeConfig(**runtime_config)
        realtime_talk = data.get("realtime_talk")
        if isinstance(realtime_talk, dict):
            data["realtime_talk"] = RealtimeTalkConfig(**realtime_talk)
        voice_context_snapshot = data.get("voice_context_snapshot")
        if isinstance(voice_context_snapshot, dict):
            data["voice_context_snapshot"] = VoiceContextSnapshot(**voice_context_snapshot)
        data.setdefault(
            "mcp_configured",
            bool(data.get("mcp_registered_at") or data.get("mcp_command_path")),
        )
        return ConnectorState(**data)

    def save(self, state: ConnectorState) -> ConnectorState:
        self.state_dir.mkdir(parents=True, exist_ok=True)
        try:
            os.chmod(self.state_dir, 0o700)
        except PermissionError:
            pass

        self.state_path.write_text(json.dumps(asdict(state), indent=2, sort_keys=True), encoding="utf-8")
        try:
            os.chmod(self.state_path, 0o600)
        except PermissionError:
            pass
        return state

    def load_secrets(self) -> ConnectorSecrets:
        if not self.secrets_path.exists():
            return ConnectorSecrets()
        data = json.loads(self.secrets_path.read_text(encoding="utf-8"))
        return ConnectorSecrets(**data)

    def save_secrets(self, secrets: ConnectorSecrets) -> ConnectorSecrets:
        self.state_dir.mkdir(parents=True, exist_ok=True)
        try:
            os.chmod(self.state_dir, 0o700)
        except PermissionError:
            pass

        self.secrets_path.write_text(json.dumps(asdict(secrets), indent=2, sort_keys=True), encoding="utf-8")
        try:
            os.chmod(self.secrets_path, 0o600)
        except PermissionError:
            pass
        return secrets

    def clear(self) -> None:
        if self.state_path.exists():
            self.state_path.unlink()
        if self.secrets_path.exists():
            self.secrets_path.unlink()
        if self.state_dir.exists() and not any(self.state_dir.iterdir()):
            self.state_dir.rmdir()
