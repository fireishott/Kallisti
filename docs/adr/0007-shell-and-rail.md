# ADR 0007 - The application shell: a rail, and a wallpaper that is the desk

- Status: **implemented** (1.0.3)
- Date: 2026-10-19
- Wave: 1.0.3, the shell pass
- Related: `crates/eris-host/src/web/templates.rs` (`shell`, `Chrome`),
  `crates/eris-host/src/web/css.rs`, ADR 0004 (palette), ADR 0006 (chat)
- Supersedes: the top-bar shell from P12, and ADR 0006's chat-only backdrop

## Context

1.0.2 had a horizontal bar across the top and a wallpaper that only the chat page
painted. Both were wrong in the same way: the chrome was borrowed from a settings
page while the product is a workspace. Seven sections in a top bar also stop
scaling at about as many as there are, and the wallpaper read as a chat
decoration rather than as the surface the application sits on.

## Decision

### A fixed rail

`shell()` renders an `<header class="side">` rail plus a `<div class="wrap">`
surface, inside one `.app` grid.

- The rail is 236px, sticky against the viewport, solid `--bg`, hairline right
  edge. It holds the brand, the nav stacked as a column, and a foot with the
  state badge and the version.
- The current page is marked three ways: `aria-current="page"`, the raised
  background, and a 2px platinum left border. That is a marker a reader, a
  monochrome screen and a grep all agree on.
- The surface scrolls independently of the rail, so the rail is a place, not a
  header that disappears.
- Below 900px the grid collapses: the rail becomes a static bar above the
  content, the nav wraps, and the foot moves inline. Static, not sticky, because
  a rail that follows the scroll lands in the middle of a transcript on a phone.

### The wallpaper is the desk

The backdrop moved from the chat page into `Chrome`: `wallpaper_url` is resolved
once in `AppState::chrome()` from one settings row and one uploaded file, and
every page paints `<div class="app-wall">` behind the surface.

- One answer per chrome render, so no page can disagree about what the backdrop
  is.
- It is fixed, full-bleed, and behind the surface. The rail is opaque and the
  cards are opaque panels, so the wallpaper lives in the margins the way a
  desktop wallpaper lives behind windows. Contrast never depends on the picture.
- `custom` with no uploaded file falls back to the default preset rather than
  painting a hole.
- The chat column keeps a bounded height with its transcript scrolling inside it,
  so the composer and the status strip sit below the conversation instead of over
  it.

## Consequences

- Adding a section is one `Nav` variant and one route; the rail has room.
- Pages that carry no navigation (the wizard, sign-in, an error) get the same
  rail with an empty nav, which keeps the identity and the footer truthful
  without a second shell.
- The palette rule in ADR 0004 is unchanged: the rail is `--bg`, the surface is
  the wallpaper, and the cards are still thirteen colours.
- The chat-only `.chat-wall` element is gone; a page that wants a different
  backdrop needs to argue for a second setting rather than a second element.
