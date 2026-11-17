"""Acceptance test: resume a live turn from the last ack.

Creates a turn, reads a few SSE frames, drops the connection, reconnects with
Last-Event-ID, and reports whether the resumed stream continued without
duplicating a frame the first connection already delivered.

Prints one line per phase and a final PASS/FAIL, so the evidence is the output.
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


def post(path, payload):
    request = urllib.request.Request(
        BASE + path,
        data=json.dumps(payload).encode(),
        headers={"Authorization": "Bearer " + TOKEN, "Content-Type": "application/json"},
        method="POST",
    )
    with urllib.request.urlopen(request, timeout=30) as response:
        return json.loads(response.read())


def read_events(job_id, last_id, seconds):
    """Read SSE frames for a while, returning (ids, kinds, last_id)."""
    headers = {"Authorization": "Bearer " + TOKEN, "Accept": "text/event-stream"}
    if last_id is not None:
        headers["Last-Event-ID"] = str(last_id)
    request = urllib.request.Request(BASE + "/v1/turns/" + job_id + "/events", headers=headers)
    ids, kinds, current_id = [], [], last_id
    started = time.time()
    try:
        with urllib.request.urlopen(request, timeout=5) as response:
            while time.time() - started < seconds:
                line = response.readline()
                if not line:
                    break
                text = line.decode().strip()
                if text.startswith("id:"):
                    current_id = int(text.split(":", 1)[1].strip())
                    ids.append(current_id)
                elif text.startswith("event:"):
                    kinds.append(text.split(":", 1)[1].strip())
                elif text.startswith("data:") and "done" in kinds[-1:]:
                    pass
    except Exception as error:  # a dropped socket is the point of the test
        print("  connection ended: %s" % type(error).__name__)
    return ids, kinds, current_id


turn = post("/v1/turns", {"text": "Write a detailed 300 word explanation of how a diesel engine works, in paragraphs."})
job = turn["job_id"]
print("job %s" % job)

first_ids, first_kinds, last = read_events(job, None, 3)
print("  first connection: %d frames, last id %s, kinds %s"
      % (len(first_ids), last, sorted(set(first_kinds))))

time.sleep(1)

second_ids, second_kinds, last2 = read_events(job, last, 90)
print("  resumed: %d frames, last id %s" % (len(second_ids), last2))

overlap = sorted(set(first_ids) & set(second_ids))
gaps = []
for earlier, later in zip(sorted(first_ids + second_ids), sorted(first_ids + second_ids)[1:]):
    if later != earlier + 1 and later not in first_ids and earlier not in second_ids:
        gaps.append((earlier, later))

print("  overlap (must be empty): %s" % overlap)
print("  terminal frame on resume: %s" % ("done" in second_kinds or "error" in second_kinds))
ok = not overlap and "done" in second_kinds
print("RESULT: %s" % ("PASS" if ok else "FAIL"))
sys.exit(0 if ok else 1)
