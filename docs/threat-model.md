# Threat model: one operator, one host, one phone

Written for WP-22. It states what this daemon trusts, what it refuses, and what a
compromise costs. Where a control does not exist, it says so in the same voice as
the ones that do.

## The shape

One person runs this host for themselves. There is no second tenant, no shared
room that reaches someone else's installation, and no multi-user authorisation
model to get wrong. That is a deliberate constraint rather than an unfinished
feature: every control below assumes exactly one trusted human.

```
phone  --https/SSE-->  daemon (fih-ai-host)  --tools-->  this machine
```

No relay sits in the path. Nothing outside the tailnet or the LAN can reach the
device listener.

## Assets, in the order a compromise would hurt

1. **The bearer tokens.** A live token is every capability the phone has.
2. **The pairing code.** Five minutes of power to mint a token.
3. **The transcript.** Everything ever said, including health and location.
4. **The sensor store.** Where the operator is, and how they slept.
5. **The shell.** `run_command` in a workspace.
6. **The outbox.** What was composed for sending, and to whom.

## Controls that exist

| Threat | Control | Where |
|---|---|---|
| A token in the clear | Hashed verifier only; never stored or logged in plaintext | `auth/store.rs` |
| A token replayed from another machine | `auth_tokens.minted_for` = this host's machine id; a row bound elsewhere is refused `token_from_another_daemon` | migration 0037, `auth/token.rs` |
| A stolen refresh token reused forever | Rotation retires the presented row before minting; the old pair cannot be replayed | `TokenService::rotate` |
| Decided-token rows accumulating as hashes | `prune_dead_tokens` on boot and after each rotation | `auth/token.rs` |
| A pairing code guessed or replayed | 5-minute TTL, single-use consume inside the same statement that reads it | `auth/pairing.rs` |
| A phone without administration doing host work | `require_admin` on operator routes; chat scope cannot read diagnostics or the model catalogue | `device/handlers.rs` |
| A screen-locked phone holding an open call | The turn is a job with a lease and a heartbeat; the phone resumes from its last `seq` | WP-02, WP-04 |
| A model mailing anyone it likes | Comms allowlist, confirm step, recipient lock; no transport exists at all | `comms.rs` |
| A model leaking a reading the operator declined | Values stripped before the write, not at read time | `sensitivity.rs` |

## What a compromise costs

- **Phone stolen, unlocked.** The thief has a live access token. Rotation on the
  next refresh does not help them or you: both sides keep working until the token
  expires. The real control is `device revoke`, which is a manual action and
  therefore a human timeline, not a technical one.
- **Phone stolen, locked.** The token is in the keychain behind the device
  passcode. This is the case the threat model actually relies on.
- **Host compromised.** Everything is over. The database is unencrypted at rest,
  and the operator's threat model has always been "the host is mine".
- **Tailnet compromised.** Reachability, not authentication, is what the tailnet
  provides; the token is still required. A tailnet-only listener is convenience,
  not a control.

## Controls that do not exist

- **No per-request attestation.** A token is enough; there is no device binding on
  the request itself.
- **No rate limit on pairing attempts.** A code is 5 minutes and single-use, but
  nothing stops repeated tries inside that window. The entropy is high enough that
  this is a note rather than a hole, and it is written down rather than assumed.
- **No encryption at rest.** SQLite files are plaintext on disk.
- **No remote revocation by the operator from the phone.** Revoking means running
  `eris device revoke` on the host.
- **No audit of reads.** Writes are audited; a token reading the transcript leaves
  no row behind.

## Tunnel guidance

Two ways in, both authenticated by token first:

1. **Tailscale (preferred).** The device listener is reachable only on the tailnet.
   Nothing is published to the internet, no port forward, no certificate to renew.
   The tailnet name resolves from anywhere the operator is signed in.
2. **LAN.** `http://<host>:7341` works on the local network. It is plaintext HTTP,
   so a token crosses the wire in the clear on that segment. Acceptable at home;
   not acceptable on a network the operator does not control.

**Never**: a public DNS name pointed at the device listener with a port forward.
The daemon has no rate limiting and no WAF, and its authentication is a bearer
token - that is a bad shape to put on the open internet.

## Rotation

| Secret | Rotates | How |
|---|---|---|
| Access token | On refresh | `TokenService::rotate`, old row retired first |
| Refresh token | Same | Same call |
| Pairing code | 5 minutes, single use | TTL checked on redeem |
| Machine identity | Never | `/etc/machine-id`, the anchor the contrast test uses |

A device is revoked with `eris device revoke`, which retires every row for it. The
revocation is immediate for new requests and leaves the current stream to end.
