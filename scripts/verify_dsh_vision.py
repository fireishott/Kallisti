#!/usr/bin/env python3
"""Verification gate for the direct-DSH Kallisti path and DSH image routing.

The failure this exists to prevent: claiming "vision works" from a model catalog
or a changed config file. Every check here completes a real turn and asserts on
the *reply text*, so a green run means a prompt carrying an image reached a
vision-capable model and came back with the right answer.

Checks, in order:
  1. health      - the DSH phone API answers
  2. text        - a text turn completes and echoes a token
  3. vision      - an image turn on a known vision model reads the image
  4. auto-vision - an image turn started on a TEXT-ONLY model still completes:
                   the dsh-auto-vision plugin switches models instead of
                   raising MODEL_DOES_NOT_SUPPORT_IMAGES

Usage:
    python3 scripts/verify_dsh_vision.py [--image PATH] [--token TOKEN] [--base URL]

Defaults read the build's DSH settings out of Config/DSH.local.xcconfig, so the
gate uses the same host and credentials the app ships with.
"""

from __future__ import annotations

import argparse
import base64
import json
import re
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent

# A model with no image input at all - the case that used to hard-error.
TEXT_ONLY_MODEL = ("ninerouter", "ds/deepseek-chat")
# A model verified to read images on this deployment.
VISION_MODEL = ("ninerouter", "cmc/moonshotai/Kimi-K2.7-Code")

TURN_TIMEOUT_SECONDS = 120
POLL_SECONDS = 2

# Text visible in the bundled fixture screenshot, used to prove the model
# actually looked at the pixels instead of answering from the prompt.
IMAGE_FIXTURE_TEXT = "good homie"


def read_xcconfig() -> dict[str, str]:
    path = REPO / "Config" / "DSH.local.xcconfig"
    values: dict[str, str] = {}
    if not path.exists():
        return values
    for line in path.read_text().splitlines():
        match = re.match(r"\s*(KALLISTI_DSH_(?:TOKEN|BASE_URL))\s*=\s*(.+?)\s*$", line)
        if match:
            values[match.group(1)] = match.group(2).replace("$()", "")
    return values


class PhoneApi:
    def __init__(self, base: str, token: str) -> None:
        self.base = base.rstrip("/")
        self.headers = {
            "Authorization": f"Bearer {token}",
            "Content-Type": "application/json",
        }

    def call(self, path: str, body: dict | None = None) -> dict:
        request = urllib.request.Request(
            f"{self.base}{path}",
            data=None if body is None else json.dumps(body).encode(),
            headers=self.headers,
            method="GET" if body is None else "POST",
        )
        with urllib.request.urlopen(request, timeout=90) as response:
            return json.load(response)

    def new_session(self) -> str:
        return self.call("/session", {"agentPreset": "ignyte"})["sessionId"]

    def select(self, session_id: str, provider: str, model: str) -> None:
        self.call("/model", {"sessionId": session_id, "provider": provider, "model": model})

    def prompt(self, session_id: str, text: str, images: list[dict] | None = None) -> dict:
        body: dict = {"sessionId": session_id, "text": text, "mode": "queue"}
        if images:
            body["images"] = images
        return self.call("/prompt", body)

    def page(self, session_id: str) -> dict:
        return self.call(f"/page?sessionId={session_id}")

    def await_turn(self, session_id: str) -> tuple[list[str], str | None, str | None]:
        """Poll until the turn commits. Returns (replies, model_used, error)."""
        deadline = time.time() + TURN_TIMEOUT_SECONDS
        while time.time() < deadline:
            time.sleep(POLL_SECONDS)
            replies: list[str] = []
            model_used: str | None = None
            for record in self.page(session_id).get("records", []):
                event = record.get("event", {})
                kind = event.get("type")
                if kind == "assistant/message":
                    message = event["data"]["message"]
                    replies.append(
                        "".join(
                            block.get("text", "")
                            for block in message.get("content", [])
                            if block.get("type") == "text"
                        )
                    )
                    model_used = message.get("source", {}).get("model") or model_used
                elif kind == "turn/end":
                    reason = event["data"].get("reason", {})
                    if reason.get("kind") == "error":
                        return replies, model_used, reason.get("error", {}).get("message", "turn failed")
            if replies:
                return replies, model_used, None
        return [], None, "timed out waiting for the turn to finish"


