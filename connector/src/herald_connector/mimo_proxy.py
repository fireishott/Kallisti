"""Server-side Xiaomi MiMo ASR/TTS proxy for Herald iOS.

Build 104: the iOS app no longer talks to ``api.xiaomimimo.com``
directly. Instead it posts to ``/v1/mimo/asr`` and ``/v1/mimo/tts``
on the connector (already behind the existing bearer-token
authentication), and the connector forwards the request using a
key it owns in ``~/.config/herald-mimo.env`` (mode 0600).

Xiaomi's published MiMo V2.5 audio contract is
``POST /v1/chat/completions`` with a JSON body that carries
``messages[].content[].type == "input_audio"`` and a Base64
data URL inside ``input_audio.data``. See
https://mimo.mi.com/docs/en-US/api/audio/Speech-Recognition
and https://mimo.mi.com/docs/en-US/usage-guide/Speech-Recognition.

The proxy never echoes the upstream key in a log line or an
HTTP response body. Errors are typed: ``mimoNotConfigured``
(no key on disk), ``mimoUpstreamError`` (4xx/5xx from Xiaomi),
``mimoUpstreamUnreachable`` (network/timeout), ``mimoNoFinalTranscript``
(stream ended without a final), ``mimoAudioTooShort`` (recorded audio
under a sensible minimum).
"""

from __future__ import annotations

import base64
import json
import logging
import os
import time
from pathlib import Path
from typing import Any, AsyncIterator

import httpx

logger = logging.getLogger("herald.mimo_proxy")

# Xiaomi's published MiMo V2.5 base URL. Override via env for tests.
DEFAULT_MIMO_BASE_URL = "https://api.xiaomimimo.com"

# Where the key is read from. The file is mode 0600; the connector
# service runs as the user that owns it.
DEFAULT_MIMO_ENV_PATH = ".config/herald-mimo.env"

# Streaming chunk target size for NDJSON output. Apple apps expect
# line-delimited JSON like ``{"type":"delta","text":"…"}`` /
# ``{"type":"final","text":"…"}``.
_ASR_STREAM_CHUNK = 4096

# Minimum acceptable WAV payload size. Anything smaller is treated as
# a non-speech or "user let go before saying anything" artifact.
_MIN_WAV_BYTES = 320

# Default timeout for the upstream call.  Xiaomi's audio models can
# take several seconds for long utterances; the iOS client times out
# at 30s for ASR.
_UPSTREAM_TIMEOUT = 25.0


class MimoProxyError(Exception):
    """Typed error envelope for the iOS client."""

    def __init__(
        self,
        code: str,
        message: str,
        *,
        status_code: int = 502,
        detail: dict[str, Any] | None = None,
    ) -> None:
        super().__init__(message)
        self.code = code
        self.message = message
        self.status_code = status_code
        self.detail = detail or {}


# ── Key loading ───────────────────────────────────────────────────────────


def _mimo_env_path() -> Path:
    raw = os.getenv("HERALD_MIMO_ENV_PATH")
    if raw:
        return Path(raw).expanduser()
    return Path(os.path.expanduser("~")) / DEFAULT_MIMO_ENV_PATH


def _mimo_base_url() -> str:
    return os.getenv("HERALD_MIMO_BASE_URL", DEFAULT_MIMO_BASE_URL).rstrip("/")


def _load_mimo_api_key() -> str | None:
    """Read the MiMo API key from the on-disk env file.

    Returns ``None`` if the file is missing or contains no key. The
    key value itself is held in memory only; never written to logs.
    """
    env_path = _mimo_env_path()
    if not env_path.exists():
        return None
    try:
        for line in env_path.read_text().splitlines():
            stripped = line.strip()
            if not stripped or stripped.startswith("#"):
                continue
            if "=" not in stripped:
                continue
            key, _, value = stripped.partition("=")
            if key.strip() == "HERALD_MIMO_API_KEY":
                value = value.strip().strip('"').strip("'")
                return value or None
    except OSError as exc:
        logger.warning(
            "mimo: could not read %s (%s)", env_path, exc,
        )
    return None


# ── Request transformation ────────────────────────────────────────────────


def _wav_to_chat_completions(
    *,
    audio_bytes: bytes,
    mime_type: str = "audio/wav",
    language: str = "auto",
    model: str = "mimo-v2.5-asr",
    stream: bool = True,
) -> dict[str, Any]:
    """Translate the iOS multipart upload into Xiaomi's chat-completions body.

    The connector owns this transformation; iOS never has to know the
    upstream contract.
    """
    fmt = "wav" if "wav" in mime_type.lower() else "mp3"
    data_url = (
        f"data:{mime_type};base64,{base64.b64encode(audio_bytes).decode('ascii')}"
    )
    return {
        "model": model,
        "messages": [
            {
                "role": "user",
                "content": [
                    {
                        "type": "input_audio",
                        "input_audio": {
                            "data": data_url,
                            "format": fmt,
                        },
                    }
                ],
            }
        ],
        "asr_options": {"language": language},
        "stream": stream,
    }


# ── Streaming chat-completions → NDJSON ──────────────────────────────────


