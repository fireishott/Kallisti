"""Hermes-native speech worker for Talk (Kallisti 0.1.0).

Runs under the HERMES venv python (not the connector's), with the
hermes-agent repo root on sys.path (HERMES_AGENT_ROOT env) and
HERMES_HOME inherited from the connector unit, so STT/TTS provider
resolution is exactly what `hermes` itself would do.

Protocol: one JSON object per line on stdin -> one per line on stdout.
  {"id": 1, "op": "status"}
  {"id": 2, "op": "transcribe", "path": "/tmp/u.wav"}
  {"id": 3, "op": "speak", "text": "Hello.", "output_dir": "/tmp"}
Responses always echo "id" and carry "success"; errors carry "error".
Never writes anything except protocol lines to stdout (logs go to stderr).
"""
from __future__ import annotations

import json
import os
import subprocess
import sys
import tempfile
import traceback

sys.path.insert(0, os.environ.get("HERMES_AGENT_ROOT", os.getcwd()))


def _status() -> dict:
    from tools.transcription_tools import _load_stt_config, _get_provider  # type: ignore
    from tools.tts_tool import _load_tts_config  # type: ignore
    stt = _load_stt_config() or {}
    tts = _load_tts_config() or {}
    return {
        "success": True,
        "stt_provider": _get_provider(stt),
        "tts_provider": str(tts.get("provider") or ""),
    }


def _transcribe(path: str) -> dict:
    from tools.transcription_tools import transcribe_audio  # type: ignore
    result = transcribe_audio(path)
    return {
        "success": bool(result.get("success")),
        "transcript": result.get("transcript", ""),
        "error": result.get("error") or None,
    }


def _speak(text: str, output_dir: str) -> dict:
    from tools.tts_tool import text_to_speech_tool  # type: ignore
    raw_path = os.path.join(output_dir, "tts-raw")
    result = json.loads(text_to_speech_tool(text, output_path=raw_path))
    if not result.get("success"):
        return {"success": False, "error": result.get("error") or "TTS failed."}
    produced = result.get("file_path") or raw_path
    wav_fd, wav_path = tempfile.mkstemp(suffix=".wav", dir=output_dir)
    os.close(wav_fd)
    # Normalize whatever container the provider produced (edge -> mp3) to
    # the single format the iOS PCM queue consumes: 24 kHz mono s16 WAV.
    proc = subprocess.run(
        ["ffmpeg", "-y", "-loglevel", "error", "-i", produced,
         "-ar", "24000", "-ac", "1", "-sample_fmt", "s16", wav_path],
        capture_output=True, text=True, timeout=30,
    )
    if proc.returncode != 0:
        return {"success": False, "error": f"ffmpeg failed: {proc.stderr[-300:]}"}
    if os.path.exists(produced):
        os.unlink(produced)
    return {"success": True, "wav_path": wav_path}


def main() -> None:
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            req = json.loads(line)
            op = req.get("op")
            if op == "status":
                resp = _status()
            elif op == "transcribe":
                resp = _transcribe(str(req["path"]))
            elif op == "speak":
                resp = _speak(str(req["text"]), str(req.get("output_dir") or tempfile.gettempdir()))
            else:
                resp = {"success": False, "error": f"unknown op {op!r}"}
        except Exception as exc:  # noqa: BLE001
            traceback.print_exc(file=sys.stderr)
            resp = {"success": False, "error": f"worker exception: {exc!r}"}
        resp["id"] = req.get("id") if isinstance(req, dict) else None
        sys.stdout.write(json.dumps(resp) + "\n")
        sys.stdout.flush()


if __name__ == "__main__":
    main()
