---
name: local-model-coding
description: Use when code work should run on the local GGUF models (llama.cpp router on the Studio) instead of a paid provider: writing, fixing, debugging, choosing a model by measurement, and the local-code tool that closes a test loop.
---

# Local Model Coding and Debugging

The local inference path on this host does real code work. Use it when a task is
mechanical, private, or too numerous to pay for, and when provider budget is the
thing standing between Curtis and the change.

## When to Use

- A small, well-specified code change with a test that decides pass or fail.
- Bulk passes: many files, many similar fixes, lint-to-green, docstring and
  type-annotation sweeps.
- Anything that should not leave the house.
- Measuring which local model is worth handing a task to.

Not for architecture, repo-wide refactors, or anything needing tool use. Those
belong to the frontier model or the coding harness.

## Where the models are

Inference runs on `fih-mac-studio` (Apple M5 Max, 36 GB) behind a llama.cpp
router. This host is 2 cores, no GPU, 31 GB RAM: a client, never an inference
box. Never try to run a GGUF here.

- Base URL `http://192.168.68.88:8080/v1`, OpenAI-compatible.
- Auth `Authorization: Bearer $STUDIO_LOCAL_API_KEY` from `~/.config/kallisti/eris.env`.
- `GET /props` reports `max_instances: 1`, `models_autoload: true`: one model
  resident, swap on request, so back-to-back calls to different models pay a reload.
- Inventory 2026-11-26: `qwen3.5-4b`, `lfm2.5-24b-a2b`, `vl-qwen3-8b`,
  `qwen3-30b-a3b-existing`, `nemotron-3.5-lightning`.
- Ollama on `127.0.0.1:11434` holds only `nomic-embed-text`. Embeddings only;
  never aim a chat completion at it.

## Which model, measured

`local-code bench`, one greedy prompt, verdicts by executing the answer:

| model | verdict | speed | note |
|---|---|---|---|
| `vl-qwen3-8b` | PASS | 3.1 s, 71 tok/s | quick, correct, no reasoning overhead |
| `lfm2.5-24b-a2b` | PASS | 3.5 s, 156 tok/s | fastest of the 20B class |
| `qwen3-30b-a3b-existing` | PASS | 15.5 s, 112 tok/s | reasons in `reasoning_content`, needs 6000 tokens |
| `nemotron-3.5-lightning` | PASS | 18.8 s, 91 tok/s | same reasoning budget trap |
| `qwen3.5-4b` | FAIL | 2.0 s, 106 tok/s | fast but sloppy at 900 tokens; in the fix loop it converged on attempt 3 |

Throughput is similar across all five; latency is not. The big models are worth
it only when the task needs reasoning, and they cost 5 to 10 times the wall clock.

## The two traps that waste an afternoon

**Reasoning models look empty.** `qwen3-30b-a3b` and `nemotron` put everything
in `message.reasoning_content` and return `message.content == ""` with
`finish_reason: length` when the cap is small. The answer is not missing, it is
in the other field. Budget 6000 tokens for them and read both fields.

**Small GGUFs answer a file-editing prompt with a tool call.** Put a filesystem
path in the prompt to `lfm2.5-24b-a2b` and it replies
`<|tool_call_start|>[bash(command='cat /path/work.py')]<|tool_call_end|>`, or
`[write(code='...')]`, instead of a fenced file. There is no tool here. Two
fixes, both applied in `local-code`: never put a path in the prompt (label the
code block `### FILE`), and recover `write(code='...')` payloads in the parser.

## Capability: local-code

`local-code`, symlinked into `~/.local/bin`, source at
`~/kallisti-workspace/local-ai/local-code`, stdlib Python only.

```
local-code models
local-code ask "<question>" [--file F ...] [--deep] [--code] [--max-tokens N] [--system S]
local-code fix <file> --test "<cmd>" [--attempts 3] [--model M] [--keep-attempts]
local-code bench [--models a,b]
```

`fix` is the capability that matters: run the decider, hand the file plus the
failing output to the model, take the fenced file back, write it, re-run, repeat.
It keeps `*.localcode.bak`, restores the original and exits non-zero when the
loop ends red, and `--keep-attempts` saves every candidate plus the raw reply so
a failure can be read instead of guessed. Temperature ramps from 0 on attempt 1
to 0.4 then 0.8: at temperature 0 every retry is byte-identical, which is how a
loop spends three attempts producing the same wrong file.

### Measured loop result

Broken `largest_even` (returns 0 instead of None, drops negative evens) against
three pytest cases, same file, same test, `--attempts 3`:

- `qwen3.5-4b` GREEN on attempt 3, 4 s total
- `lfm2.5-24b-a2b` GREEN on attempt 1, 2 s
- `vl-qwen3-8b` GREEN on attempt 1, 3 s
- `qwen3-30b-a3b-existing` GREEN on attempt 1, 18 s
- `nemotron-3.5-lightning` GREEN on attempt 1, 18 s

Default model for the loop is `qwen3.5-4b` for speed; reach for
`lfm2.5-24b-a2b` when the 4B thrashes, and a reasoning model only when the bug
is a logic puzzle rather than a typo.

### The loop that works

1. Write the decider first. Without a test the local model is guessing.
2. Point the test at the exact module the tool rewrites. A test that imports
   `broken` while `fix` edits `work.py` reports red forever and looks like model
   failure; that mistake cost a full round here.
3. Run `fix` from any directory: it executes the decider in the target file's
   own directory, so relative pytest paths resolve.
4. Read the diff against `.localcode.bak` before trusting green. Green proves
   the test passed, not that the change is right.
5. Escalate model, not attempts, when two attempts fail the same way.

## Pitfalls

- Do not fire parallel requests at different models. One instance means they
  serialise behind a reload and each pays a cold start; a five-model sweep takes
  over a minute in reloads alone.
- The router sleeps an idle model after 900 s. The first call after a quiet
  stretch carries the load; do not read it as throughput.
- Never repoint `STUDIO_ROUTER` at a paid provider. `local-code` has no fallback
  on purpose so a failure stays visible instead of getting billed.
- A whole repo as `--file` context does not fit and truncates silently. Send the
  file under repair plus the interfaces it touches.
- A test file can keep a stale module import across runs; if a fix looks wrong,
  run the decider by hand in that directory before blaming the model.

## Verification

- `local-code models` lists the inventory: the path is alive.
- `local-code bench` prints a verdict per model, decided by executing the code,
  plus seconds, tok/s and the content/reasoning split.
- `local-code fix` on a deliberately broken scratch file must report GREEN and
  the suite must still pass outside the tool.