async def _stream_chat_completions_to_ndjson(
    body: dict[str, Any],
    *,
    api_key: str,
) -> AsyncIterator[dict[str, Any]]:
    """Yield NDJSON dicts parsed from the upstream SSE / line-delimited stream.

    Xiaomi's documented response shape uses OpenAI-compatible SSE
    (``data: {"choices":[{"delta":{"content":"…"}}]}``), with a final
    ``data: [DONE]`` sentinel. The function maps ``delta.content`` →
    ``{"type":"delta","text":…}`` and the final ``choices[0].finish_reason``
    → ``{"type":"final","text":…}`` (with the accumulated text).
    """
    accumulated: list[str] = []
    async with httpx.AsyncClient(timeout=_UPSTREAM_TIMEOUT) as client:
        try:
            async with client.stream(
                "POST",
                f"{_mimo_base_url()}/v1/chat/completions",
                headers={
                    "Content-Type": "application/json",
                    "Authorization": f"Bearer {api_key}",
                    "Accept": "text/event-stream",
                },
                json=body,
            ) as resp:
                if resp.status_code >= 400:
                    raw = await resp.aread()
                    raise MimoProxyError(
                        "mimoUpstreamError",
                        f"MiMo upstream returned HTTP {resp.status_code}.",
                        status_code=resp.status_code,
                        detail={"upstreamStatus": resp.status_code},
                    )
                async for raw_line in resp.aiter_lines():
                    if not raw_line:
                        continue
                    line = raw_line
                    if line.startswith("data: "):
                        line = line[len("data: "):]
                    elif line.startswith("data:"):
                        line = line[len("data:"):]
                    line = line.strip()
                    if not line or line == "[DONE]":
                        if line == "[DONE]":
                            yield {
                                "type": "final",
                                "text": "".join(accumulated),
                            }
                        continue
                    try:
                        evt = json.loads(line)
                    except ValueError:
                        continue
                    delta = (
                        (evt.get("choices") or [{}])[0]
                        .get("delta") or {}
                    )
                    chunk = delta.get("content")
                    if chunk:
                        accumulated.append(chunk)
                        yield {"type": "delta", "text": chunk}
                    finish = (
                        (evt.get("choices") or [{}])[0]
                        .get("finish_reason")
                    )
                    if finish and finish != "null":
                        yield {
                            "type": "final",
                            "text": "".join(accumulated),
                        }
                        return
        except httpx.TimeoutException as exc:
            raise MimoProxyError(
                "mimoUpstreamUnreachable",
                f"MiMo upstream timed out: {exc!r}",
                status_code=504,
            ) from exc
        except httpx.RequestError as exc:
            raise MimoProxyError(
                "mimoUpstreamUnreachable",
                f"MiMo upstream connection failed: {exc!r}",
                status_code=502,
            ) from exc


# ── Public API used by the HTTP facade ───────────────────────────────────


async def transcribe_audio(
    *,
    audio_bytes: bytes,
    mime_type: str = "audio/wav",
    language: str = "auto",
    model: str = "mimo-v2.5-asr",
) -> AsyncIterator[dict[str, Any]]:
    """Stream NDJSON ASR events for the supplied audio.

    The iOS client is the canonical consumer.  Tests may
    iterate the result directly.
    """
    api_key = _load_mimo_api_key()
    if not api_key:
        raise MimoProxyError(
            "mimoNotConfigured",
            "MiMo API key is not configured on the connector host.",
            status_code=503,
        )
    if len(audio_bytes) < _MIN_WAV_BYTES:
        raise MimoProxyError(
            "mimoAudioTooShort",
            (
                "Audio payload is too short for transcription; "
                f"got {len(audio_bytes)} bytes, expected at least "
                f"{_MIN_WAV_BYTES}."
            ),
            status_code=400,
        )
    body = _wav_to_chat_completions(
        audio_bytes=audio_bytes,
        mime_type=mime_type,
        language=language,
        model=model,
        stream=True,
    )
    started = time.monotonic()
    final_seen = False
    async for event in _stream_chat_completions_to_ndjson(
        body, api_key=api_key
    ):
        if event.get("type") == "final":
            final_seen = True
        yield event
    if not final_seen:
        # Documented failure: the iOS client cannot synthesize a
        # final transcript from partial deltas alone.
        raise MimoProxyError(
            "mimoNoFinalTranscript",
            "MiMo stream ended without a final transcript.",
            status_code=502,
        )
    logger.info(
        "mimo: asr completed in %.2fs (%d bytes)",
        time.monotonic() - started,
        len(audio_bytes),
    )


async def probe_upstream_reachable() -> tuple[bool, str]:
    """Return ``(ok, reason)`` for the readiness probe.

    Called by ``/v1/talk/readiness``.  Cheap: one HEAD request with
    a 5 s timeout.  Never raises.
    """
    api_key = _load_mimo_api_key()
    if not api_key:
        return False, "MiMo API key not configured on the connector host."
    try:
        async with httpx.AsyncClient(timeout=5.0) as client:
            resp = await client.request(
                "GET",
                f"{_mimo_base_url()}/v1/models",
                headers={"Authorization": f"Bearer {api_key}"},
            )
            # Readiness is a product guarantee, not merely a TCP probe. A
            # rejected credential must block Talk before we light the mic.
            if resp.status_code in (200, 204):
                return True, "ok"
            if resp.status_code in (401, 403):
                return False, "MiMo API key was rejected by the upstream service."
            if resp.status_code == 404:
                return False, "MiMo upstream does not expose the required models endpoint."
            return False, f"MiMo upstream returned HTTP {resp.status_code}."
    except Exception as exc:  # noqa: BLE001
        return False, f"MiMo upstream unreachable: {exc!r}"


def proxy_error_payload(exc: MimoProxyError) -> dict[str, Any]:
    """Stable JSON shape for typed MimoProxyError returns."""
    return {
        "$schema": "mimo-error-v1",
        "error": exc.code,
        "message": exc.message,
        "detail": exc.detail,
    }
