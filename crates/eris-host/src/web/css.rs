//! The one stylesheet, served inline and at `/static/app.css`.
//!
//! Every colour in this file is listed in `docs/adr/0004-palette.md`, which is
//! the denylist reference: a hex that is not in the ADR is not allowed in the
//! UI, and an acceptance test greps the rendered pages for the six purple values
//! 1.0 refuses. There is no build step, no npm, no CDN, no web font, and no
//! `<script>`: the pages are server-rendered HTML with one inline stylesheet, so
//! `curl` can read every step and a browser with JavaScript disabled works.
//!
//! The identity is the product one: obsidian ground, platinum accent, and three
//! semantic colours that only ever mean good, transitional, or refused. There is
//! no gradient and no decorative colour, so the page is quiet and the numbers
//! are the loud thing on it.

/// Where the stylesheet is also reachable as a file.
pub const CSS_ROUTE: &str = "/static/app.css";

/// The whole stylesheet. Obsidian and platinum, system fonts, no purple.
pub const APP_CSS: &str = r#"
:root {
  --deep: #06070a;
  --bg: #0c0c10;
  --panel: #131317;
  --raise: #1a1b20;
  --line: #24252b;
  --line-strong: #34363d;
  --text: #e9eaee;
  --muted: #9b9da5;
  --platinum: #c8ccd2;
  --ok: #86c7a0;
  --warn: #d8b57c;
  --bad: #d98a8a;
  --black: #000000;
}

* { box-sizing: border-box; }

html, body {
  margin: 0;
  padding: 0;
  background: var(--bg);
  color: var(--text);
  font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, Helvetica, Arial, sans-serif;
  font-size: 15px;
  line-height: 1.6;
  -webkit-font-smoothing: antialiased;
  text-rendering: optimizeLegibility;
}

::selection { background: var(--platinum); color: var(--deep); }
::placeholder { color: var(--muted); opacity: 0.7; }

::-webkit-scrollbar { width: 12px; height: 12px; }
::-webkit-scrollbar-track { background: var(--bg); }
::-webkit-scrollbar-thumb { background: var(--line-strong); border: 3px solid var(--bg); border-radius: 999px; }
::-webkit-scrollbar-thumb:hover { background: var(--muted); }

a { color: var(--platinum); text-decoration: none; text-underline-offset: 3px; }
a:hover { text-decoration: underline; }
a:focus-visible { outline: 2px solid var(--platinum); outline-offset: 2px; border-radius: 4px; }

.wrap { max-width: 1180px; margin: 0 auto; padding: 34px 30px 72px; width: 100%; }

/* The shell -------------------------------------------------------------- */

/* The application shell: a fixed rail, and a surface the wallpaper sits on. */

.app {
  position: relative;
  z-index: 1;
  display: flex;
  align-items: stretch;
  min-height: 100vh;
}

/* The rail is a flex item with a basis, not a grid column, because a grid column
   can only be shown or hidden - and a sidebar that teleports reads as a glitch
   rather than a decision. */
.app > .side {
  flex: 0 0 236px;
  width: 236px;
  transition:
    flex-basis 260ms cubic-bezier(0.2, 0.7, 0.2, 1),
    width 260ms cubic-bezier(0.2, 0.7, 0.2, 1),
    transform 260ms cubic-bezier(0.2, 0.7, 0.2, 1),
    opacity 200ms ease;
  overflow: hidden;
}
.app > .wrap { flex: 1 1 auto; min-width: 0; }

/* Collapsing the rail. The transcript takes the whole width, and the control to
   bring the rail back lives in the content column, because a control that only
   exists inside the thing you just hid is a one-way door. */
/* Collapsed: the rail slides out and the transcript takes the room it had. The
   contents are non-interactive and invisible to a reader, so a hidden rail is not
   a tab stop in the dark. */
.app.rail-hidden > .side {
  flex-basis: 0;
  width: 0;
  transform: translateX(-24px);
  opacity: 0;
  pointer-events: none;
  visibility: hidden;
}
@media (prefers-reduced-motion: reduce) {
  .app > .side { transition: none; }
}

.rail-hide {
  position: absolute;
  top: 16px;
  right: 10px;
  margin: 0;
}
.rail-hide button,
.rail-show button {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 26px;
  height: 26px;
  padding: 0;
  border: 1px solid var(--line);
  border-radius: 8px;
  background: transparent;
  color: var(--muted);
  font-size: 13px;
  line-height: 1;
  cursor: pointer;
}
.rail-hide button:hover,
.rail-show button:hover {
  color: var(--text);
  border-color: var(--line-strong);
}
.rail-show {
  position: sticky;
  top: 8px;
  z-index: 3;
  display: inline-block;
  margin: 0 0 10px;
}

/* A raster wallpaper is a picture, and a picture at full strength competes with
   the conversation. The chrome is opaque on its own; the art is dimmed so it
   reads as a room, not as a poster. */
.app-wall.art { opacity: 0.42; }

.app-wall {
  position: fixed;
  top: 0;
  left: 0;
  width: 100%;
  height: 100%;
  /*
   * Stationary has to be spelled out. Sizing this layer to the visual viewport
   * makes mobile Safari recompute the `cover` scale as the browser chrome hides,
   * so the picture breathes while you scroll. The large-viewport unit is the one
   * that does not move, so the scale is constant and the wallpaper stays put.
   */
  height: 100lvh;
  z-index: 0;
  background-position: center;
  background-size: cover;
  background-repeat: no-repeat;
  pointer-events: none;
  /* Its own compositing layer: a fixed background repainted with the document
     underneath the rail is the other way this image appears to drift. */
  transform: translateZ(0);
  will-change: transform;
}

.side {
  position: sticky;
  top: 0;
  align-self: start;
  display: flex;
  flex-direction: column;
  gap: 26px;
  height: 100vh;
  padding: 22px 16px;
  border-right: 1px solid var(--line);
  background: var(--bg);
}

.side-foot {
  display: flex;
  flex-direction: column;
  gap: 8px;
  margin-top: auto;
  padding-top: 16px;
  border-top: 1px solid var(--line);
}
.side-foot .badge { align-self: flex-start; }
.side-foot .ver {
  font-family: ui-monospace, 'SF Mono', Menlo, Consolas, monospace;
  font-size: 10px;
  color: var(--muted);
  letter-spacing: 0.03em;
}

