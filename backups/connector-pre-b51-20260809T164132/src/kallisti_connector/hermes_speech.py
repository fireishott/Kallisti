"""Supervisor/client for the hermes-native speech worker (Kallisti 0.1.0).

Spawns speech_worker.py under the HERMES venv python, keeps it warm,
serializes requests (single-user device), restarts it on crash, and
maps failures to typed SpeechError codes the facade returns verbatim.
"""
from __future__ import annotations

import asyncio
import json
import logging
import os
from typing import Any

logger = logging.getLogger("kallisti.hermes_speech")

DEFAULT_HERMES_ROOT = os.path.expanduser("~/.hermes/hermes-agent")
DEFAULT_PYTHON = os.path.join(DEFAULT_HERMES_ROOT, "venv", "bin", "python")
DEFAULT_WORKER = os.path.join(os.path.dirname(__file__), "speech_worker.py")


class SpeechError(Exception):
    def __init__(self, code: str, message: str, *, status_code: int = 502) -> None:
        super().__init__(message)
        self.code = code
        self.message = message
        self.status_code = status_code

    def payload(self) -> dict[str, Any]:
        return {"$schema": "talk-error-v1", "error": self.code, "message": self.message}


class HermesSpeechClient:
    def __init__(
        self,
        *,
        python_bin: str | None = None,
        worker_script: str | None = None,
        hermes_root: str | None = None,
        request_timeout: float = 60.0,
    ) -> None:
        self._python = python_bin or os.getenv("KALLISTI_SPEECH_PYTHON", DEFAULT_PYTHON)
        self._script = worker_script or DEFAULT_WORKER
        self._root = hermes_root or os.getenv("KALLISTI_SPEECH_HERMES_ROOT", DEFAULT_HERMES_ROOT)
        self._timeout = request_timeout
        self._proc: asyncio.subprocess.Process | None = None
        self._lock = asyncio.Lock()
        self._next_id = 0

    async def _ensure_proc(self) -> asyncio.subprocess.Process:
        if self._proc is not None and self._proc.returncode is None:
            return self._proc
        if not os.path.exists(self._python):
            raise SpeechError(
                "speechNotConfigured",
                f"Hermes python not found at {self._python}.",
                status_code=503,
            )
        env = dict(os.environ, HERMES_AGENT_ROOT=self._root)
        self._proc = await asyncio.create_subprocess_exec(
            self._python, self._script,
            stdin=asyncio.subprocess.PIPE,
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.DEVNULL,
            cwd=self._root,
            env=env,
        )
        logger.info("speech worker started (pid %s)", self._proc.pid)
        return self._proc

    async def _request(self, body: dict[str, Any]) -> dict[str, Any]:
        async with self._lock:
            self._next_id += 1
            body = dict(body, id=self._next_id)
            try:
                proc = await self._ensure_proc()
                assert proc.stdin is not None and proc.stdout is not None
                proc.stdin.write((json.dumps(body) + "\n").encode("utf-8"))
                await proc.stdin.drain()
                raw = await asyncio.wait_for(proc.stdout.readline(), self._timeout)
            except SpeechError:
                raise
            except asyncio.TimeoutError as exc:
                await self._kill()
                raise SpeechError(
                    "speechTimeout", "Speech worker timed out.", status_code=504,
                ) from exc
            except Exception as exc:  # noqa: BLE001
                await self._kill()
                raise SpeechError(
                    "speechWorkerFailed", f"Speech worker I/O failed: {exc!r}",
                ) from exc
            if not raw:
                await self._kill()
                raise SpeechError("speechWorkerFailed", "Speech worker exited mid-request.")
            try:
                resp = json.loads(raw.decode("utf-8"))
            except ValueError as exc:
                await self._kill()
                raise SpeechError(
                    "speechWorkerFailed", "Speech worker emitted a non-JSON line.",
                ) from exc
            if not resp.get("success"):
                raise SpeechError(
                    "speechWorkerFailed", str(resp.get("error") or "Speech worker failed."),
                )
            return resp

    async def _kill(self) -> None:
        if self._proc is not None and self._proc.returncode is None:
            self._proc.kill()
        self._proc = None

    async def status(self) -> dict[str, Any]:
        return await self._request({"op": "status"})

    async def transcribe(self, wav_path: str) -> str:
        resp = await self._request({"op": "transcribe", "path": wav_path})
        return str(resp.get("transcript", ""))

    async def speak(self, text: str, *, output_dir: str) -> str:
        resp = await self._request({"op": "speak", "text": text, "output_dir": output_dir})
        return str(resp["wav_path"])


_client: HermesSpeechClient | None = None


def get_speech_client() -> HermesSpeechClient:
    global _client
    if _client is None:
        _client = HermesSpeechClient()
    return _client
