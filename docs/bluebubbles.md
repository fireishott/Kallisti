# Texting from the daemon: BlueBubbles

Kallisti can send a real SMS or iMessage instead of only writing a row that says
"agreed, not delivered". The transport is [BlueBubbles](https://bluebubbles.app), a
self-hosted bridge that runs on a Mac signed into Messages. Nothing here needs a
vendor account and nothing routes through a third party other than the Mac itself.

## What it is

BlueBubbles runs on a Mac that is signed into iMessage. It exposes that Mac's
Messages over a local REST API, proxied to an HTTPS URL you choose (Cloudflare,
zrok or your own DNS). The daemon calls it with `curl`:

```
POST <server>/api/v1/message/text?password=<server password>
{"chatGuid":"any;-;<recipient>","tempGuid":"temp-<uuid>","message":"..."}
```

A `status` of 200 means the Mac queued it. Anything else is recorded, with the
server's own words, on the message row.

## Requirements

- A Mac that stays awake and stays signed into Messages. `caffeinate -s &` and a
  login item are the two settings that matter.
- BlueBubbles installed on that Mac, with Full Disk Access granted.
- The server's public HTTPS URL and its password.

## Configure the daemon

Two settings, both strings, in the daemon's `settings` table:

| key | value |
|---|---|
| `comms.bluebubbles.url` | server base URL, e.g. `https://bb.example.ts.net` |
| `comms.bluebubbles.password` | the BlueBubbles server password |

```bash
sqlite3 ~/.local/share/kallisti/eris.db \
  "INSERT INTO settings (key, value) VALUES ('comms.bluebubbles.url', 'https://bb.example.ts.net')
   ON CONFLICT(key) DO UPDATE SET value = excluded.value;"
```

Set the password the same way. Check the pair with the `comms_status` tool: it
reports the transport, the allowlist and the last five messages.

## The rules do not change

Confirmation is still the gate. `comms_stage` writes the row, `comms_confirm`
locks the recipient and only then hands the text to the transport. An address
that is not on `comms.allowlist` is refused before anything is written, and a
confirmation that names a different recipient is refused outright.

`email` and `call` still stop at the row. Only `sms` and `imessage` leave the host.

## Received messages

BlueBubbles can POST a webhook for `new-message`. Point one at
`<gateway>/webhook/bluebubbles` when the daemon surface is reachable from the Mac,
or poll `GET /api/v1/message?limit=10&sort=DESC` from a script. The skill
`kallisti-imessage` carries both shapes.

## Failure modes worth knowing

- **Wrong password.** The reply is a 401 with `error.error` "Invalid password". The
  message row records it as `send_failed`, not sent.
- **Cloudflare URL rotates on restart.** Re-read the URL from the BlueBubbles UI
  after every restart, or use zrok or DDNS so it stops moving.
- **The Mac sleeps.** Sends time out after 20 seconds and fail loudly.
- **A phone number with no iMessage.** `chatGuid` still says `any;-;`, and the Mac
  falls back to green-bubble SMS if the line can do it at all.
