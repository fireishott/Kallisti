---
name: Craft Anything DSH Backend
description: Configure and verify DSH as a Craft Anything backend.
---

## When to Use
- Use when adding DeepSeek Harness (`dsh`) as a Craft Anything `agent_cli` backend and selecting it as the default builder.

## Procedure
- Validate standalone DSH in the intended noninteractive profile: `echo 'Reply with the single word PONG.' | dsh --profile headless`; require a successful `PONG` response.
- Register the adapter with `craft-anything-cli ai add --kind agent_cli --model dsh "DeepSeek Harness"`.
- Select it as the builder with `craft-anything-cli ai builder "DeepSeek Harness"`.
- Confirm persistence with `craft-anything-cli ai list --format text`, `craft-anything-cli settings-show --format text`, and `~/.config/craft-anything/config.toml`.
- Run `craft-anything-cli ai test "DeepSeek Harness" --format text` and require a successful exit.
- Create a disposable directory containing `probe.txt` with a unique token, then run `timeout 300 craft-anything-cli chat "Read probe.txt and reply with its contents." --backend "DeepSeek Harness" --target "$(command -v dsh)" --deep --format text` from that directory.

## Verification
- Confirm the backend and builder appear in the CLI output and persisted configuration.
- Confirm the adapter launches DSH with Craft Anything's generated MCP overlay.
- Require the end-to-end chat to return the exact token from `probe.txt`.

- Verify DSH through Craft Anything, not only with `doctor` or a direct DSH prompt: create an isolated target containing `probe.txt` with a unique token, run `craft-anything-cli chat "Read probe.txt in the current folder and reply with its exact contents." --backend "DeepSeek Harness" --target "$TARGET_DIR" --deep --format text`, and require the exact token in the reply so the generated MCP overlay is exercised.
- When Playwright Chromium is blocked by host AppArmor or user-namespace policy, configure `engines.tools.chromium` with `craft-anything-cli settings-set --key engines.tools.chromium --value "$HOME/.local/bin/chromium-craft" --output-format text`; ensure the wrapper adds `--no-sandbox` and verify `settings-show` plus a headless DOM render, otherwise browser-backed rebuilds fail at launch.
- Require `craft-anything-cli rebuild` to exit successfully and inspect its generated output before calling the full integration complete; a running PID and passing component probes do not prove rebuild completion.

## Pitfalls
- Do not equate registration or a launched DSH process with end-to-end success, because the generated MCP path can still be pending.
- Treat a detached chat as pending rather than successful, because no final response means the integrated path is unverified.
- Use `set -o pipefail` when piping timed commands, because `$?` otherwise reports the final filter and can hide a timeout.
