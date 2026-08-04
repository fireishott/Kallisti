"""Build 104 P0 — server-side Xiaomi MiMo ASR/TTS proxy contract.

The proxy is the source of truth for the Xiaomi endpoint and the
header shape.  The iOS app and the connector share this contract:
iOS posts to ``/v1/mimo/asr`` and the connector forwards to
``api.xiaomimimo.com/v1/chat/completions`` with the documented
``input_audio`` content part.  These tests cover the request
transformation and the typed error surfaces; the network round-trip
itself is exercised by the live canary after deployment.
"""

from __future__ import annotations

import base64
import json
import os
from pathlib import Path
from unittest.mock import AsyncMock, patch

import pytest

import kallisti_connector.mimo_proxy as mimo


# ── Key loading ───────────────────────────────────────────────────────────


class TestKeyLoading:
    def test_no_env_file_returns_none(self, tmp_path, monkeypatch):
        monkeypatch.setenv("HERALD_MIMO_ENV_PATH", str(tmp_path / "missing.env"))
        assert mimo._load_mimo_api_key() is None

    def test_env_file_with_key_returns_value(self, tmp_path, monkeypatch):
        env_path = tmp_path / "mimo.env"
        env_path.write_text(
            "# comment line\n"
            "OTHER=value\n"
            "HERALD_MIMO_API_KEY=sk-test-12345\n"
        )
        monkeypatch.setenv("HERALD_MIMO_ENV_PATH", str(env_path))
        assert mimo._load_mimo_api_key() == "sk-test-12345"

    def test_env_file_with_quoted_key(self, tmp_path, monkeypatch):
        env_path = tmp_path / "mimo.env"
        env_path.write_text("HERALD_MIMO_API_KEY=\"quoted-secret\"\n")
        monkeypatch.setenv("HERALD_MIMO_ENV_PATH", str(env_path))
        assert mimo._load_mimo_api_key() == "quoted-secret"

    def test_env_file_empty_key_returns_none(self, tmp_path, monkeypatch):
        env_path = tmp_path / "mimo.env"
        env_path.write_text("HERALD_MIMO_API_KEY=\n")
        monkeypatch.setenv("HERALD_MIMO_ENV_PATH", str(env_path))
        assert mimo._load_mimo_api_key() is None


# ── Request body shape ───────────────────────────────────────────────────


class TestChatCompletionsBody:
    def test_body_shape_matches_documented_contract(self):
        audio = b"RIFF\x24\x00\x00\x00WAVE" + b"\x00" * 100
        body = mimo._wav_to_chat_completions(audio_bytes=audio)
        assert body["model"] == "mimo-v2.5-asr"
        assert body["stream"] is True
        assert body["asr_options"] == {"language": "auto"}
        assert isinstance(body["messages"], list) and len(body["messages"]) == 1
        msg = body["messages"][0]
        assert msg["role"] == "user"
        assert isinstance(msg["content"], list) and len(msg["content"]) == 1
        part = msg["content"][0]
        assert part["type"] == "input_audio"
        data = part["input_audio"]["data"]
        # The data is a data: URL with base64 of the input bytes.
        assert data.startswith("data:audio/wav;base64,")
        recovered = base64.b64decode(
            data.split(",", 1)[1]
        )
        assert recovered == audio
        assert part["input_audio"]["format"] == "wav"

    def test_mp3_mime_type(self):
        body = mimo._wav_to_chat_completions(
            audio_bytes=b"\x00" * 100,
            mime_type="audio/mpeg",
        )
        assert body["messages"][0]["content"][0]["input_audio"]["format"] == "mp3"


# ── Error envelope shape ─────────────────────────────────────────────────


class TestErrorEnvelope:
    def test_proxy_error_payload(self):
        exc = mimo.MimoProxyError("mimoUpstreamError", "boom", status_code=502)
        payload = mimo.proxy_error_payload(exc)
        assert payload["$schema"] == "mimo-error-v1"
        assert payload["error"] == "mimoUpstreamError"
        assert payload["message"] == "boom"
        assert payload["detail"] == {}


# ── Transcribe behaviour ─────────────────────────────────────────────────


