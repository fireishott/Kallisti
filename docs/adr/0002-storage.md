# ADR 0002 - Storage: rusqlite bundled, WAL segments, plain WAL file

- Status: **proposed** - nothing built yet, no database created
- Date: 2026-10-19
- Wave: 0 (decision recorded), implemented in Waves 1 and 3
- Related: locked amendments 3, 7

## Context

Two different things in Kallisti are both called "WAL", and confusing them is how
a design goes wrong:

1. **SQLite's WAL** is the journal mode for the relational database that holds
   app state, devices, pairing codes, jobs, settings, and the audit log.
2. **The session event log's WAL** is an application-level append-only file of
   JSON envelopes, one per line, that is fsynced before an ack.

They have different durability rules, different failure modes, and different
retention. They are written down here separately so no later change collapses
them.

Locked amendment 7: do not append one growing `.jsonl.zst`. Sealed immutable
segments plus a short uncompressed WAL. Recovery never rewrites a committed
segment.

## Decision

### Relational store

- `rusqlite` 0.40.2 with the `bundled` feature. The amalgamation is compiled into
  the binary, so the host's `sqlite3` (3.46.1 today) is irrelevant and there is no
  system library version drift between the dev host and a user's machine.
- Database at `~/.local/share/kallisti/eris.db`, `journal_mode=WAL`,
  `synchronous=FULL`, `busy_timeout` set. One writer process, so the writer
  contention that WAL is good at is not the point; crash safety is.
- Embedded schema version table, migrations applied on start. Migration 0001
  creates `app_state`, `admin_credential`, `sessions`, `devices`,
  `pairing_codes`, `jobs`, `job_events`, `settings`, `audit_log`.
- No migration ever runs against a database it does not recognise. Unknown
  version means refuse to start.

### Session event log

- Live log: `sessions/<session_id>/wal.jsonl`, plain UTF-8, one envelope per
  line, uncompressed, never compressed. Fsync before ack.
- Seal: validate a WAL range, write `session.v1.jsonl.zst`, fsync the segment,
  and only then truncate the WAL. A crash between those two steps must replay
  clean.
- Recovery: a torn final WAL line (no trailing newline or unparseable JSON) is
  truncated back to the last valid envelope and recorded as a `recovery.truncated`
  event. A zstd decode error in a **sealed** segment is an error, never a trim.
- Lock: `fs4` advisory lock on `sessions/<id>/.lock`. The second writer gets
  `AlreadyLocked` rather than interleaving envelopes.
- A committed segment is never rewritten, ever.

## Consequences

- The `bundled` feature needs a C compiler at build time. `gcc` is present on
  this host; CI must have one too.
- `synchronous=FULL` costs write throughput. Correct for a local daemon that
  acks a turn; wrong if this ever becomes a high write-rate server, which is out
  of scope.
- The event log is not a queryable store. Anything that needs a query belongs in
  SQLite, and the event log stays the durability record.
- A stolen state directory must not yield a usable bearer (amendment 3). That
  constrains what these tables may ever hold: verifiers, never secrets.

## VERIFY (none of these have been run)

- VERIFY: `rusqlite` 0.40.2 exposes the `bundled` feature and builds the
  amalgamation on this host with gcc 15.x.
- VERIFY: the pinned rusqlite API for opening a connection with `busy_timeout`
  and setting `journal_mode`/`synchronous` pragmas, checked against docs.rs for
  0.40.2 rather than assumed from an older version.
- VERIFY: `fs4` 1.1.0 advisory-lock API (`try_lock_exclusive` and friends) on
  this version, including the error shape a second writer receives.
- VERIFY: `zstd` 0.14.3 streaming encoder and decoder round-trip a sealed
  segment byte-identically.
- VERIFY: torn-tail truncation on ext4 over this NVMe, with a real `kill -9`
  between the fsync and the truncate, replays clean.
- VERIFY: a deliberately corrupt sealed segment surfaces an error and does not
  trim.

## Not in this ADR

No database is created and no migration is written in Wave 0. `rusqlite` and
`fs4` are declared in the workspace dependency table with their pinned versions.
