---
name: Craft Anything
description: Investigate how shipped software works (native, Android, Apple, .NET, JS, web, firmware) and rebuild it clean-room, using the Craft Anything engine via MCP tools or the CLI.
metadata:
  version: "1"
---

# Craft Anything: evidence-first study and rebuild

Craft Anything is a pure-Rust reverse-engineering engine: 322 CLI commands, 303 engine tools in 17
toolsets. It inspects a program, app bundle, APK, Electron app, WASM module, firmware image, crash
dump or HAR capture, and it can have a coding agent rebuild a clean-room version of it. Use it when
the answer depends on shipped bytes. If the source is available and the question is about the
source, read the source instead.

## Where it lives on this host

- CLI: `~/.local/bin/craft-anything-cli` (v0.6.0, from the official GitHub release
  tarball, SHA256 verified against the release `SHA256SUMS.txt`).
- Source clone for docs and tool lists: `~/research/storytold/craft-anything`
  (`docs/tools.md`, `docs/tool-ids.md`, `docs/STATUS.md`, `docs/ROADMAP.md`).
- Upstream: https://github.com/storytold/craft-anything
- No desktop GUI is installed. The CLI and the MCP server are the working surfaces.

## Connect

Kallisti runs the MCP server as a configured server named `craft-anything`, so its tools reach a
turn as `craft-anything__<tool>` (for example `craft-anything__core_target_open`). The default
listing is the 30-tool core set; `craft-anything__list_toolsets` shows the rest and
`craft-anything__enable_toolset` switches one on (native, android, apple, managed, javascript,
browser, firmware, formats, packets, db, game, projects, setup). Do not enable every toolset: 322
tools in one turn is a tool list the model cannot hold.

If the MCP tools are missing from the tool list, the server was not started or the daemon has not
reloaded the list. The CLI always works:

```sh
export PATH="$HOME/.local/bin:$PATH"
craft-anything-cli doctor                       # install, registrations, engines, MCP self-test
craft-anything-cli capabilities --format text   # toolsets, engine availability, current mode
craft-anything-cli commands --format json       # every tool: id, toolset, summary
```

Flag note: `--format` before the tool name sets the envelope format (json, jsonl, md, text, csv,
tsv). A tool that has its own `--format` takes that one, so use `--output-format` for the envelope.

## First moves for a target

| Goal | Command |
|---|---|
| What is this file | `craft-anything-cli triage /path/to/file` |
| Open and describe | `craft-anything-cli core-target-open /path/to/file` then `craft-anything-cli core-overview-show` |
| Ask a question about it | `craft-anything-cli chat "what does this do with X" --target /path/to/file --beginner` |
| Full report | `craft-anything-cli report --format md --output /path/report.md` |
| Rebuild it | `craft-anything-cli rebuild https://example.com "Recreate this website" --rounds 1` |

Over MCP the same steps are `core_target_open`, `core_overview_show`, `core_session_show`,
`core_feature_find`, `core_report_write`.

## Working memory (use it, do not improvise)

Long investigations outlive one context window, so the project keeps the state.

