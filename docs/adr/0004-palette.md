# ADR 0004 - Palette: the closed hex list, and the purple denylist

- Status: **implemented** (P7, onboarding wizard and dashboard)
- Date: 2026-10-19
- Wave: P7
- Related: plan section 10.5, `crates/eris-host/src/web/css.rs`,
  `crates/eris-host/src/web/templates.rs`
- Supersedes: nothing

## Context

The UI is one server-rendered stylesheet with no build step, no npm, no CDN and
no web font, so the colour set lives in a Rust `const` and nowhere else. That is
convenient, and it is also how a twelfth shade of grey quietly appears at 11pm.

Two things have to be true instead:

1. every hex the UI can paint is listed here, so a reviewer can diff the list
   against the stylesheet in one pass;
2. the purple family 1.0 refuses is absent from the rendered HTML, and that is a
   test, not a style note.

## Decision

### The allowed set: thirteen colours

| Hex | Token | What it paints |
|---|---|---|
| `#0B0D10` | `--deep` | Code blocks, the deepest surface |
| `#0F1115` | `--bg` | Page background |
| `#161A20` | `--panel` | Cards, tables, the wizard body |
| `#1D2229` | `--raise` | Inputs, radio cards, anything that sits on a card |
| `#262C35` | `--line` | Hairline borders and table rules |
| `#394150` | `--line-strong` | Borders that need to read at a glance |
| `#E6E9EE` | `--text` | Body text |
| `#9AA4B2` | `--muted` | Secondary text, hints, captions |
| `#7DD3A0` | `--ok` | The good state (ready, saved, notice) |
| `#E8C07D` | `--warn` | The transitional state (setup) |
| `#E07A7A` | `--bad` | A refusal, a validation error, a destructive action |
| `#6EA8FE` | `--link` | Links, focus rings, the selected radio border, the primary button |
| `#000000` | `--black` | Reserved for alpha compositing; not used as a fill on its own |

Thirteen. Not fourteen. A new colour is a change to this table and to the
`const`, in the same commit, with a reason.

The set is deliberately dark-neutral: one background ramp, one text ramp, three
semantic accents. There is no decorative colour and no gradient. A permission
table that has to be read carefully is not the place for a mood.

### Typography and surfaces

- System font stack only: `-apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto,
  Helvetica, Arial, sans-serif`. No `@font-face`, no `url()`, no `@import`.
- Monospace: `ui-monospace, 'SF Mono', Menlo, Consolas, monospace`, for ids,
  paths, timings and pairing codes.
- No `<script>` anywhere. The wizard is a progressive form: plain POST, then a
  303 to the next step, and a failed POST re-renders the same page with status
  400 and the message in an `aria-live` region.

### The purple denylist

These six values are refused in any rendered HTML, in any case:

```
#800080  #7B1FA2  #6A1B9A  #9C27B0  #8E24AA  #673AB7
```

They are the Material purple family plus the web "purple" keyword. The list is
enforced twice:

- `web/css.rs` has a unit test that fails if any of them appears in the
  stylesheet, and a second test that fails if a hex outside this table appears in
  it;
- `tests/phase7.rs` greps the rendered `/setup` HTML, and
  `web/templates.rs` greps the rendered wizard, dashboard and error pages.

The acceptance run repeats the grep against the served bytes, so the check is on
what a browser receives rather than on what the source says.

### Colour is never the only signal

Every semantic colour is paired with text: the state badge says `setup` or
`ready`, an error carries its code and message, a locked row says `locked` and
gives the reason, and the selected radio is also the one the browser reports as
`checked`. That is an accessibility requirement, not a preference.

## Consequences

- A UI change that needs a new colour needs an ADR edit. That is the point.
- The palette test reads the stylesheet's own custom properties, so a hex added
  outside the `:root` block also fails, not just a new token.
- The dark palette is the only palette. There is no light theme in 1.0, so there
  is no second set of hexes to keep in sync.
- Red and amber are used only for refusal and transition, never for decoration,
  which keeps "something is wrong" unambiguous on a page that is mostly grey.
