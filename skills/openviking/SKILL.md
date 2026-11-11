---
name: openviking
description: Use when storing or retrieving long-lived context (knowledge, memory, skills) in the local OpenViking server, or when the operator asks about viking:// URIs, ov CLI commands, or the OpenViking MCP bridge.
version: 1.1.0
author: Ignyte
license: AGPL-3.0
platforms: [linux]
---

## When to Use
- The operator asks to put documents, repos or notes into a searchable context store.
- A question needs retrieval over a corpus too large to read directly (thousands of files).
- Durable preferences or decisions should outlive this conversation and OpenViking is running.
- Not for: ordinary file reads in the workspace, or vault notes (use the vault path directly).

## What Is Running Here
- Package: `openviking` 0.5.4.1, installed as a uv tool: `~/.local/bin/openviking-server`, `~/.local/bin/ov`.
- Server: `openviking.service` (systemd --user, **enabled**), `http://127.0.0.1:1933`, `auth` mode `dev`, loopback only. MCP server reports itself as `openviking 1.28.1` on protocol `2024-11-05`.
- Workspace: `/home/fihadmin/.openviking/data`. Config: `~/.openviking/ov.conf` (mode 600). CLI config: `~/.openviking/ovcli.conf` (profile `local`), language in `~/.openviking/ovcli.settings.conf`.
- Models: VLM `ds/deepseek-v4-flash-vision-exp` via 9router `127.0.0.1:20128/v1`; embeddings `nomic-embed-text` on Ollama `127.0.0.1:11434/v1`, 768 dims. Never point embeddings at a model with a different dimension than the stored vectors.
- Data on disk: ~117 MB under `~/.openviking`, of which the vector store is a few MB and grows as imports finish.

## Procedure
- Check before acting: `ov status` (`/ready` also works). A reachable port is not health: `/ready` must show `agfs`, `vectordb`, `embedding` all `ok`.
- List what exists before importing: `ov ls viking://resources/` and `ov tree viking://resources/<project> -L 2`.
- Import with `ov add-resource <url|path>`; it returns a `task_id` in ~100 ms and processes in the background. A repo the size of volcengine/OpenViking (5990 files) takes 15+ minutes because every directory gets an L0/L1 summary.
- Read the queue as two numbers, not one: `ov task list -o json` shows the task rows, `ov status` shows `Pending`/`In progress`. Pending went 0 -> 369 -> 26 during one import, so a nonzero Pending is progress, not a stuck queue.
- Retrieve with `ov find "<query>" -u viking://resources/<project> -n 8`. Scope the URI. A scoped query on the imported repo answered in 89 ms once indexing caught up; the same query against a cold index took 9.2 s and could have hit the daemon's 20 s MCP call cap.
- Escalate deliberately: `ov tree`/`ov ls` for shape, `ov abstract` (L0) and `ov overview` (L1) before `ov read` (L2). Reading full files first burns tokens for nothing.
- Content search (exact tokens) is `ov grep "<pattern>" -u <uri>`. It returns matches with line numbers and file URIs, and it works even while summaries are still being built.
- Durability: `ov add-memory "<fact>"`, then `ov find "<topic>"` to prove it is retrievable.
- Sessions: `ov session new`, `ov session add-message <id> --role user --content "..."`, `ov session commit <id>` archives the conversation and extracts memories.
- Other commands worth knowing: `ov stat`, `ov attrs`, `ov acl`, `ov get`, `ov write`, `ov set-tags`, `ov reindex <uri> --mode vectors_only`, `ov export`/`ov import` (.ovpack), `ov snapshot`, `ov mcps` (remote MCP servers OpenViking itself consumes), `ov tui`.

## MCP Bridge
- The daemon registers `openviking` as a stdio MCP server in the `mcp.servers` settings row of `~/.local/share/kallisti/eris.db`:
  `{"name": "openviking", "command": "/usr/bin/python3", "args": ["/home/fihadmin/.config/kallisti/mcp/openviking-bridge.py"]}`.
  It sits alongside `echo` and `craft-anything`; the stored list is now three servers.
