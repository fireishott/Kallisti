"""Prove the call sequence the app now makes: bind, send, reload.

The phone's bug was that a turn could go out with no conversation, so the reply
existed on the stream and nowhere else. The app now creates a conversation first,
sends with it, and reloads by id. This replays that exact sequence against the live
daemon and asserts the transcript comes back with both rows, in order, once.

Prints each step so the evidence is the output.
"""
import json
import os
import subprocess
import sys
import time
import urllib.request

BASE = "http://127.0.0.1:7341"
DEVICE = "01a1e23f-e18d-7049-ac5c-967613948204"
TOKEN = subprocess.run(
    [os.path.expanduser("~/.local/opt/kallisti/bin/eris"), "device", "token",
     "--id", DEVICE, "--print"],
    capture_output=True, text=True, check=True,
).stdout.strip()


def call(method, path, payload=None, headers=None):
    body = json.dumps(payload).encode() if payload is not None else None
    merged = {"Authorization": "Bearer " + TOKEN, "Content-Type": "application/json"}
    merged.update(headers or {})
    request = urllib.request.Request(BASE + path, data=body, headers=merged, method=method)
    with urllib.request.urlopen(request, timeout=60) as response:
        return json.loads(response.read())


created = call("POST", "/v1/conversations", {"title": "mirror-check"})
conversation = created["id"]
print("1. created conversation %s" % conversation)

turn = call("POST", "/v1/turns", {
    "text": "Reply with exactly: mirror-ok",
    "conversation": conversation,
})
job = turn["job_id"]
print("2. turn %s sent with that conversation" % job)

for _ in range(30):
    answer = call("POST", "/v1/rpc", {"method": "turn.state", "params": {"job_id": job}})
    state = answer.get("result", answer)
    if state.get("state") in ("succeeded", "failed", "cancelled", "expired"):
        break
    time.sleep(1)
print("3. turn ended %s" % state.get("state"))

detail = call("GET", "/v1/conversations/%s" % conversation)
messages = detail.get("messages", [])
print("4. reload returned %d messages" % len(messages))
for message in messages:
    body = (message.get("body") or message.get("content") or "")[:48].replace("\n", " ")
    print("   %-10s %s" % (message.get("role"), body))

roles = [m.get("role") for m in messages]
ok = (
    len(messages) == 2
    and roles == ["user", "assistant"]
    and "mirror-ok" in json.dumps(messages)
)
print("RESULT: %s" % ("PASS" if ok else "FAIL"))

# Leave nothing behind.
call("POST", "/v1/conversations/%s/clear" % conversation)
print("5. cleared the check conversation")
sys.exit(0 if ok else 1)
