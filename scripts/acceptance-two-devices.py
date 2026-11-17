"""Acceptance test: two devices resume from their own marks, independently.

Two devices, one turn each. A turn belongs to the device that created it - a second
device asking for it is answered job_not_found, which is the ownership rule working
rather than a gap - so independence is proved the way it actually happens: each
device streams its own turn, both drop, and both resume from their own mark without
touching the other.

Prints each phase so the evidence is the output.
"""
import json
import os
import subprocess
import sys
import time
import urllib.request

BASE = "http://127.0.0.1:7341"
PHONE = "01a1e23f-e18d-7049-ac5c-967613948204"
IPAD = "01a1e240-aad6-7775-9cff-a2541bd7d72e"


def token(device):
    return subprocess.run(
        [os.path.expanduser("~/.local/opt/kallisti/bin/eris"), "device", "token",
         "--id", device, "--print"],
        capture_output=True, text=True, check=True,
    ).stdout.strip()


def post(path, payload, tok):
    request = urllib.request.Request(
        BASE + path,
        data=json.dumps(payload).encode(),
        headers={"Authorization": "Bearer " + tok, "Content-Type": "application/json"},
        method="POST",
    )
    with urllib.request.urlopen(request, timeout=30) as response:
        return json.loads(response.read())


def read(job, tok, last_id, seconds):
    headers = {"Authorization": "Bearer " + tok, "Accept": "text/event-stream"}
    if last_id is not None:
        headers["Last-Event-ID"] = str(last_id)
    request = urllib.request.Request(BASE + "/v1/turns/" + job + "/events", headers=headers)
    ids, current = [], last_id
    started = time.time()
    try:
        with urllib.request.urlopen(request, timeout=5) as response:
            while time.time() - started < seconds:
                line = response.readline()
                if not line:
                    break
                text = line.decode().strip()
                if text.startswith("id:"):
                    current = int(text.split(":", 1)[1].strip())
                    ids.append(current)
                elif "done" in text and text.startswith("event:"):
                    break
    except Exception as error:
        print("  stream ended: %s" % type(error).__name__)
    return ids, current


phone_tok = token(PHONE)
ipad_tok = token(IPAD)

phone_turn = post("/v1/turns", {"text": "Write a 200 word description of a tide pool."}, phone_tok)
ipad_turn = post("/v1/turns", {"text": "Write a 200 word description of a pine forest."}, ipad_tok)
print("phone job %s" % phone_turn["job_id"])
print("ipad job %s" % ipad_turn["job_id"])

phone_ids, phone_mark = read(phone_turn["job_id"], phone_tok, None, 6)
ipad_ids, ipad_mark = read(ipad_turn["job_id"], ipad_tok, None, 6)
print("  phone first read: %d frames, mark %s" % (len(phone_ids), phone_mark))
print("  ipad first read: %d frames, mark %s" % (len(ipad_ids), ipad_mark))

phone_again, phone_mark2 = read(phone_turn["job_id"], phone_tok, phone_mark, 60)
ipad_again, ipad_mark2 = read(ipad_turn["job_id"], ipad_tok, ipad_mark, 60)
print("  phone resumed: %d frames, first %s" % (len(phone_again), phone_again[0] if phone_again else None))
print("  ipad resumed: %d frames, first %s" % (len(ipad_again), ipad_again[0] if ipad_again else None))

phone_ok = (
    phone_mark is not None and bool(phone_again)
    and phone_again[0] == phone_mark + 1
    and not set(phone_ids) & set(phone_again)
)
ipad_ok = (
    ipad_mark is not None and bool(ipad_again)
    and ipad_again[0] == ipad_mark + 1
    and not set(ipad_ids) & set(ipad_again)
)
print("  phone resumed from its own mark with no repeats: %s" % phone_ok)
print("  ipad resumed from its own mark with no repeats: %s" % ipad_ok)
ok = phone_ok and ipad_ok
print("RESULT: %s" % ("PASS" if ok else "FAIL"))
sys.exit(0 if ok else 1)
