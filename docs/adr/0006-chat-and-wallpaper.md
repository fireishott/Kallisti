# ADR 0006 - The chat surface, one inline script, and the wallpaper

- Status: **implemented** (1.0.1)
- Date: 2026-10-19
- Wave: 1.0.1, the rich chat pass
- Related: `crates/eris-host/src/web/chat.rs`,
  `crates/eris-host/src/web/wallpaper.rs`,
  `crates/eris-host/src/web/templates.rs`, `crates/eris-host/src/web/css.rs`,
  ADR 0004 (the chrome palette), ADR 0002 (storage)
- Supersedes: the "no `<script>` anywhere" sentence in ADR 0004, for one page and
  under the conditions below

## Context

1.0 shipped an admin panel with no conversation surface at all: the only way to
talk to the daemon was the device listener, which is loopback and expects a
paired phone. The dashboard could rotate a key, show a job timeline, and empty a
transcript it could not create.

Three constraints met the request for a real chat UI:

1. **The pages are server-rendered, with no build step and no CDN.** A streaming
   answer needs a client that reads an event stream, and there is nothing on the
   page to run one.
2. **`curl` must still be able to drive every form.** The wizard and every
   settings page are plain POSTs, and the tests treat that as a contract.
3. **The product has a face, and the chat is where people will spend their time.**
   A flat grey column does not carry it.

## Decision

### One page, one inline script, as enhancement

The chat composer is a real `form` that POSTs to `/dashboard/chat/send`. With
JavaScript disabled, or blocked, the POST runs the turn to completion and
redirects back to the transcript, exactly like every other form in the daemon.
`curl -d message=... /dashboard/chat/send` works with no script at all.

When the page's one inline script runs, it sends the same body with
`Accept: text/event-stream`, reads the event stream, and appends deltas into the
bubble it created. It is:

- inline (no `src`, so nothing is fetched and no third party is involved);
- on exactly one page, asserted by the shell sweep, which fails if any other
  page carries a script or if the chat page carries more than one;
- free of network requests of its own: the only fetch is the turn.

That is a deliberate, narrow reversal of ADR 0004's blanket "no `<script>`"
sentence. The rule that survives is the one that mattered: **the UI works without
it**, and no page loads code it did not author.

### The transcript is a table, the turn is the provider seam

`chat_messages` (migration 0005) is the record: one row per message, a
conversation id so a thread list does not need a rewrite later, a closed `state`
set, and the provider's usage kept beside the reply it belongs to.

The turn reuses the same provider seam, persona file and key reference as the
device surface. It does **not** reuse the job/lease/resume machinery: a browser
has one tab and one socket, and giving it a second job engine would mean two
answers to "what is running". History is bounded to the last 24 settled messages,
and a message that is still streaming or that failed is never sent as if it were
an answer.

### The wallpaper is an image, never a page

- Three presets are generated SVGs shipped in the binary, plus `none` and, once
  uploaded, `custom`. No binary asset is committed and nothing is fetched.
- An upload is accepted by **magic bytes**, not by filename or by the browser's
  `Content-Type`: PNG, JPEG and WebP only, up to 8 MiB. SVG is refused outright,
  because an SVG can carry script and a wallpaper is not worth an injection.
- The file is written beside its destination and renamed, so a half-uploaded
  image is never served.
- The chosen wallpaper paints only the chat backdrop. The chrome keeps ADR 0004's
  palette, so contrast never depends on the picture.

## Consequences

- The palette test still governs the stylesheet; wallpapers are image assets and
  carry their own colours, which is why the SVG presets may use gradients while
  the UI may not.
- A second page that wants a script has to argue for it in this ADR. The sweep
  will fail until it does.
- `settings` gains two keys (`appearance.wallpaper`,
  `appearance.wallpaper_content_type`) and the schema gains one table; the
  backup archive already copies both because it copies the database and the
  config directory.
- Version is 1.0.1. The health endpoint, the footer and the tests all report the
  one value from `[workspace.package]`.