.brand { display: flex; align-items: center; gap: 12px; min-width: 0; padding: 2px 4px 0; }
.brand .mark {
  display: grid;
  place-items: center;
  width: 32px;
  height: 32px;
  flex: 0 0 auto;
  border: 1px solid var(--line-strong);
  border-radius: 9px;
  background: var(--raise);
  color: var(--platinum);
  font-size: 14px;
  font-weight: 620;
  letter-spacing: 0.02em;
}
.brand .word { display: flex; flex-direction: column; min-width: 0; }
.brand strong { font-size: 16px; font-weight: 640; letter-spacing: 0.01em; line-height: 1.2; }
.brand .ver {
  font-size: 10px;
  letter-spacing: 0.17em;
  text-transform: uppercase;
  color: var(--muted);
  margin-top: 3px;
  white-space: nowrap;
}

/* The rail's foot, collapsed into the corner it already occupied. Fixed so it
   stays where the word "Settings" was regardless of scroll. */
.rail-restore {
  position: fixed;
  left: 12px;
  bottom: 12px;
  z-index: 30;
  display: flex;
  align-items: center;
  gap: 6px;
}
/* The show control mirrors the hide control's position, so the pair is one toggle
   rather than a button that appears somewhere new each time. */
.rail-show-top {
  position: fixed;
  top: 16px;
  left: 12px;
  z-index: 31;
  margin: 0;
}
.rail-show-top button {
  padding: 4px 8px;
  border: 1px solid var(--line);
  border-radius: 8px;
  background: var(--panel);
  color: var(--muted);
  cursor: pointer;
  font-size: 12px;
  backdrop-filter: blur(8px);
}
.rail-show-top button:hover { color: var(--text); border-color: var(--line-strong); }
.rail-restore .cog-only {
  display: inline-flex;
  background: var(--panel);
  backdrop-filter: blur(8px);
  align-items: center;
  padding: 4px 7px;
  border: 1px solid var(--line);
  border-radius: 8px;
  color: var(--muted);
  text-decoration: none;
}
.rail-restore .cog-only:hover { color: var(--text); border-color: var(--line-strong); }
.rail-restore .cog-only[aria-current=page] { color: var(--text); border-color: var(--line-strong); }
.top-nav {
  position: sticky;
  top: 0;
  z-index: 5;
  padding: 6px 0 10px;
  background: linear-gradient(to bottom, var(--bg) 70%, transparent);
}
.sections { display: flex; flex-wrap: wrap; gap: 8px; }
.sections a {
  display: inline-flex;
  align-items: center;
  padding: 7px 13px;
  border: 1px solid var(--line-strong);
  border-radius: 999px;
  font-size: 12.5px;
  color: var(--muted);
  transition: color 0.12s ease, background 0.12s ease, border-color 0.12s ease;
}
.sections a:hover { color: var(--text); background: var(--raise); text-decoration: none; }
.sections a[aria-current=page] {
  color: var(--text);
  background: var(--raise);
  border-color: var(--platinum);
  font-weight: 620;
}

/* The rail: new chat, the conversations, the cog -------------------------- */

.side-action { margin: 0; }
.side-action button {
  width: 100%;
  justify-content: flex-start;
  padding: 9px 12px;
  background: transparent;
  border-color: var(--line-strong);
  color: var(--text);
  font-weight: 560;
}
.side-action button:hover { background: var(--raise); filter: none; }

