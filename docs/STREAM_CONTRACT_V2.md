# Stream Contract v2

Single source of truth for the Herald v2 event envelope and vocabulary.

## Envelope

Every event in a v2 stream is a JSON object with these fields:

| Field              | Type    | Description                                      |
|--------------------|---------|--------------------------------------------------|
| `contractVersion`  | integer | Always `2`                                       |
| `jobId`            | string  | UUID identifying the job                         |
| `conversationId`   | string  | UUID identifying the conversation                |
| `attempt`          | integer | Monotonically increasing per job                 |
| `seq`              | integer | Monotonically increasing per job, starts at `1`  |
| `type`             | string  | One of the event types below                     |
| `timestamp`        | string  | ISO 8601 UTC                                     |
| `payload`          | object  | Varies by type                                   |

## Event Vocabulary

| Type                | Payload                                 | Notes                                         |
|---------------------|-----------------------------------------|-----------------------------------------------|
| `run.started`       | `{phase, attempt}`                      | First event for a job attempt                 |
| `text.delta`        | `{delta, segmentId}`                    | Incremental assistant text                    |
| `reasoning.delta`   | `{delta, segmentId}`                    | Incremental reasoning/thinking                |
| `tool.started`      | `{toolCallId, name, args}`              | Tool invocation beginning                     |
| `tool.progress`     | `{toolCallId, label}`                   | Tool progress update                          |
| `tool.completed`    | `{toolCallId, output}`                  | Tool invocation finished                      |
| `commentary`        | `{text}`                                | System commentary                             |
| `approval.required` | `{toolCallId, prompt}`                  | Awaiting user approval                        |
| `run.completed`     | `{messageId, text, usage, diff}`        | **Terminal** — successful completion          |
| `run.failed`        | `{error, retryable}`                    | **Terminal** — unrecoverable failure          |
| `run.cancelled`     | `{reason}`                              | **Terminal** — user/system cancelled          |
| `run.requeued`      | `{fromAttempt, toAttempt}`              | Job moved to new attempt                      |

## Terminal Rules

- Exactly one terminal event per job (`run.completed`, `run.failed`, or `run.cancelled`)
- Terminal events are immutable — late nonterminal events are rejected
- `attempt` in terminal must match the current attempt

## v1-to-v2 Compatibility Map

| v2 event           | v1 equivalent                          |
|--------------------|----------------------------------------|
| `text.delta`       | `text_delta`                           |
| `reasoning.delta`  | `reasoning_delta`                      |
| `tool.started`     | `tool_activity` (start phase)          |
| `tool.progress`    | `tool_activity` (progress phase)       |
| `tool.completed`   | `tool_activity` (end phase)            |
| `run.completed`    | `done` with `status: completed`        |
| `run.failed`       | `done` with `status: failed`           |
| `run.cancelled`    | *(new — no v1 equivalent)*             |
| `run.requeued`     | *(new — no v1 equivalent)*             |
| `commentary`       | *(new — no v1 equivalent)*             |
| `approval.required`| *(new — no v1 equivalent)*             |
