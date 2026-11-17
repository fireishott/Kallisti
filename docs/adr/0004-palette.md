# ADR 0004 - Palette: the closed hex list, and the purple denylist

- Status: **implemented** (wizard, dashboard, settings, sessions, devices,
  activity, backups, sign-in)
- Date: 2026-10-19, revised the same day for the Kallisti identity pass
- Wave: P7, then the theme pass
- Related: `crates/eris-host/src/web/css.rs`, `crates/eris-host/src/web/templates.rs`
- Supersedes: the first revision of this ADR, which used a cold grey ramp and a
  blue accent (`--link`, `#6EA8FE`)

## Context

The UI is one server-rendered stylesheet with no build step, no npm, no CDN and
no web font, so the colour set lives in a Rust `const` and nowhere else. That is
convenient, and it is also how a twelfth shade of grey quietly appears at 11pm.

Two things have to be true instead:

1. every hex the UI can paint is listed here, so a reviewer can diff the list
   against the stylesheet in one pass;
2. the purple family 1.0 refuses is absent from the rendered HTML, and that is a
   test, not a style note.

The first revision was a competent grey admin panel. It was not the product. The
product identity is obsidian and platinum, decided long before this daemon and
already carried by the app, the site and the icons. A dashboard that belongs to
the same product should not be the one surface painted in a different language,
so the ramp was rebuilt around the brand values and the accent moved from blue to
platinum.

## Decision

### The allowed set: thirteen colours

| Hex | Token | What it paints |
|---|---|---|
| `#06070A` | `--deep` | Code blocks, the pairing code plate, the deepest surface |
| `#0C0C10` | `--bg` | Page background, obsidian. The brand ground |
| `#131317` | `--panel` | Cards, tables, the wizard body |
| `#1A1B20` | `--raise` | Inputs, radio cards, the current nav item, hovered table rows |
| `#24252B` | `--line` | Hairline borders, table rules, the rule under a card title |
| `#34363D` | `--line-strong` | Borders that need to read at a glance, dashed empty states |
| `#E9EAEE` | `--text` | Body text |
| `#9B9DA5` | `--muted` | Secondary text, hints, captions, micro labels |
| `#C8CCD2` | `--platinum` | The accent: links, focus rings, the selected radio, the primary button, the active nav rule, the pairing code |
| `#86C7A0` | `--ok` | The good state (ready, saved, notice, a live session) |
| `#D8B57C` | `--warn` | The transitional state (setup, expired) |
| `#D98A8A` | `--bad` | A refusal, a validation error, a revoked session, a destructive action |
| `#000000` | `--black` | Reserved for alpha compositing; not used as a fill on its own |

Thirteen. Not fourteen. A new colour is a change to this table and to the
`const`, in the same commit, with a reason.

The set is deliberately dark-neutral with one metal accent: one background ramp,
one text ramp, platinum for anything interactive, and three semantic accents that
only ever mean good, transitional, or refused. There is no decorative colour and
no gradient. A permission table that has to be read carefully is not the place
for a mood, and a gradient is how a settings page starts to look like a landing
page.

### Typography and surfaces

- System font stack only: `-apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto,
  Helvetica, Arial, sans-serif`. No `@font-face`, no `url()`, no `@import`.
- Monospace: `ui-monospace, 'SF Mono', Menlo, Consolas, monospace`, for ids,
  paths, timings, pairing codes and every number that is read as a value.
- Numerals are tabular wherever they sit in a column or a tile, so a count that
  changes does not shift the layout.
- No `<script>` anywhere. The wizard is a progressive form: plain POST, then a
  303 to the next step, and a failed POST re-renders the same page with status
  400 and the message in an `aria-live` region.
- The top bar is sticky and hairline-separated, the brand carries a monogram
  tile, and every card title is followed by a rule so a long page reads as
  sections rather than as one scroll.
- Micro labels (card eyebrows, table headers, stat captions, the footer
  signature) are uppercase at 10-11px with wide tracking. The body never is.
- Empty states are dashed plates, not a bare sentence, so a page with no data
  still looks finished.
- Below 700px a table keeps its columns and scrolls inside its own card rather
  than crushing prose into a narrow gutter, and the state badge moves up beside
  the brand so the top bar stays two rows instead of three.

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
  which keeps "something is wrong" unambiguous on a page that is mostly obsidian.
- Platinum is a low-chroma accent: it reads as interactive without competing with
  the three semantic colours, which is why the primary button, the link and the
  focus ring can all be the same token.
- Inline `style=` colour attributes are gone from the templates. Every colour a
  page can paint now comes through this stylesheet, so the list above is the whole
  surface area and the test can be exhaustive.