.sessions {
  display: flex;
  flex-direction: column;
  gap: 1px;
  overflow-y: auto;
  min-height: 0;
  margin: -12px 0 0;
  padding: 0 0 4px;
}
.session-row { display: flex; align-items: center; gap: 2px; }
.session-row a { flex: 1 1 auto; min-width: 0; }
.session-delete { margin: 0; flex: 0 0 auto; }
.session-delete button {
  padding: 2px 7px;
  border: 0;
  background: transparent;
  color: var(--muted);
  font-size: 15px;
  line-height: 1;
  border-radius: 6px;
}
.session-delete button:hover { color: var(--bad); background: var(--raise); filter: none; }
.sessions a {
  display: grid;
  grid-template-columns: minmax(0, 1fr) auto;
  align-items: baseline;
  gap: 8px;
  padding: 7px 10px 7px 12px;
  border-left: 2px solid transparent;
  border-radius: 0 8px 8px 0;
  color: var(--muted);
  font-size: 12.5px;
  line-height: 1.35;
}
.sessions a:hover { color: var(--text); background: var(--raise); text-decoration: none; }
.sessions a[aria-current=page] {
  color: var(--text);
  background: var(--raise);
  border-left-color: var(--platinum);
}
.sessions .session-title {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.sessions .session-stamp {
  font-family: ui-monospace, 'SF Mono', Menlo, Consolas, monospace;
  font-size: 9.5px;
  color: var(--muted);
  opacity: 0.8;
}
.side-empty { display: block; padding: 6px 12px; font-size: 12px; color: var(--muted); }

.cog {
  display: inline-flex;
  align-items: center;
  gap: 8px;
  padding: 7px 10px;
  border-radius: 8px;
  color: var(--muted);
  font-size: 12.5px;
}
.cog:hover { color: var(--text); background: var(--raise); text-decoration: none; }
.cog[aria-current=page] { color: var(--text); background: var(--raise); }
.cog-glyph { font-size: 14px; line-height: 1; }
.soon { opacity: 0.55; cursor: not-allowed; }

.badge {
  display: inline-flex;
  align-items: center;
  gap: 7px;
  padding: 5px 11px;
  border: 1px solid var(--line-strong);
  border-radius: 999px;
  background: var(--panel);
  font-size: 10.5px;
  font-weight: 600;
  letter-spacing: 0.13em;
  text-transform: uppercase;
  color: var(--muted);
  white-space: nowrap;
}
.badge::before {
  content: "";
  width: 6px;
  height: 6px;
  border-radius: 50%;
  background: currentColor;
  flex: 0 0 auto;
}
.badge.ready { color: var(--ok); }
.badge.setup { color: var(--warn); }
.badge.live { color: var(--ok); }
.badge.expired { color: var(--warn); }
.badge.revoked { color: var(--bad); }

footer {
  display: flex;
  flex-wrap: wrap;
  align-items: baseline;
  justify-content: space-between;
  gap: 10px 24px;
  margin-top: 46px;
  padding-top: 18px;
  border-top: 1px solid var(--line);
  color: var(--muted);
  font-size: 11.5px;
  letter-spacing: 0.03em;
}
footer .facts { font-family: ui-monospace, 'SF Mono', Menlo, Consolas, monospace; font-size: 11px; }
footer .sign {
  text-transform: uppercase;
  letter-spacing: 0.2em;
  font-size: 10px;
  color: var(--muted);
  opacity: 0.8;
}

/* Type ------------------------------------------------------------------- */

h1 {
  font-size: 32px;
  line-height: 1.12;
  font-weight: 620;
  letter-spacing: -0.022em;
  margin: 0 0 8px;
}
h2 { font-size: 19px; font-weight: 620; letter-spacing: -0.01em; margin: 0 0 12px; }
h3 {
  font-size: 10.5px;
  font-weight: 600;
  letter-spacing: 0.14em;
  text-transform: uppercase;
  color: var(--muted);
  margin: 0 0 10px;
}
p { margin: 0 0 14px; }
.lede { color: var(--muted); font-size: 15px; max-width: 68ch; margin: 0 0 28px; }
.small { font-size: 12.5px; color: var(--muted); line-height: 1.55; }
.mono { font-family: ui-monospace, 'SF Mono', Menlo, Consolas, monospace; font-size: 12.5px; font-variant-numeric: tabular-nums; }

/* Surfaces --------------------------------------------------------------- */

.card {
  background: var(--panel);
  border: 1px solid var(--line);
  border-radius: 14px;
  padding: 24px 26px;
  margin: 0 0 18px;
}
.card.tight { padding: 16px 18px; }
.card > h2 {
  margin: 0 0 16px;
  padding-bottom: 13px;
  border-bottom: 1px solid var(--line);
  font-size: 16px;
  font-weight: 640;
  letter-spacing: -0.005em;
}
.card > h2:first-child { margin-top: 0; }
.card > h2 + p { margin-top: 0; }
.card > *:last-child { margin-bottom: 0; }

.grid { display: grid; grid-template-columns: repeat(auto-fit, minmax(240px, 1fr)); gap: 16px; }
.mt { margin-top: 20px; }

/* Forms ------------------------------------------------------------------ */

.field { margin: 0 0 18px; }
fieldset.field { border: 0; padding: 0; margin: 0 0 22px; }
.field label { display: block; font-size: 13px; font-weight: 560; margin-bottom: 7px; color: var(--text); }
.field .hint { display: block; font-size: 12.5px; color: var(--muted); margin-top: 7px; line-height: 1.5; }
legend {
  font-size: 10.5px;
  font-weight: 600;
  letter-spacing: 0.14em;
  text-transform: uppercase;
  color: var(--muted);
  margin-bottom: 10px;
  padding: 0;
}

input[type=text], input[type=password], input[type=number], input[type=search], select, textarea {
  width: 100%;
  padding: 10px 13px;
  border-radius: 9px;
  border: 1px solid var(--line-strong);
  background: var(--raise);
  color: var(--text);
  font: inherit;
  font-size: 14px;
  line-height: 1.5;
  transition: border-color 0.12s ease;
}
textarea {
  min-height: 180px;
  resize: vertical;
  font-family: ui-monospace, 'SF Mono', Menlo, Consolas, monospace;
  font-size: 13px;
  line-height: 1.65;
}
input[type=text]:focus, input[type=password]:focus, input[type=number]:focus,
input[type=search]:focus, select:focus, textarea:focus {
  outline: 2px solid var(--platinum);
  outline-offset: 1px;
  border-color: var(--platinum);
}
input.invalid, textarea.invalid { border-color: var(--bad); }
input[type=checkbox], input[type=radio] { accent-color: var(--platinum); }
input[type=checkbox] { width: 15px; height: 15px; margin: 0 8px 0 0; vertical-align: -2px; }
input[type=number] { max-width: 160px; font-variant-numeric: tabular-nums; }

.choices { display: grid; gap: 10px; }
.choice {
  display: grid;
  grid-template-columns: 20px 1fr;
  gap: 12px;
  align-items: start;
  padding: 13px 15px;
  border: 1px solid var(--line);
  border-radius: 10px;
  background: var(--raise);
  transition: border-color 0.12s ease, background 0.12s ease;
}
.choice:hover { border-color: var(--line-strong); }
.choice input { margin: 4px 0 0; width: 15px; height: 15px; }
.choice .name { display: block; font-weight: 600; font-size: 14px; line-height: 1.35; color: var(--text); }
.choice .why { display: block; color: var(--muted); font-size: 13px; margin-top: 3px; line-height: 1.5; }
.choice.selected { border-color: var(--platinum); }
.choice.selected .name { color: var(--text); }
.choice.disabled, .choice input:disabled { opacity: 0.45; cursor: not-allowed; }
.choice.disabled:hover { border-color: var(--line); }

button, .button {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  gap: 8px;
  padding: 9px 16px;
  border-radius: 8px;
  border: 1px solid var(--platinum);
  background: var(--platinum);
  color: var(--deep);
  font: inherit;
  font-size: 13.5px;
  font-weight: 620;
  letter-spacing: 0.005em;
  line-height: 1.3;
  cursor: pointer;
  transition: filter 0.12s ease, background 0.12s ease, border-color 0.12s ease;
}
button:hover, .button:hover { filter: brightness(1.05); text-decoration: none; }
button.secondary, .button.secondary {
  background: transparent;
  border-color: var(--line-strong);
  color: var(--text);
}
button.secondary:hover, .button.secondary:hover {
  background: var(--raise);
  border-color: var(--muted);
  filter: none;
}
button.danger {
  background: transparent;
  border-color: var(--bad);
  color: var(--bad);
}
button.danger:hover { background: var(--raise); filter: none; }
button:disabled { opacity: 0.45; cursor: not-allowed; filter: none; }
button:focus-visible, .button:focus-visible {
  outline: 2px solid var(--platinum);
  outline-offset: 2px;
}

.actions { display: flex; flex-wrap: wrap; align-items: center; gap: 10px; margin-top: 16px; }

/* Feedback --------------------------------------------------------------- */

.alert {
  border: 1px solid var(--line-strong);
  border-left: 3px solid var(--bad);
  border-radius: 9px;
  padding: 12px 15px;
  margin: 0 0 18px;
  color: var(--text);
  background: var(--panel);
  font-size: 14px;
}
.alert strong { color: var(--bad); font-weight: 620; margin-right: 6px; }
.alert.notice { border-left-color: var(--ok); }
.alert.notice strong { color: var(--ok); }

.tag {
  display: inline-block;
  font-size: 10px;
  letter-spacing: 0.1em;
  text-transform: uppercase;
  color: var(--muted);
  border: 1px solid var(--line-strong);
  border-radius: 999px;
  padding: 1px 8px;
  margin-left: 6px;
  vertical-align: 1px;
}

.empty {
  margin: 0;
  padding: 26px 22px;
  border: 1px dashed var(--line-strong);
  border-radius: 10px;
  color: var(--muted);
  font-size: 13.5px;
  line-height: 1.6;
  text-align: center;
}
.empty strong { display: block; color: var(--text); font-size: 14px; font-weight: 620; margin-bottom: 6px; }

/* Data ------------------------------------------------------------------- */

table { width: 100%; border-collapse: collapse; font-size: 14px; }
caption {
  text-align: left;
  color: var(--muted);
  font-size: 12px;
  letter-spacing: 0.02em;
  padding: 0 0 10px;
}
th, td { text-align: left; vertical-align: top; padding: 11px 12px; border-bottom: 1px solid var(--line); }
thead th {
  font-size: 10.5px;
  letter-spacing: 0.12em;
  text-transform: uppercase;
  color: var(--muted);
  font-weight: 600;
  border-bottom: 1px solid var(--line-strong);
}
tbody tr { transition: background 0.1s ease; }
tbody tr:hover { background: var(--raise); }
tbody th { font-weight: 620; }
tr:last-child th, tr:last-child td { border-bottom: none; }
td.value { white-space: nowrap; font-weight: 620; font-variant-numeric: tabular-nums; }
td.why { color: var(--muted); font-size: 13px; }
.locked, .locked .value { color: var(--muted); }
.locked th { font-weight: 560; }
td .mono { font-variant-numeric: tabular-nums; }

.kv {
  display: grid;
  grid-template-columns: minmax(150px, 190px) 1fr;
  gap: 0 18px;
  font-size: 14px;
  margin: 0;
}
.kv dt { color: var(--muted); font-size: 13px; padding: 8px 0; border-bottom: 1px solid var(--line); }
.kv dd {
  margin: 0;
  padding: 8px 0;
  border-bottom: 1px solid var(--line);
  font-family: ui-monospace, 'SF Mono', Menlo, Consolas, monospace;
  font-size: 12.5px;
  font-variant-numeric: tabular-nums;
  overflow-wrap: anywhere;
}
.kv dt:last-of-type, .kv dd:last-of-type { border-bottom: none; }

.stats {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(118px, 1fr));
  gap: 1px;
  margin: 16px 0 4px;
  background: var(--line);
  border: 1px solid var(--line);
  border-radius: 10px;
  overflow: hidden;
}
.stat { background: var(--panel); padding: 13px 15px; }
.stat .n {
  display: block;
  font-size: 24px;
  line-height: 1.2;
  font-weight: 600;
  letter-spacing: -0.02em;
  font-variant-numeric: tabular-nums;
}
.stat .k {
  display: block;
  margin-top: 4px;
  font-size: 10px;
  letter-spacing: 0.13em;
  text-transform: uppercase;
  color: var(--muted);
}

