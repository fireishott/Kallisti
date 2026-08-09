from __future__ import annotations

from types import SimpleNamespace

from app.config import Settings
from app.hermes_adapter import CLIHeraldAdapter, CLIHeraldResponse, HeraldConversationMessage


def build_adapter() -> CLIHeraldAdapter:
    return CLIHeraldAdapter(
        Settings(
            environment="test",
            public_base_url="http://testserver/v1",
            database_url="sqlite://",
            internal_api_key="test-internal-key",
            herald_adapter="cli",
            herald_command="hermes",
            herald_source="tool",
        )
    )


def test_parse_cli_output_extracts_response_and_session_id():
    adapter = build_adapter()

    parsed = adapter._parse_cli_output(
        "↻ Resumed session session-123 (1 user message, 2 total messages)\n\n"
        "╭─ ⚕ Herald ───────────────────────────────────────────────────────────────────╮\n"
        "HELLO\n\n"
        "session_id: session-123\n"
    )

    assert parsed.text == "HELLO"
    assert parsed.session_id == "session-123"
    assert parsed.missing_session is False


def test_send_message_replays_when_resumed_session_is_missing(monkeypatch):
    adapter = build_adapter()
    calls: list[list[str]] = []

    monkeypatch.setattr("app.hermes_adapter.shutil.which", lambda command: f"/usr/bin/{command}")

    def fake_run(command, cwd, capture_output, text, check):
        calls.append(command)
        if "--resume" in command:
            return SimpleNamespace(
                returncode=0,
                stdout=(
                    "Session not found: missing-session\n"
                    "Use a session ID from a previous CLI run (herald sessions list).\n"
                ),
                stderr="",
            )

        return SimpleNamespace(
            returncode=0,
            stdout=(
                "╭─ ⚕ Herald ───────────────────────────────────────────────────────────────────╮\n"
                "Recovered reply\n\n"
                "session_id: recovered-session\n"
            ),
            stderr="",
        )

    monkeypatch.setattr("app.hermes_adapter.subprocess.run", fake_run)

    result = adapter.send_message(
        latest_user_message="Need help",
        history=[HeraldConversationMessage(role="user", text="Earlier context")],
        session_id="missing-session",
    )

    assert result.text == "Recovered reply"
    assert result.session_id == "recovered-session"
    assert len(calls) == 2
    assert "--resume" in calls[0]
    assert "--resume" not in calls[1]
    assert calls[1][calls[1].index("-q") + 1].startswith("You are Herald responding inside Herald.")
