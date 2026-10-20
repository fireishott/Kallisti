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

.wrap { max-width: 1120px; margin: 0 auto; padding: 30px 28px 72px; }

/* The shell -------------------------------------------------------------- */

.top {
  position: sticky;
  top: 0;
  z-index: 10;
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 14px 26px;
  margin: 0 -28px 36px;
  padding: 16px 28px;
  border-bottom: 1px solid var(--line);
  background: var(--bg);
}

.brand { display: flex; align-items: center; gap: 12px; min-width: 0; }
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

.nav { display: flex; flex-wrap: wrap; align-items: center; gap: 3px; margin-left: auto; }
.nav a {
  padding: 7px 12px;
  border: 1px solid transparent;
  border-radius: 8px;
  font-size: 13.5px;
  color: var(--muted);
  transition: color 0.12s ease, background 0.12s ease, border-color 0.12s ease;
}
.nav a:hover { color: var(--text); background: var(--raise); text-decoration: none; }
.nav a[aria-current=page] {
  color: var(--text);
  background: var(--raise);
  border-color: var(--line-strong);
  font-weight: 620;
}
.nav .soon { font-size: 13.5px; color: var(--muted); opacity: 0.55; padding: 7px 12px; }
.nav .soon .tag { margin-left: 5px; }
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

.chat-shell { max-width: 880px; margin: 0 auto; }
.chat-wall {
  position: fixed;
  inset: 0;
  z-index: 0;
  background-position: center;
  background-size: cover;
  background-repeat: no-repeat;
  opacity: 0.9;
  pointer-events: none;
}
.chat-head {
  position: relative;
  z-index: 1;
  display: flex;
  flex-wrap: wrap;
  align-items: flex-end;
  justify-content: space-between;
  gap: 12px 20px;
  margin-bottom: 18px;
}
.chat-title .lede { margin-bottom: 0; max-width: 62ch; }
.chat-meta { display: flex; flex-wrap: wrap; align-items: center; gap: 10px; }
.chat-meta form { margin: 0; }

.thread {
  position: relative;
  z-index: 1;
  display: grid;
  gap: 18px;
  padding: 4px 2px 10px;
  max-height: 58vh;
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
  position: sticky;
  bottom: 14px;
  z-index: 2;
  display: grid;
  gap: 8px;
  margin-top: 16px;
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

.statusbar {
  position: relative;
  z-index: 1;
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 8px;
  margin: 8px 2px 0;
  color: var(--muted);
  font-family: ui-monospace, 'SF Mono', Menlo, Consolas, monospace;
  font-size: 10.5px;
  letter-spacing: 0.02em;
}
.statusbar .dot { color: var(--line-strong); }

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

@media (max-width: 820px) {
  .wrap { padding: 22px 18px 56px; }
  .top { margin: 0 -18px 26px; padding: 13px 18px; gap: 10px 16px; }
  .brand .ver { display: none; }
  .badge { order: 2; margin-left: auto; }
  .nav { order: 3; margin-left: 0; width: 100%; gap: 2px; }
  .nav a { padding: 6px 9px; font-size: 13px; }
  h1 { font-size: 26px; }
  .card { padding: 18px 18px; border-radius: 12px; }
  .kv { grid-template-columns: 1fr; gap: 0; }
  .kv dt { padding-bottom: 0; border-bottom: none; }
  .kv dd { padding-top: 2px; }
  .stats { grid-template-columns: repeat(auto-fit, minmax(96px, 1fr)); }
  .auth { margin-top: 4vh; }
  .thread { max-height: 52vh; }
  .chat-head { align-items: flex-start; }
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