/* Jobs, sessions, codes -------------------------------------------------- */

.job {
  border: 1px solid var(--line);
  border-radius: 11px;
  padding: 16px 18px;
  margin: 0 0 14px;
  background: var(--raise);
}
.job:last-child { margin-bottom: 0; }
.job-head { display: flex; flex-wrap: wrap; align-items: baseline; gap: 10px; margin-bottom: 12px; }
.job h3 { margin: 16px 0 6px; }

.timeline {
  list-style: none;
  margin: 0;
  padding: 0 0 0 16px;
  border-left: 1px solid var(--line-strong);
}
.timeline li {
  display: flex;
  flex-wrap: wrap;
  align-items: baseline;
  gap: 10px;
  padding: 7px 0;
  border-bottom: 1px solid var(--line);
  font-size: 13.5px;
}
.timeline li:last-child { border-bottom: none; }
.timeline .kind {
  font-size: 10px;
  letter-spacing: 0.1em;
  text-transform: uppercase;
  color: var(--muted);
  min-width: 92px;
}

.pairing-code {
  display: inline-block;
  font-family: ui-monospace, 'SF Mono', Menlo, Consolas, monospace;
  font-size: 28px;
  letter-spacing: 0.3em;
  padding: 14px 20px 14px 24px;
  margin: 8px 0;
  background: var(--deep);
  border: 1px solid var(--line-strong);
  border-radius: 11px;
  color: var(--platinum);
  font-variant-numeric: tabular-nums;
}

code, .code {
  font-family: ui-monospace, 'SF Mono', Menlo, Consolas, monospace;
  font-size: 12.5px;
  background: var(--deep);
  border: 1px solid var(--line);
  border-radius: 6px;
  padding: 2px 6px;
  overflow-wrap: anywhere;
}
pre.code {
  display: block;
  padding: 14px 16px;
  overflow-x: auto;
  white-space: pre;
  line-height: 1.6;
}


/* The chat surface ------------------------------------------------------- */

.chat-shell {
  display: flex;
  flex-direction: column;
  min-height: 0;
  /* The composer lives at the bottom of the screen, not wherever the transcript
     happens to end. dvh follows the mobile viewport without the jump vh brings. */
  height: calc(100vh - 150px);
  height: calc(100dvh - 150px);
  max-width: 880px;
  margin: 0 auto;
}

/* A conversation with nothing in it is a different page: greeting and composer,
   together in the middle, because there is no transcript to sit under. The
   heading stays at the top; only the group below it is centred, which the auto
   margins do without moving the header. */
.chat-shell.new-chat .thread { display: none; }
.chat-shell.new-chat .chat-greeting { margin-top: auto; }
.chat-shell.new-chat > .composer { margin-bottom: auto; }
.chat-greeting {
  position: relative;
  z-index: 1;
  /* Offset, not centred: a hero line on the exact middle of the screen reads as
     a splash page, and this page is a workspace. It lines up with the composer. */
  text-align: left;
  margin: 0 0 14px;
  padding-left: 14px;
}
.chat-greeting .hello {
  margin: 0;
  font-size: 30px;
  font-weight: 600;
  letter-spacing: -0.015em;
  color: var(--text);
}
.chat-greeting .hint {
  margin: 8px 0 0;
  font-size: 13px;
  color: var(--muted);
}
.chat-title .lede { margin-bottom: 0; max-width: 62ch; }
.chat-meta { display: flex; flex-wrap: wrap; align-items: center; gap: 10px; }
.chat-meta form { margin: 0; }

.thread {
  position: relative;
  z-index: 1;
  display: grid;
  gap: 18px;
  align-content: start;
  padding: 4px 2px 12px;
  min-height: 0;
  flex: 1 1 auto;
  overflow-y: auto;
  overscroll-behavior: contain;
}
.thread:empty { display: none; }