- Tools appear to a turn as `openviking__ov_find`, `openviking__ov_read`, `openviking__ov_ls`, `openviking__ov_tree`, `openviking__ov_grep`, `openviking__ov_add_resource`, `openviking__ov_remember`, `openviking__ov_status`, `openviking__ov_tasks`.
- The bridge is a thin wrapper over the CLI, so its output matches what `ov` prints. Every non-zero exit becomes `{"ok": false, "error": "..."}`; treat that as a CLI failure, never as an empty corpus.
- Handshake verified directly against the bridge: `initialize` returns `2024-11-05` / `openviking 1.28.1`, `tools/list` returns 9 tools, and stdout carries only JSON-RPC lines (the `mcp` python package logs to stderr).
- **The pool is built once at daemon construction.** `Pool::new` runs in `AppState::new` and there is no reload route, so a saved server is invisible to turns until `kallisti-eris.service` restarts. Saving the settings row is half the job; the restart is the other half.
- Timings that matter: the daemon gives an MCP request 20 s (`REQUEST_TIMEOUT`) and a handshake 25 s (`START_TIMEOUT`). A cold bridge handshake takes ~1.5 s, so handshakes are safe; a cold `ov find` is not.
- The bridge runs as a child of the daemon, inside `ProtectHome=read-only`. It needs no write access to `$HOME` because every operation is a call to the loopback server; keep it that way.

## Pitfalls
- `ov` refuses to run until a display language is set once: `ov language en`.
- `openviking-server init` is an interactive wizard; writing `~/.openviking/ov.conf` and then running `openviking-server doctor` is the non-interactive path. `doctor` prints `All checks passed`.
- `doctor` reporting `Ollama: PASS not configured` is normal: Ollama is reached through the OpenAI-compatible embedder, not the built-in Ollama provider.
- `openviking-server --bot` needs a separate VikingBot process. Do not add that flag to fix a missing feature.
- Root API keys cannot cancel tasks (`ROOT may not cancel tasks`); use a user key for task control.
- Do not delete or rebuild `/home/fihadmin/.openviking/data` to fix retrieval: that discards every vector and memory. `ov reindex <uri> --mode vectors_only` instead.
- Web Studio is served by the same server at `http://127.0.0.1:1933/studio`; no extra port.
- A `systemd-run --user --wait` wrapper is the way to prove the bridge survives the daemon's sandbox: pass `--property=ProtectHome=read-only --property=ProtectSystem=strict` and it still answers, because the work happens over HTTP.

## Verification
- `curl -s http://127.0.0.1:1933/ready` shows `agfs`, `vectordb`, `embedding` all `ok`.
- `openviking-server doctor` prints `All checks passed`.
- `ov find "<known phrase>"` returns a `viking://` URI that `ov read <uri>` opens with the expected text.
- Bridge handshake without any MCP client library:
  `printf '%s\n' '<initialize>' '<notifications/initialized>' '<tools/list>' | /usr/bin/python3 ~/.config/kallisti/mcp/openviking-bridge.py` returns exactly two JSON lines and 9 tools.
- After a daemon restart, the journal carries `eris: 3 MCP server(s) configured` and the admin listener answers on 7340.

## Verified on this host, 2026-11-11 (read from the box, not assumed)

- Server: `openviking-server` 0.5.4.1, user unit `openviking.service`, enabled,
  `127.0.0.1:1933`, `/ready` = agfs/vectordb/embedding ok. The unit was found
  **inactive** after a reboot; `systemctl --user start openviking.service` is the fix
  and `/ready` is the check. A dead server means every bridge call returns a CLI error.
- Config: `~/.openviking/ov.conf` (mode 600). Embeddings `nomic-embed-text`, 768 dims,
  via Ollama. VLM `ds/deepseek-v4-flash-vision-exp` via 9router. `ov status` reports
  models healthy "VLM + embedding available".
- The daemon reads MCP config from `~/.local/share/kallisti/eris.db`, table `settings`,
  key `mcp.servers`. `~/.config/kallisti/eris.db` is a 0-byte decoy - do not edit it.
- Bridge: `~/.config/kallisti/mcp/openviking-bridge.py`, run as
  `/usr/bin/python3 <that path>`, 9 tools, namespaced `openviking__ov_*` to the model.
- The bridge is proven inside the daemon sandbox (`ProtectHome=read-only`,
  `ProtectSystem=strict`) with `systemd-run --user --scope`-style properties; `ov_status`,
  `ov_remember` and `ov_find` all answer there. Test in that sandbox, not in a bare shell.
- Chat turns in the dashboard get MCP tools (`web/chat.rs` passes `Some(&mcp)`).
  **Device/phone turns do not**: `device/turns.rs` passes `None // MCP servers`. A phone
  turn can never call OpenViking until that argument is wired.
- The MCP client caps one request at `REQUEST_TIMEOUT` 20s and a handshake at 25s. A cold
  `ov` import or a slow embedding answer can exceed that, so keep tool calls small.
- Checking whether the tools are actually used: count real invocations, not prose about
  them, with
  `sqlite3 "file:$HOME/.local/share/kallisti/eris.db?mode=ro" "select count(*) from chat_messages where tools like [{\"name\":\"openviking__%;"`.
