"""Kallisti 0.1.0 — hermes-native speech worker supervisor contract."""
from __future__ import annotations

import asyncio
import json
import sys
import textwrap

import pytest

from kallisti_connector.hermes_speech import HermesSpeechClient, SpeechError

STUB = textwrap.dedent(
    """
    import json, sys
    for line in sys.stdin:
        req = json.loads(line)
        op = req.get("op")
        if op == "status":
            resp = {"success": True, "stt_provider": "local", "tts_provider": "edge"}
        elif op == "transcribe":
            resp = {"success": True, "transcript": "hello world", "error": None}
        elif op == "speak":
            resp = {"success": True, "wav_path": "/tmp/fake.wav"}
        elif op == "boom":
            resp = {"success": False, "error": "synthetic failure"}
        else:
            resp = {"success": False, "error": "unknown"}
        resp["id"] = req.get("id")
        sys.stdout.write(json.dumps(resp) + "\\n")
        sys.stdout.flush()
    """
)


@pytest.fixture
def stub_client(tmp_path):
    script = tmp_path / "stub_worker.py"
    script.write_text(STUB)
    return HermesSpeechClient(
        python_bin=sys.executable,
        worker_script=str(script),
        hermes_root=str(tmp_path),
        request_timeout=5.0,
    )


class TestSpeechClient:
    @pytest.mark.asyncio
    async def test_status_roundtrip(self, stub_client):
        status = await stub_client.status()
        assert status["stt_provider"] == "local"
        assert status["tts_provider"] == "edge"

    @pytest.mark.asyncio
    async def test_transcribe_returns_text(self, stub_client):
        text = await stub_client.transcribe("/tmp/whatever.wav")
        assert text == "hello world"

    @pytest.mark.asyncio
    async def test_speak_returns_wav_path(self, stub_client):
        path = await stub_client.speak("Hello.", output_dir="/tmp")
        assert path == "/tmp/fake.wav"

    @pytest.mark.asyncio
    async def test_worker_error_raises_typed(self, stub_client):
        with pytest.raises(SpeechError) as excinfo:
            await stub_client._request({"op": "boom"})
        assert excinfo.value.code == "speechWorkerFailed"

    @pytest.mark.asyncio
    async def test_worker_restarts_after_death(self, stub_client):
        await stub_client.status()
        stub_client._proc.kill()
        await asyncio.sleep(0.1)
        status = await stub_client.status()  # supervisor must respawn
        assert status["success"] is True
