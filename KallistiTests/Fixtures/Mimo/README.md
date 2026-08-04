# MiMo API Fixtures

## Auth Header
- MiMo API uses `api-key` header (NOT `Authorization: Bearer`)
- Herald currently sends `Authorization: Bearer` — this is a bug (T5)
- Fix: change to `api-key: <key>` header

## ASR (mimo-v2.5-asr)
- Endpoint: POST /v1/audio/transcriptions
- Input: WAV audio, base64 encoded
- Max size: 10 MB
- Languages: auto, zh, en
- Response: streamed deltas + final transcript

## TTS
- Endpoint: POST /v1/audio/speech
- Input: text
- Output: PCM16, 24kHz, 16-bit LE, mono
- Voices: Mia, Chloe, Milo, Dean
- Supports streaming
