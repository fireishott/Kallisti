from __future__ import annotations

import re
import shutil
import subprocess
from dataclasses import dataclass
import os
from pathlib import Path

from .state import ConnectorRuntimeConfig


@dataclass(frozen=True)
class HeraldConversationMessage:
    role: str
    text: str


@dataclass(frozen=True)
class HeraldChatResult:
    text: str
    session_id: str | None = None
    usage: dict | None = None


@dataclass(frozen=True)
class ConnectorHeraldSettings:
    herald_command: str
    herald_workdir: str | None
    herald_provider: str | None
    herald_model: str | None
    herald_toolsets: str | None
    herald_source: str
    herald_history_limit: int

    @classmethod
    def from_env(cls) -> "ConnectorHeraldSettings":
        return cls(
            herald_command=os.getenv("HERMES_COMMAND", "hermes"),
            herald_workdir=os.getenv("HERMES_WORKDIR") or None,
            herald_provider=os.getenv("HERMES_PROVIDER") or None,
            herald_model=os.getenv("HERMES_MODEL") or None,
            herald_toolsets=os.getenv("HERMES_TOOLSETS") or None,
            herald_source=os.getenv("HERMES_SOURCE", "tool"),
            herald_history_limit=int(os.getenv("HERMES_HISTORY_LIMIT", "20")),
        )

    @classmethod
    def from_runtime_config(cls, config: ConnectorRuntimeConfig) -> "ConnectorHeraldSettings":
        return cls(
            herald_command=config.hermes_command,
            herald_workdir=config.hermes_workdir,
            herald_provider=config.hermes_provider,
            herald_model=config.hermes_model,
            herald_toolsets=config.hermes_toolsets,
            herald_source=config.hermes_source,
            herald_history_limit=config.hermes_history_limit,
        )


@dataclass(frozen=True)
class CLIHermesResponse:
    text: str
    session_id: str | None
    missing_session: bool = False


class HeraldCLIExecutor:
    SESSION_ID_PATTERN = re.compile(r"(?m)^session_id:\s*(?P<session_id>\S+)\s*$")

    def __init__(self, settings: ConnectorHeraldSettings | None = None) -> None:
        self.settings = settings or ConnectorHeraldSettings.from_env()

    def resolved_command_path(self) -> str | None:
        match = shutil.which(self.settings.herald_command)
        if match:
            return str(Path(match).resolve())

        candidate = Path(self.settings.herald_command).expanduser()
        if candidate.exists():
            return str(candidate.resolve())

        return None

    def detect_version(self) -> str | None:
        command_path = self.resolved_command_path()
        if command_path is None:
            return None

        completed = subprocess.run(
            [command_path, "--version"],
            cwd=self.settings.herald_workdir or None,
            capture_output=True,
            text=True,
            check=False,
        )
        if completed.returncode != 0:
            return None

        # The relay rejects multi-line version strings (WebSocket 4401), and
        # some hermes builds print banner text before the version — keep only
        # the first line.
        output = completed.stdout.strip() or completed.stderr.strip()
        first_line = output.splitlines()[0].strip() if output else ""
        return first_line or None

    def send_message(
        self,
        *,
        latest_user_message: str,
        history: list[HeraldConversationMessage],
        session_id: str | None = None,
    ) -> HeraldChatResult:
        if shutil.which(self.settings.herald_command) is None:
            raise RuntimeError(f"Hermes command not found: {self.settings.herald_command}")

        response = self._send_with_resume(
            latest_user_message=latest_user_message,
            history=history,
            session_id=session_id,
        )

        if response.missing_session and session_id:
            response = self._send_with_replay(latest_user_message=latest_user_message, history=history)

        if not response.text:
            raise RuntimeError("Hermes CLI returned an empty response.")

        return HeraldChatResult(text=response.text, session_id=response.session_id or session_id)

    def _send_with_resume(
        self,
        *,
        latest_user_message: str,
        history: list[HeraldConversationMessage],
        session_id: str | None,
    ) -> CLIHermesResponse:
        if session_id:
            command = self._build_command(query=latest_user_message, session_id=session_id)
            return self._run_command(command)
        return self._send_with_replay(latest_user_message=latest_user_message, history=history)

    def _send_with_replay(
        self,
        *,
        latest_user_message: str,
        history: list[HeraldConversationMessage],
    ) -> CLIHermesResponse:
        prompt = self._build_prompt(latest_user_message=latest_user_message, history=history)
        return self._run_command(self._build_command(query=prompt))

    def _build_command(self, *, query: str, session_id: str | None = None) -> list[str]:
        command = [self.settings.herald_command, "chat", "-Q", "-q", query]

        if session_id:
            command.extend(["--resume", session_id])
        if self.settings.herald_provider:
            command.extend(["--provider", self.settings.herald_provider])
        if self.settings.herald_model:
            command.extend(["--model", self.settings.herald_model])
        if self.settings.herald_toolsets:
            command.extend(["--toolsets", self.settings.herald_toolsets])
        if self.settings.herald_source:
            command.extend(["--source", self.settings.herald_source])

        return command

    def _run_command(self, command: list[str]) -> CLIHermesResponse:
        completed = subprocess.run(
            command,
            cwd=self.settings.herald_workdir or None,
            capture_output=True,
            text=True,
            check=False,
        )

        if completed.returncode != 0:
            error_text = completed.stderr.strip() or completed.stdout.strip() or "Hermes CLI request failed."
            raise RuntimeError(error_text)

        return self._parse_cli_output(completed.stdout)

    def _parse_cli_output(self, output: str) -> CLIHermesResponse:
        session_match = self.SESSION_ID_PATTERN.search(output)
        session_id = session_match.group("session_id") if session_match else None
        body = self.SESSION_ID_PATTERN.sub("", output).strip()

        lines = body.splitlines()
        while lines and self._is_metadata_line(lines[0]):
            lines.pop(0)

        text = "\n".join(lines).strip()
        return CLIHermesResponse(
            text=text,
            session_id=session_id,
            missing_session=text.startswith("Session not found:"),
        )

    def _is_metadata_line(self, line: str) -> bool:
        stripped = line.strip()
        if not stripped:
            return True
        metadata_prefixes = (
            "↻ Resumed session",
            "╭─ ⚕ Hermes",
            "Warning:",
            "⚠️",
            "⚠",
            "🔄",
            "⏳",
            "🌐",
            "📝",
            "📋",
            "✏️",
            "🧠",
            "Rate limit reached.",
            "API call failed",
            "Provider:",
            "Model:",
            "Endpoint:",
            "Error:",
            "Details:",
        )
        return any(stripped.startswith(prefix) for prefix in metadata_prefixes)

    def _build_prompt(self, *, latest_user_message: str, history: list[HeraldConversationMessage]) -> str:
        history_lines = []
        for message in history[-self.settings.herald_history_limit :]:
            prefix = "User" if message.role in ("user", "voice_user") else "Herald"
            history_lines.append(f"{prefix}: {message.text}")

        transcript = "\n".join(history_lines) if history_lines else "(no prior messages)"

        return (
            "You are Hermes responding inside Herald.\n"
            "Continue the conversation naturally using the history below.\n"
            "Return only the next assistant reply.\n\n"
            f"Conversation history:\n{transcript}\n\n"
            f"Latest user message:\nUser: {latest_user_message}"
        )