def encode_image(path: Path, mime: str = "image/jpeg") -> list[dict]:
    return [{
        "mimeType": mime,
        "data": base64.b64encode(path.read_bytes()).decode(),
    }]


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--image", default=str(Path.home() / ".dsh" / "attachments" / "v1" / "objects" / "2d"
                        / "2d682a1d06ebb9c8202ed0ca68ff0c9882c43e3b35ebdaa98eb87a2f118c7441"))
    parser.add_argument("--token", default=None)
    parser.add_argument("--base", default=None)
    args = parser.parse_args()

    config = read_xcconfig()
    token = args.token or config.get("KALLISTI_DSH_TOKEN", "")
    base = args.base or config.get("KALLISTI_DSH_BASE_URL", "")
    if not token or not base:
        print("FAIL: no DSH token/base URL (pass --token/--base or fill Config/DSH.local.xcconfig)")
        return 2
    api = PhoneApi(base.rstrip("/") + "/phone/v1", token)

    image_path = Path(args.image)
    if not image_path.exists():
        print(f"FAIL: fixture image not found at {image_path}")
        return 2
    images = encode_image(image_path)

    failures: list[str] = []

    print("health")
    try:
        health = api.call("/health")
        print(f"  phone api            OK ({health.get('service')} {health.get('version')})")
    except Exception as error:  # noqa: BLE001
        print(f"  phone api            FAIL - {error}")
        return 1

    print("text")
    session = api.new_session()
    api.prompt(session, "Reply with exactly DSH_TEXT_OK and nothing else.")
    replies, _, error = api.await_turn(session)
    if error or not any("DSH_TEXT_OK" in reply for reply in replies):
        print(f"  text round trip      FAIL - {error or replies}")
        failures.append("text")
    else:
        print("  text round trip      OK")

    print("vision")
    session = api.new_session()
    api.select(session, *VISION_MODEL)
    api.prompt(session, "Reply with exactly DSH_VISION_OK followed by the text of the first "
                        "right-aligned chat bubble.", images=images)
    replies, model_used, error = api.await_turn(session)
    if error or not any("DSH_VISION_OK" in r and IMAGE_FIXTURE_TEXT in r for r in replies):
        print(f"  image on vision model FAIL - {error or replies}")
        failures.append("vision")
    else:
        print(f"  image on vision model OK (read the image via {model_used})")

    print("auto-vision")
    session = api.new_session()
    api.select(session, *TEXT_ONLY_MODEL)
    try:
        api.prompt(session, "Reply with exactly DSH_AUTOVISION_OK followed by the text of the "
                            "first right-aligned chat bubble.", images=images)
    except urllib.error.HTTPError as http_error:
        detail = http_error.read().decode(errors="replace")
        print(f"  text-only + image    FAIL - HTTP {http_error.code} {detail}")
        print("                       (dsh-auto-vision did not recover the admission)")
        failures.append("auto-vision")
    else:
        replies, model_used, error = api.await_turn(session)
        if error or not any("DSH_AUTOVISION_OK" in r and IMAGE_FIXTURE_TEXT in r for r in replies):
            print(f"  text-only + image    FAIL - {error or replies}")
            failures.append("auto-vision")
        elif model_used == TEXT_ONLY_MODEL[1]:
            print(f"  text-only + image    FAIL - stayed on {model_used}, which cannot read images")
            failures.append("auto-vision")
        else:
            print(f"  text-only + image    OK (auto-switched to {model_used})")

    if failures:
        print(f"\nFAILED: {', '.join(failures)}")
        return 1
    print("\nOK: direct DSH text and image turns both complete; images auto-route to a vision model.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