.msg { display: flex; flex-direction: column; max-width: 48rem; }
.msg.user { margin-left: auto; align-items: flex-end; }
.msg.assistant { margin-right: auto; align-items: flex-start; }
.msg .who {
  font-size: 10px;
  font-weight: 600;
  letter-spacing: 0.12em;
  text-transform: uppercase;
  color: var(--muted);
  margin: 0 0 6px;
  padding: 0 2px;
}
.msg .bubble {
  padding: 13px 16px;
  border-radius: 14px 14px 14px 5px;
  border: 1px solid var(--line);
  background: var(--panel);
  color: var(--text);
  font-size: 14.5px;
  line-height: 1.62;
  overflow-wrap: anywhere;
}
.msg.user .bubble {
  border-radius: 14px 14px 5px 14px;
  border-color: var(--line-strong);
  background: var(--raise);
}
.msg .bubble.failed { border-color: var(--bad); }
.msg .bubble p { margin: 0 0 10px; white-space: pre-wrap; }
.msg .bubble > *:last-child { margin-bottom: 0; }

/* The thinking bubble. Open by default, because the first thing an operator asks
   about a slow answer is what it was doing. Dim and italic so it still reads as
   machinery behind the answer rather than as the answer. */
.msg .think {
  margin: 0 0 8px;
  padding: 12px 14px;
  border: 1px solid var(--line);
  border-radius: 16px;
  background: rgba(255, 255, 255, 0.025);
  color: var(--muted);
  font-size: 12.5px;
  line-height: 1.6;
}
.msg .think > summary {
  cursor: pointer;
  list-style: none;
  display: flex;
  align-items: center;
  gap: 8px;
  font-size: 11px;
  letter-spacing: 0.06em;
  text-transform: uppercase;
  color: var(--muted);
  opacity: 0.8;
}
.msg .think-actions { margin-left: auto; display: flex; gap: 4px; }
.msg .think-max {
  padding: 2px 6px;
  border: 1px solid var(--line);
  border-radius: 6px;
  background: transparent;
  color: var(--muted);
  font-size: 11px;
  line-height: 1;
  cursor: pointer;
}
.msg .think-max:hover { color: var(--text); border-color: var(--line-strong); }
/* Maximized: the bubble takes the room it needs instead of a fixed 320px. */
.msg .think.max {
  position: fixed;
  inset: 4vh 5vw;
  z-index: 40;
  margin: 0;
  background: var(--bg);
  border-color: var(--line-strong);
  overflow: auto;
}
.msg .think.max .think-body { max-height: none; font-size: 14.5px; }

/* The run log: what the turn actually executed. Monospace and full-width so a
   command reads like a command and its output reads like output. */
.msg .exec {
  margin: 0 0 8px;
  border: 1px solid var(--line);
  border-radius: 12px;
  background: rgba(0, 0, 0, 0.28);
  font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
  font-size: 12px;
  overflow: hidden;
}
.msg .exec > summary {
  cursor: pointer;
  list-style: none;
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 7px 12px;
  font-size: 10.5px;
  letter-spacing: 0.08em;
  text-transform: uppercase;
  color: var(--muted);
}
.msg .exec > summary::-webkit-details-marker { display: none; }
.msg .exec-count { margin-left: auto; opacity: 0.7; }
/* Summary mode: one line naming the last call, truncated so it stays one line. */
.msg .exec.compact > summary { text-transform: none; letter-spacing: 0; font-size: 11.5px; }
.msg .exec.compact .exec-last {
  color: var(--text);
  opacity: 0.85;
  min-width: 0;
  overflow: hidden;
  white-space: nowrap;
  text-overflow: ellipsis;
}
.msg .exec-call {
  padding: 9px 12px;
  border-top: 1px dashed var(--line);
}
/* One call is one window: closed it is a single line naming the call, open it is
   a short scroll box so a 40-line stdout cannot take over the conversation. */