class TestTranscribeAudio:
    @pytest.mark.asyncio
    async def test_no_key_raises_typed_error(self, monkeypatch, tmp_path):
        monkeypatch.setenv("HERALD_MIMO_ENV_PATH", str(tmp_path / "missing.env"))
        audio = b"RIFF\x24\x00\x00\x00WAVE" + b"\x00" * 400
        with pytest.raises(mimo.MimoProxyError) as exc:
            async for _ in mimo.transcribe_audio(audio_bytes=audio):
                pass
        assert exc.value.code == "mimoNotConfigured"
        assert exc.value.status_code == 503

    @pytest.mark.asyncio
    async def test_too_short_raises_typed_error(self, tmp_path, monkeypatch):
        env_path = tmp_path / "mimo.env"
        env_path.write_text("HERALD_MIMO_API_KEY=sk-test\n")
        monkeypatch.setenv("HERALD_MIMO_ENV_PATH", str(env_path))
        with pytest.raises(mimo.MimoProxyError) as exc:
            async for _ in mimo.transcribe_audio(audio_bytes=b"\x00" * 10):
                pass
        assert exc.value.code == "mimoAudioTooShort"
        assert exc.value.status_code == 400

    @pytest.mark.asyncio
    async def test_stream_without_final_raises_no_final(self, tmp_path, monkeypatch):
        env_path = tmp_path / "mimo.env"
        env_path.write_text("HERALD_MIMO_API_KEY=sk-test\n")
        monkeypatch.setenv("HERALD_MIMO_ENV_PATH", str(env_path))

        # Stub the upstream to return a single delta and no final.
        async def _fake_stream(body, *, api_key):
            yield {"type": "delta", "text": "hi"}

        monkeypatch.setattr(
            mimo, "_stream_chat_completions_to_ndjson", _fake_stream
        )
        audio = b"RIFF\x24\x00\x00\x00WAVE" + b"\x00" * 400
        with pytest.raises(mimo.MimoProxyError) as exc:
            async for _ in mimo.transcribe_audio(audio_bytes=audio):
                pass
        assert exc.value.code == "mimoNoFinalTranscript"
        assert exc.value.status_code == 502

    @pytest.mark.asyncio
    async def test_full_stream_emits_deltas_and_final(self, tmp_path, monkeypatch):
        env_path = tmp_path / "mimo.env"
        env_path.write_text("HERALD_MIMO_API_KEY=sk-test\n")
        monkeypatch.setenv("HERALD_MIMO_ENV_PATH", str(env_path))

        async def _fake_stream(body, *, api_key):
            yield {"type": "delta", "text": "He"}
            yield {"type": "delta", "text": "llo"}
            yield {"type": "final", "text": "Hello"}

        monkeypatch.setattr(
            mimo, "_stream_chat_completions_to_ndjson", _fake_stream
        )
        audio = b"RIFF\x24\x00\x00\x00WAVE" + b"\x00" * 400
        events = []
        async for event in mimo.transcribe_audio(audio_bytes=audio):
            events.append(event)
        assert events == [
            {"type": "delta", "text": "He"},
            {"type": "delta", "text": "llo"},
            {"type": "final", "text": "Hello"},
        ]


# ── Readiness probe ──────────────────────────────────────────────────────


class TestProbeUpstreamReachable:
    @pytest.mark.asyncio
    async def test_no_key_returns_blocked(self, tmp_path, monkeypatch):
        monkeypatch.setenv("HERALD_MIMO_ENV_PATH", str(tmp_path / "missing.env"))
        ok, reason = await mimo.probe_upstream_reachable()
        assert ok is False
        assert "not configured" in reason.lower()

    @pytest.mark.asyncio
    async def test_rejected_key_blocks_talk_readiness(self, tmp_path, monkeypatch):
        env_path = tmp_path / "mimo.env"
        env_path.write_text("HERALD_MIMO_API_KEY=sk-test\n")
        monkeypatch.setenv("HERALD_MIMO_ENV_PATH", str(env_path))

        class _Resp:
            status_code = 401
        class _Client:
            async def __aenter__(self):
                return self
            async def __aexit__(self, *a):
                return False
            async def request(self, *a, **k):
                return _Resp()
        monkeypatch.setattr(
            mimo, "httpx", type("X", (), {"AsyncClient": lambda *a, **k: _Client()})
        )
        ok, reason = await mimo.probe_upstream_reachable()
        assert ok is False
        assert "rejected" in reason.lower()
