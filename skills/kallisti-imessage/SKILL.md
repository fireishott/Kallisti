---
name: kallisti-imessage
description: Send and read iMessages and SMS through a BlueBubbles bridge — stage and confirm a text, list recent threads, and pull incoming messages.
version: 1.0.0
author: Kallisti
license: MIT
platforms: [linux, macos]
metadata:
  kallisti:
    tags: [imessage, sms, texting, bluebubbles, comms]
    related_skills: [kallisti-ios, email-inbox-triage]
---

# iMessage and SMS

The daemon texts through a BlueBubbles server: a Mac signed into Messages,
reachable at an HTTPS URL, holding the password that is the API key. Nothing goes
out in the operator's name without their yes, so every send is two steps.

## The two tools

**`comms_stage`** with `kind` `sms` or `imessage`, the recipient, and the exact
body. Stage first, then show the words and the number. Nothing has left yet.

**`comms_confirm`** with the id from the stage and the same recipient, byte for
byte. That is the step that actually sends. A different recipient is refused: you
re-stage instead of swapping.

The reply says `delivered: true` with a message guid, or `delivered: false` with
either a reason (no transport configured) or the server's own failure words. Never
tell the operator a text went out unless the tool said so.

**`comms_status`** reports whether the transport is configured and what the last
five messages did. Read it before promising a send, especially on a host you have
not used for texting.

## Recipients are allowlisted

Only addresses and numbers in `comms.allowlist` can be staged. A refusal names
the allowlist and writes nothing. The operator adds a recipient to that setting;
you cannot widen it yourself.

## Format

- Keep a text short. It lands in Messages, not a mail client.
- One thought per message. If it runs past two sentences, it probably wants to be
  a chat message instead.
- `chatGuid` is built for you: `any;-;<recipient>` covers a number or an Apple ID.
  It never needs to be passed.

## Reading a thread

Two shapes, both read-only:

```bash
# recent messages across every chat
curl -sS "$SERVER/api/v1/message?password=$PW&limit=10&sort=DESC"

# one thread, by its chatGuid (from /api/v1/chat/query)
curl -sS "$SERVER/api/v1/chat/any;-;+15025550100/message?password=$PW&limit=25&sort=DESC"
```

Find a chat's guid first, because a phone number alone will not match a group
thread:

```bash
curl -sS -X POST "$SERVER/api/v1/chat/query?password=$PW" \
  -H 'Content-Type: application/json' -d '{"limit":10,"with":["lastMessage"]}'
```

`isFromMe` separates what the operator sent from what came in. `dateCreated` is
milliseconds since the epoch. An attachment is a path on the Mac, fetched with
`POST /api/v1/message/attachment` only when asked.

## A file instead of words

```bash
curl -sS -X POST "$SERVER/api/v1/message/attachment?password=$PW" \
  -F "chatGuid=any;-;+15025550100" -F "tempGuid=temp-$(uuidgen)" \
  -F "message=here it is" -F "attachment=@/path/on/the/mac.png"
```

The path is the Mac's, not this host's. For a file produced here, copy it over
first or hand it to the operator.

## Failures, and what they mean

| reply | cause |
|---|---|
| 401 `Invalid password` | the stored password is wrong or rotated |
| curl timeout after 20s | the Mac is asleep or unreachable |
| 400 on send | `chatGuid` malformed, or the recipient cannot take a message |
| 404 on the URL | the Cloudflare URL rotated on a server restart |

Every failure is recorded as `send_failed` with the server's words. Report the
failure as a failure, and say which of the four it was.