1. `core_investigation_show` first: resume from the handoff and the plan instead of restarting.
2. `core_brief_write` the question, purpose, targets, constraints (for example "do not run the
   program") and what done looks like. `core_plan_update` with short numbered steps, each
   `todo`/`doing`/`done`/`blocked` and a short reason.
3. Record the claim before testing it: `core_hypothesis_record` with the prediction that would
   confirm or refute it. Then `core_hypothesis_update` (supported, refuted, abandoned) with the
   evidence ids.
4. Settle it with `core_finding_record`, which needs at least one evidence id. Reports are built
   from findings, never from chat text.
5. End every turn with `core_handoff_write`: where things stand, the next step, the dead ends.

`core_journal_read` shows every earlier tool call and model turn when you need to know what was
already tried.

## Method

- Label every claim: observed (straight from bytes or a capture), derived (computed, for example a
  call graph), inferred (judgment about names or intent).
- Cite evidence ids (`ev_...`). A claim with no evidence id is a hypothesis.
- Absence is not proof. State the search boundary and the coverage the tool reported, and record
  an unknown with `core_unknown_add`.
- Predict, then verify. If the result disagrees with the prediction, update the hypothesis instead
  of explaining the result away.
- Large results come back as a summary plus an evidence id. Page them with `core_evidence_browse`
  instead of repeating a call that already succeeded.

## Clean room versus Deep mode

- Clean room is the default: nothing inside the target is read. Tools that would read code, files
  or page source are refused with reason `clean_room`.
- Deep mode reads code, files and page source, and uses Ghidra when it is installed. One run:
  `--deep` on the CLI or the MCP server. Persist it with `settings-set deep_mode true`.
- Deep mode is a rule about what Craft Anything offers, not an operating-system sandbox. A coding
  agent it runs still acts as the operator.

## Rebuild workflow

`craft-anything-cli rebuild <target> "<request>"` runs Study, Build, Check, Ready. Options that
matter:

- `--rounds N` refinement rounds after the first build (default 2).
- `--build-with unity|unreal|godot|rust` or a stack name for new projects.
- `--continue-in <folder>` for a follow-up request on an existing project; `--resume` to carry on
  where the last run stopped.
- `--project-folder <folder>` with `--existing edit` (edit the app in place) or
  `--existing start-from` (use the files as material, app built in `app/`).
- `--backend <name>` picks the coding agent. `--unattended` answers the builder's questions with
  the first choice and refuses approvals, so use it only when the operator asked for that.

New projects land in the projects folder from settings (default `Documents/Craft Anything`). Each
project folder holds `craft.json`, `capture/`, `app/`, `checks/`, `investigation.cany/` and
`inbox/`.

## Coding agents and AI backends

Building needs a coding agent signed in on this computer; `craft-anything-cli doctor` says what is
missing. Detected here: Claude Code, Gemini CLI, DeepSeek Harness, a DeepSeek API key in
the environment, and a local Ollama server on 127.0.0.1:11434 with no models pulled. No backend is
configured yet. Add one with `craft-anything-cli ai detect` then
`craft-anything-cli ai add --kind agent_cli --model claude-code "Claude Code"`, and choose the
builder with `craft-anything-cli ai builder`. Keep keys out of the command line: `ai add --key-env
VAR` or `--key-stdin`.

## Engines present and missing

Present: Chromium (`/usr/bin/chromium-browser`), GDB, objdump. Missing: Ghidra, Java, adb, JADX,
Apktool, Frida, LLDB, mitmproxy, binwalk, unblob, ilspycmd, tshark, rizin, rtl_433, vineflower.
So website and native-binary work is strong, while Android, Apple, .NET, firmware and packet
capture stay shallow until those engines are installed. `craft-anything-cli capabilities --format
text` lists them with paths.

## When something fails

| Error | What to do |
|---|---|
| `not_found` | Open a target first with `core_target_open`, or check the path or name |
| `feature_missing` / `engine_missing` | Read the remediation: it names the toolset, engine or setting needed |
| `over_limit` | Page it, or read it by evidence id |
| `stale_revision` | Re-read with `core_investigation_show`, merge, retry |
| `consent_required` | The operator must approve. Explain what it would do and wait |
| `clean_room` | The tool needs Deep mode. Say so instead of working around it |

## Pitfalls

- Never run `craft-anything-cli setup --yes` on a client that already lists every tool. Setup
  writes the plain `mcp` command and drops `--all-tools`, so the server falls back to the small
  core toolset. Read the client config first.
- `core_person_ask` and `core_action_approve` expect the Craft Anything desktop app to answer.
  In a chat surface with no such app, use the host's own question tool instead of waiting on a
  question nobody can answer.
- Never put secrets, cookies or personal data into a brief, finding or handoff.
- Static analysis never runs the target. Do not claim runtime behavior from static evidence; label
  it inferred.
- The upstream project is early alpha. `docs/STATUS.md` says what works today, `docs/ROADMAP.md`
  what is next. Read those before promising a result.
- Keep the offered tool list small: the core 30 plus one or two toolsets. Every listed tool costs
  context on every round of every turn.

## Keeping it current

Upstream ships releases often, so the CLI is refreshed by a timer instead of by hand.

- `~/.local/bin/craft-anything-update` asks GitHub for the newest tag and does nothing when the
  installed CLI already is that version.
- `craft-anything-update.timer` runs it daily at 04:23 local.
  `journalctl --user -u craft-anything-update.service` shows what it did.
- Every download is checked twice: the release `SHA256SUMS.txt` must carry a good GPG signature from
  the pinned ArtCraft release key (fingerprint `2B76 741B 414A AD6C D33E 6312 13A0 05C0 B9D7 C3ED`),
  and the tarball must match that signed list. A mismatch aborts and changes nothing.
- The new binary lands at `~/.local/bin/craft-anything-cli` and the old one is kept under
  `~/.local/share/craft-anything-update/backups/`. If `craft-anything-cli doctor` does not report
  healthy afterwards, the backup is restored and the run is reported as failed.
- The version and the tool counts in this file are rewritten by the same run, so this skill never
  points at an old build.
- A running MCP child is stopped after a successful swap, so the daemon starts the new binary on
  the next turn.
- Check the state by hand: `craft-anything-cli --version`, `craft-anything-cli doctor`, and
  `cat ~/.local/share/craft-anything-update/state.json`.