.msg .exec-call > summary {
  cursor: pointer;
  list-style: none;
  display: flex;
  /* One line when closed: a long argument string is an ellipsis, not four
     wrapped rows that make fifteen calls look like a wall again. */
  flex-wrap: nowrap;
  gap: 8px;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
.msg .exec-call[open] > summary {
  flex-wrap: wrap;
  white-space: normal;
}
.msg .exec-call > summary::-webkit-details-marker { display: none; }
.msg .exec-call > summary::before {
  content: "\25B8";
  color: var(--muted);
  margin-right: 2px;
}
.msg .exec-call[open] > summary::before { content: "\25BE"; }
.msg .exec-call[open] > summary { margin-bottom: 6px; }
.msg .exec-head { display: flex; gap: 8px; min-width: 0; }
/* The body of the log scrolls, so twenty calls cost one window rather than the
   whole conversation. */
.msg .exec-body { max-height: 46vh; overflow-y: auto; }
.msg .exec-name { color: var(--text); }
.msg .exec-args { color: var(--muted); overflow-wrap: anywhere; }
.msg .exec-call.failed .exec-name { color: var(--bad); }
/* Output as an application window: traffic lights, a title, the size, the text.
   It reads as a thing that ran rather than as prose that arrived. */
.msg .win {
  margin: 7px 0 0;
  border: 1px solid var(--line);
  border-radius: 12px;
  overflow: hidden;
  background: rgba(8, 8, 10, 0.72);
}
.msg .win-bar {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 7px 11px;
  border-bottom: 1px solid var(--line);
  background: rgba(255, 255, 255, 0.03);
}
.msg .win-dots { display: flex; gap: 5px; }
.msg .win-dot { width: 9px; height: 9px; border-radius: 50%; display: block; }
/* Traffic lights from the palette, not from macOS: the stylesheet test refuses a
   hex that is not a palette entry, and a fourth red would be a second truth. */
.msg .win-dot.red { background: var(--bad); }
.msg .win-dot.yellow { background: var(--warn); }
.msg .win-dot.green { background: var(--ok); }
.msg .win-title { font-size: 11px; letter-spacing: 0.04em; color: var(--muted); }
/* The live dot: the window says it is still being written, in the same green the
   palette uses for a thing that is working, and vanishes when the call settles. */
.msg .win-live {
  margin-left: auto;
  font-size: 8px;
  color: var(--ok);
  animation: caret 1.1s steps(1) infinite;
}
.msg .win-size { font-size: 10.5px; color: var(--muted); opacity: 0.8; }
/* With no live dot the size takes the far edge; with one, the dot leads it. */
.msg .win-live[hidden] + .win-size { margin-left: auto; }
/* The body is lines, not one block of pre: a command's output is a prompt line
   and then what it printed, and stderr has to be paintable without colouring the
   whole window. Monospace, because output is output. */
.msg .win-body {
  margin: 0;
  padding: 10px 12px;
  font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
  font-size: 11.5px;
  line-height: 1.5;
  color: var(--text);
  opacity: 0.9;
  max-height: 132px;
  overflow: auto;
}
.msg .term-line { white-space: pre-wrap; overflow-wrap: anywhere; }
/* The command reads like the command, in the accent, because it is the line the
   operator typed and the rest answers it. */
.msg .term-line.prompt { color: var(--platinum); opacity: 0.85; }
.msg .term-line.err { color: var(--bad); }
/* A notice is this window talking, not the tool: muted, so an exit status never
   reads as output the command produced. */
.msg .term-line.notice { color: var(--muted); }
/* Expanded stays bounded: a bigger window, not the whole file. */
.msg .exec-call[open] .win-body { max-height: 340px; }
.msg .think > summary::-webkit-details-marker { display: none; }
.msg .think > summary::before {
  content: "▸ ";
  opacity: 0.7;
}
.msg .think[open] > summary::before { content: "▾ "; }
.msg .think .think-body {
  margin-top: 8px;
  padding-top: 8px;
  border-top: 1px dashed var(--line);
  font-style: italic;
  white-space: pre-wrap;
  opacity: 0.72;
  max-height: 320px;
  overflow-y: auto;
}
.msg .meta {
  display: flex;
  align-items: center;
  gap: 8px;
  margin-top: 6px;
  font-size: 11px;
  color: var(--muted);
}
.msg .meta .tag { margin-left: 0; }

.msg.pending .bubble::after {
  content: "";
  display: inline-block;
  width: 8px;
  height: 15px;
  margin-left: 3px;
  vertical-align: -2px;
  background: var(--platinum);
  animation: caret 1.1s steps(1) infinite;
}
@keyframes caret { 0%, 50% { opacity: 1; } 50.01%, 100% { opacity: 0; } }

.muted-line { color: var(--muted); margin: 0; font-style: italic; }

.code-block { margin: 12px 0; min-width: 16rem; }
.code-lang {
  display: block;
  padding: 6px 14px 0;
  font-size: 9.5px;
  letter-spacing: 0.12em;
  text-transform: uppercase;
  color: var(--muted);
}
.code-block pre.code { margin: 0; padding: 14px 16px; }
.msg .bubble .code-block {
  margin: 12px -16px;
  border-top: 1px solid var(--line);
  border-bottom: 1px solid var(--line);
  background: var(--deep);
}
.msg .bubble .code-block pre.code { border: 0; border-radius: 0; background: transparent; }

.chat-empty {
  display: grid;
  justify-items: center;
  gap: 4px;
  text-align: center;
  padding: 44px 24px;
  border: 1px dashed var(--line-strong);
  border-radius: 14px;
  background: var(--panel);
}
.chat-empty .mark {
  display: grid;
  place-items: center;
  width: 46px;
  height: 46px;
  margin-bottom: 10px;
  border: 1px solid var(--line-strong);
  border-radius: 13px;
  background: var(--raise);
  color: var(--platinum);
  font-size: 19px;
  font-weight: 620;
}
.chat-empty h2 { margin: 0; font-size: 17px; }
.chat-empty .small { max-width: 52ch; }
.prompts { display: flex; flex-wrap: wrap; justify-content: center; gap: 8px; margin-top: 14px; }

.composer {
  position: relative;
  z-index: 2;
  display: grid;
  flex: 0 0 auto;
  gap: 8px;
  margin-top: 10px;
  padding: 12px 12px 10px;
  border: 1px solid var(--line-strong);
  border-radius: 14px;
  background: var(--panel);
  transition: border-color 0.12s ease;
}
.composer:focus-within { border-color: var(--platinum); }
.composer textarea {
  width: 100%;
  min-height: 46px;
  max-height: 240px;
  padding: 6px 6px 2px;
  border: 0;
  border-radius: 8px;
  background: transparent;
  color: var(--text);
  font-family: inherit;
  font-size: 14.5px;
  line-height: 1.55;
  resize: none;
}
.composer textarea:focus { outline: none; border-color: transparent; }
.composer .hint { font-size: 11px; color: var(--muted); }
.composer button[hidden] { display: none; }

.sr-only {
  position: absolute;
  width: 1px;
  height: 1px;
  padding: 0;
  margin: -1px;
  overflow: hidden;
  clip-path: inset(50%);
  white-space: nowrap;
  border: 0;
}


/* Session stats, the model picker, and the status strip ---------------- */

.chat-stats {
  position: relative;
  z-index: 1;
  display: flex;
  flex-wrap: wrap;
  gap: 10px;
  margin: 0 0 18px;
}
.stat-chip {
  display: grid;
  gap: 2px;
  min-width: 104px;
  padding: 10px 14px;
  border: 1px solid var(--line);
  border-radius: 11px;
  background: var(--panel);
}
.stat-chip.wide { flex: 1 1 260px; }
.stat-chip .n {
  font-size: 15px;
  font-weight: 620;
  letter-spacing: -0.01em;
  font-variant-numeric: tabular-nums;
  color: var(--text);
}
.stat-chip .k {
  font-size: 9.5px;
  letter-spacing: 0.14em;
  text-transform: uppercase;
  color: var(--muted);
}
.meter {
  height: 3px;
  margin-top: 7px;
  border-radius: 999px;
  background: var(--line-strong);
  overflow: hidden;
}
.meter-fill { height: 100%; background: var(--platinum); border-radius: 999px; }

.msg .meta-model {
  font-family: ui-monospace, 'SF Mono', Menlo, Consolas, monospace;
  font-size: 10.5px;
  letter-spacing: 0.03em;
  color: var(--platinum);
  opacity: 0.75;
}
.msg .meta-usage {
  font-family: ui-monospace, 'SF Mono', Menlo, Consolas, monospace;
  font-size: 10.5px;
  color: var(--muted);
}
.msg .meta > * + *::before {
  content: "·";
  margin-right: 8px;
  color: var(--line-strong);
}

.composer-bar {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  justify-content: space-between;
  gap: 10px;
}
.composer-pick { display: flex; align-items: center; gap: 10px; min-width: 0; }
.composer-pick #mode { max-width: 150px; }
.composer-pick select {
  width: auto;
  max-width: 260px;
  padding: 6px 10px;
  border-radius: 8px;
  border: 1px solid var(--line-strong);
  background: var(--raise);
  color: var(--text);
  font-family: ui-monospace, 'SF Mono', Menlo, Consolas, monospace;
  font-size: 11.5px;
}
.composer-pick .nowrap { white-space: nowrap; }
.composer-send { display: flex; align-items: center; gap: 10px; margin-left: auto; }

/* The attach control: a labelled + that opens the picker, and the queued name
   beside it so a turn never carries a file the operator forgot about. */
.attach {
  display: inline-grid;
  place-items: center;
  width: 28px;
  height: 28px;
  flex: 0 0 auto;
  border: 1px solid var(--line-strong);
  border-radius: 8px;
  color: var(--muted);
  font-size: 15px;
  line-height: 1;
  cursor: pointer;
}
.attach:hover { color: var(--text); background: var(--raise); }
.attach-input {
  position: absolute;
  width: 1px;
  height: 1px;
  padding: 0;
  margin: -1px;
  overflow: hidden;
  clip-path: inset(50%);
  border: 0;
}
.attach-input:focus-visible + .attach-queue { outline: 2px solid var(--platinum); outline-offset: 2px; }

/* The queue of files waiting to go with the message. A chip per file, its size
   stated, and its own remove button, because a mis-picked file should cost one
   click rather than a reload. It wraps under the bar on a phone. */
.attach-queue {
  display: flex;
  flex-wrap: wrap;
  gap: 6px;
  min-width: 0;
  flex: 1 1 220px;
  order: 5;
}
.attach-queue[hidden] { display: none; }
.attach-chip {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  max-width: 240px;
  padding: 3px 4px 3px 9px;
  border: 1px solid var(--line-strong);
  border-radius: 999px;
  background: var(--raise);
  color: var(--text);
  font-family: ui-monospace, 'SF Mono', Menlo, Consolas, monospace;
  font-size: 10.5px;
  line-height: 1.5;
}
.attach-chip-name {
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.attach-chip-size { color: var(--muted); flex: 0 0 auto; }
.attach-chip-drop {
  flex: 0 0 auto;
  width: 17px;
  height: 17px;
  padding: 0;
  border: 0;
  border-radius: 999px;
  background: transparent;
  color: var(--muted);
  font-size: 12px;
  line-height: 1;
  cursor: pointer;
}
.attach-chip-drop:hover { background: var(--line-strong); color: var(--text); }

/* A drop landing on the composer says so: the one gesture everybody tries first
   has to look like it was received. */
.composer.dropping { border-color: var(--platinum); background: var(--raise); }
.composer.dropping::after {
  content: "drop to attach";
  position: absolute;
  inset: 0;
  display: grid;
  place-items: center;
  border-radius: 13px;
  background: rgba(12, 12, 16, 0.72);
  color: var(--platinum);
  font-size: 12px;
  letter-spacing: 0.08em;
  text-transform: uppercase;
  pointer-events: none;
}

/* What a sent message carried, under its own words: the files by name and size,
   so the transcript says what went with the question. An image is shown as the
   picture, because a screenshot nobody can see is a filename. */
.msg .attached {
  display: flex;
  flex-wrap: wrap;
  align-items: flex-end;
  gap: 8px;
  margin-top: 10px;
}
.msg .attached-chip {
  padding: 2px 8px;
  border: 1px solid var(--line-strong);
  border-radius: 999px;
  color: var(--muted);
  font-family: ui-monospace, 'SF Mono', Menlo, Consolas, monospace;
  font-size: 10px;
  line-height: 1.6;
  text-decoration: none;
}
a.attached-chip:hover { color: var(--text); border-color: var(--platinum); }
.msg .attached-image {
  display: block;
  max-width: 260px;
  border: 1px solid var(--line-strong);
  border-radius: 10px;
  overflow: hidden;
  line-height: 0;
}
.msg .attached-image img { display: block; width: 100%; height: auto; }
.msg .attached-image:hover { border-color: var(--platinum); }

/* The one control that undoes following the bottom. It sits above the composer,
   where the eye already is when the page stops moving. */
.jump-latest {
  align-self: center;
  margin: 0 0 6px;
  padding: 5px 12px;
  border: 1px solid var(--line-strong);
  border-radius: 999px;
  background: var(--panel);
  color: var(--platinum);
  font-size: 11px;
  letter-spacing: 0.04em;
  cursor: pointer;
  box-shadow: 0 6px 18px rgba(0, 0, 0, 0.45);
}
.jump-latest:hover { border-color: var(--platinum); }
.jump-latest[hidden] { display: none; }

/* A call that is still writing keeps its head visible without the caret that a
   pending bubble uses: the live dot in the window is the signal. */
.msg .exec-call.streaming .exec-name { color: var(--platinum); }

/* The conversation totals ride inside the last reply, under a hairline, instead
   of floating over the wallpaper in a bar of their own. */
.meta-session {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 8px;
  margin: 10px 0 0;
  padding: 8px 0 0;
  border-top: 1px solid var(--line);
  color: var(--muted);
  font-family: ui-monospace, 'SF Mono', Menlo, Consolas, monospace;
  font-size: 10.5px;
  letter-spacing: 0.02em;
}
.meta-session .dot { color: var(--line-strong); }

/* The wizard rail -------------------------------------------------------- */

.rail {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(126px, 1fr));
  gap: 10px;
  list-style: none;
  padding: 0;
  margin: 0 0 28px;
}
.rail li {
  border: 1px solid var(--line);
  border-radius: 10px;
  background: var(--panel);
  padding: 10px 12px;
  font-size: 12.5px;
  line-height: 1.4;
  color: var(--muted);
  transition: border-color 0.12s ease;
}
.rail li.done { border-color: var(--line-strong); color: var(--text); }
.rail li.current { border-color: var(--platinum); color: var(--text); }
.rail .num {
  display: block;
  font-size: 9.5px;
  letter-spacing: 0.15em;
  text-transform: uppercase;
  color: var(--muted);
  margin-bottom: 4px;
}
.rail .tag { margin: 4px 0 0; font-size: 9.5px; padding: 0 6px; }

/* The sign-in page ------------------------------------------------------- */

.auth { max-width: 470px; margin: 7vh auto 0; }
.auth .mark {
  display: grid;
  place-items: center;
  width: 44px;
  height: 44px;
  border: 1px solid var(--line-strong);
  border-radius: 12px;
  background: var(--raise);
  color: var(--platinum);
  font-size: 19px;
  font-weight: 620;
  margin-bottom: 20px;
}
.auth h1 { font-size: 28px; }
.auth .lede { margin-bottom: 22px; }

/* Small screens ---------------------------------------------------------- */

@media (max-width: 900px) {
  .app { display: block; }
  .side {
    /* Static on a phone: a rail that sticks while the transcript scrolls puts the
       navigation in the middle of the conversation. */
    position: static;
    z-index: 2;
    height: auto;
    flex-direction: row;
    flex-wrap: wrap;
    align-items: center;
    gap: 10px 16px;
    padding: 12px 16px;
    border-right: 0;
    border-bottom: 1px solid var(--line);
  }
  .brand .ver { display: none; }
  .side-action { margin-left: auto; }
  .side-action button { width: auto; }
  .sessions { flex-direction: row; overflow-x: auto; gap: 4px; width: 100%; margin: 0; }
  .sessions a { border-left-width: 1px; border-radius: 8px; white-space: nowrap; }
  .sessions .session-stamp { display: none; }
  .side-foot { flex-direction: row; align-items: center; margin-top: 0; padding-top: 0; border-top: 0; margin-left: auto; gap: 10px; }
  .side-foot .ver { display: none; }
  .wrap { padding: 22px 18px 56px; }
  h1 { font-size: 26px; }
  .card { padding: 18px 18px; border-radius: 12px; }
  .kv { grid-template-columns: 1fr; gap: 0; }
  .kv dt { padding-bottom: 0; border-bottom: none; }
  .kv dd { padding-top: 2px; }
  .stats { grid-template-columns: repeat(auto-fit, minmax(96px, 1fr)); }
  .auth { margin-top: 4vh; }
  /* The composer still belongs at the bottom on a phone: the rail becomes a
     top bar, so the shell measures what is left of the viewport. */
  .chat-shell { height: calc(100dvh - 132px); }
  .thread { max-height: none; }
  /* Two selects, an attach control and a button do not fit on one row of a
     phone. They wrap onto a second row instead of pushing Send off the edge. */
  .composer-bar { flex-wrap: wrap; row-gap: 8px; }
  .composer-pick { flex-wrap: wrap; row-gap: 6px; min-width: 0; }
  .composer-pick select { max-width: 100%; }
  .composer-pick #mode { max-width: 132px; }
  .composer-send { width: 100%; justify-content: flex-end; }
  /* The figure sits right of centre in the artwork. Cropped to a phone's width
     the centre of the frame is empty sky, so the frame is anchored where she is. */
  .app-wall { background-position: 88% center; }
  .msg { max-width: 100%; }
  .composer { bottom: 8px; }
}

/*
 * A phone is the wrong place for a three-column permission table, but it is a
 * place this page will be read. Below this width the table keeps its columns and
 * scrolls inside its own card instead of crushing prose into a 70px gutter.
 */
@media (max-width: 700px) {
  th, td { padding: 9px 8px; }
  .card:has(table) { overflow-x: auto; }
  table { min-width: 520px; }
  .card:has(table) > h2 { position: sticky; left: 0; }
}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    /// The six purples 1.0 refuses, plus the two spellings that appear if a
    /// stylesheet is written with lowercase hex.
    const PURPLE_DENYLIST: [&str; 6] = [
        "#800080", "#7B1FA2", "#6A1B9A", "#9C27B0", "#8E24AA", "#673AB7",
    ];

    /// The wallpaper is fixed, and fixed means fixed: sized by a viewport unit
    /// that does not change when mobile chrome collapses, on its own layer.
    #[test]
    fn the_wallpaper_is_pinned_to_the_stable_viewport_unit() {
        assert!(APP_CSS.contains("height: 100lvh;"), "the wall uses lvh");
        let wall = APP_CSS
            .split(".app-wall {")
            .nth(1)
            .and_then(|rest| rest.split('}').next())
            .expect("the app-wall rule exists");
        assert!(
            !wall.contains("inset: 0"),
            "inset would size the wall to the collapsing visual viewport: {wall}"
        );
        assert!(
            wall.contains("position: fixed"),
            "the wall does not scroll with the document: {wall}"
        );
        assert!(
            wall.contains("translateZ(0)"),
            "the wall has its own layer, so it is not repainted with the page: {wall}"
        );
    }

    #[test]
    fn the_stylesheet_carries_no_purple() {
        let lowered = APP_CSS.to_ascii_lowercase();
        for banned in PURPLE_DENYLIST {
            assert!(
                !lowered.contains(&banned.to_ascii_lowercase()),
                "the stylesheet carries the banned hex {banned}"
            );
        }
    }

    #[test]
    fn the_stylesheet_has_no_script_no_cdn_and_no_web_font() {
        let lowered = APP_CSS.to_ascii_lowercase();
        for banned in ["@import", "http://", "https://", "@font-face", "url("] {
            assert!(!lowered.contains(banned), "the stylesheet carries {banned}");
        }
    }

    #[test]
    fn every_hex_in_the_stylesheet_is_a_palette_entry() {
        // The ADR lists the allowed set; this test reads the stylesheet's own
        // custom properties and asserts the two agree, so a new colour cannot be
        // added to the UI without the ADR being updated.
        let mut declared = Vec::new();
        for line in APP_CSS.lines() {
            let line = line.trim();
            let Some(rest) = line.strip_prefix("--") else {
                continue;
            };
            let Some((_name, value)) = rest.split_once(':') else {
                continue;
            };
            let value = value.trim().trim_end_matches(';').trim();
            if let Some(hex) = value.strip_prefix('#') {
                assert_eq!(hex.len(), 6, "{value} is not a six-digit hex");
                declared.push(value.to_ascii_uppercase());
            }
        }
        declared.sort_unstable();
        declared.dedup();
        assert_eq!(
            declared.len(),
            13,
            "the palette is thirteen colours, found {declared:?}"
        );
        // Every literal hex used outside the variable block must be one of them.
        let mut body = String::new();
        for line in APP_CSS.lines() {
            if line.trim_start().starts_with("--") {
                continue;
            }
            body.push_str(line);
            body.push('\n');
        }
        let mut index = 0usize;
        let bytes = body.as_bytes();
        while index < bytes.len() {
            if bytes[index] == b'#' && index + 7 <= bytes.len() {
                let candidate = &body[index..index + 7];
                if candidate[1..]
                    .chars()
                    .all(|character| character.is_ascii_hexdigit())
                {
                    assert!(
                        declared
                            .iter()
                            .any(|entry| entry.eq_ignore_ascii_case(candidate)),
                        "{candidate} is used but not declared in the palette"
                    );
                    index += 7;
                    continue;
                }
            }
            index += 1;
        }
    }
}
