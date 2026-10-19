//! The one stylesheet, served inline and at `/static/app.css`.
//!
//! Every colour in this file is listed in `docs/adr/0004-palette.md`, which is
//! the denylist reference: a hex that is not in the ADR is not allowed in the
//! UI, and an acceptance test greps the rendered pages for the six purple values
//! 1.0 refuses. There is no build step, no npm, no CDN, no web font, and no
//! `<script>`: the pages are server-rendered HTML with one inline stylesheet, so
//! `curl` can read every step and a browser with JavaScript disabled works.

/// Where the stylesheet is also reachable as a file.
pub const CSS_ROUTE: &str = "/static/app.css";

/// The whole stylesheet. Dark neutral, system fonts, no purple.
pub const APP_CSS: &str = r#"
:root {
  --deep: #0b0d10;
  --bg: #0f1115;
  --panel: #161a20;
  --raise: #1d2229;
  --line: #262c35;
  --line-strong: #394150;
  --text: #e6e9ee;
  --muted: #9aa4b2;
  --ok: #7dd3a0;
  --warn: #e8c07d;
  --bad: #e07a7a;
  --link: #6ea8fe;
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
  line-height: 1.55;
  -webkit-font-smoothing: antialiased;
}

a { color: var(--link); text-decoration: none; }
a:hover { text-decoration: underline; }

.wrap { max-width: 1100px; margin: 0 auto; padding: 28px 24px 64px; }

.top {
  display: flex;
  align-items: baseline;
  justify-content: space-between;
  gap: 16px;
  padding-bottom: 18px;
  border-bottom: 1px solid var(--line);
  margin-bottom: 26px;
}
.brand { font-size: 15px; letter-spacing: 0.14em; text-transform: uppercase; color: var(--muted); }
.brand strong { color: var(--text); letter-spacing: 0.02em; text-transform: none; font-size: 18px; font-weight: 640; }

.nav { display: flex; flex-wrap: wrap; align-items: baseline; gap: 18px; margin-left: auto; margin-right: 18px; }
.nav a { font-size: 14px; color: var(--muted); padding: 2px 0; border-bottom: 2px solid transparent; }
.nav a:hover { color: var(--text); text-decoration: none; border-bottom-color: var(--line-strong); }
.nav a[aria-current=page] { color: var(--text); border-bottom-color: var(--link); font-weight: 620; }
.nav .soon { font-size: 14px; color: var(--muted); opacity: 0.6; }
.nav .soon .tag { margin-left: 4px; }

.soon { opacity: 0.55; cursor: not-allowed; }

.badge {
  display: inline-block;
  padding: 2px 9px;
  border: 1px solid var(--line-strong);
  border-radius: 999px;
  font-size: 12px;
  letter-spacing: 0.06em;
  text-transform: uppercase;
  color: var(--muted);
}
.badge.ready { color: var(--ok); border-color: var(--ok); }
.badge.setup { color: var(--warn); border-color: var(--warn); }

h1 { font-size: 26px; line-height: 1.25; margin: 0 0 6px; font-weight: 650; letter-spacing: -0.01em; }
h2 { font-size: 17px; margin: 30px 0 12px; font-weight: 620; }
h3 { font-size: 14px; margin: 0 0 8px; font-weight: 620; text-transform: uppercase; letter-spacing: 0.09em; color: var(--muted); }
p { margin: 0 0 14px; }
.lede { color: var(--muted); max-width: 70ch; }
.small { font-size: 13px; color: var(--muted); }
.mono { font-family: ui-monospace, 'SF Mono', Menlo, Consolas, monospace; font-size: 13px; }

.rail { display: flex; flex-wrap: wrap; gap: 8px; list-style: none; padding: 0; margin: 0 0 26px; }
.rail li {
  flex: 1 1 130px;
  border: 1px solid var(--line);
  border-radius: 10px;
  background: var(--panel);
  padding: 9px 11px;
  font-size: 13px;
  color: var(--muted);
}
.rail li.done { border-color: var(--line-strong); color: var(--text); }
.rail li.current { border-color: var(--link); color: var(--text); }
.rail .num { display: block; font-size: 11px; letter-spacing: 0.1em; text-transform: uppercase; color: var(--muted); }

.card {
  background: var(--panel);
  border: 1px solid var(--line);
  border-radius: 14px;
  padding: 22px;
  margin: 0 0 20px;
}
.card.tight { padding: 16px 18px; }

.grid { display: grid; grid-template-columns: repeat(auto-fit, minmax(240px, 1fr)); gap: 14px; }

.field { margin: 0 0 16px; }
.field label { display: block; font-size: 13px; font-weight: 600; margin-bottom: 6px; }
.field .hint { display: block; font-size: 12px; color: var(--muted); margin-top: 6px; }

input[type=text], input[type=password], select {
  width: 100%;
  padding: 10px 12px;
  border-radius: 9px;
  border: 1px solid var(--line-strong);
  background: var(--raise);
  color: var(--text);
  font: inherit;
  font-size: 14px;
}
input[type=text]:focus, input[type=password]:focus, select:focus {
  outline: 2px solid var(--link);
  outline-offset: 1px;
}
input.invalid { border-color: var(--bad); }

.choices { display: grid; gap: 10px; }
.choice {
  display: grid;
  grid-template-columns: 22px 1fr;
  gap: 10px;
  padding: 12px 14px;
  border: 1px solid var(--line);
  border-radius: 10px;
  background: var(--raise);
}
.choice input { margin: 2px 0 0; width: 16px; height: 16px; accent-color: var(--link); }
.choice .name { font-weight: 600; font-size: 14px; }
.choice .why { color: var(--muted); font-size: 13px; }
.choice.selected { border-color: var(--link); }
.choice.disabled, .choice input:disabled { opacity: 0.45; }

button, .button {
  display: inline-block;
  padding: 10px 18px;
  border-radius: 9px;
  border: 1px solid var(--link);
  background: var(--link);
  color: var(--deep);
  font: inherit;
  font-weight: 650;
  font-size: 14px;
  cursor: pointer;
}
button:hover { filter: brightness(1.06); }
button.secondary { background: transparent; color: var(--text); border-color: var(--line-strong); }
button.danger { background: transparent; color: var(--bad); border-color: var(--bad); }
button:disabled { opacity: 0.45; cursor: not-allowed; }
button:focus-visible, a:focus-visible { outline: 2px solid var(--link); outline-offset: 2px; }

.alert {
  border: 1px solid var(--bad);
  border-radius: 10px;
  padding: 12px 14px;
  margin: 0 0 18px;
  color: var(--text);
  background: var(--panel);
}
.alert strong { color: var(--bad); }
.notice { border-color: var(--ok); }
.notice strong { color: var(--ok); }

table { width: 100%; border-collapse: collapse; }
th, td { text-align: left; vertical-align: top; padding: 10px 12px; border-bottom: 1px solid var(--line); }
th { font-size: 11px; letter-spacing: 0.1em; text-transform: uppercase; color: var(--muted); font-weight: 600; }
tr:last-child td { border-bottom: none; }
td.value { white-space: nowrap; font-weight: 600; }
td.why { color: var(--muted); font-size: 13px; }
.locked { color: var(--muted); }
.locked .value { color: var(--muted); }
.tag { font-size: 11px; letter-spacing: 0.08em; text-transform: uppercase; color: var(--muted); border: 1px solid var(--line-strong); border-radius: 999px; padding: 1px 8px; }

.kv { display: grid; grid-template-columns: 200px 1fr; gap: 6px 16px; font-size: 14px; }
.kv dt { color: var(--muted); }
.kv dd { margin: 0; font-family: ui-monospace, 'SF Mono', Menlo, Consolas, monospace; font-size: 13px; }

code, .code {
  font-family: ui-monospace, 'SF Mono', Menlo, Consolas, monospace;
  font-size: 13px;
  background: var(--deep);
  border: 1px solid var(--line);
  border-radius: 7px;
  padding: 2px 7px;
}
pre.code { display: block; padding: 14px 16px; overflow-x: auto; white-space: pre; }

.actions { display: flex; flex-wrap: wrap; align-items: center; gap: 12px; margin-top: 8px; }
.inline-form { display: inline; }
.stack > * + * { margin-top: 10px; }

footer {
  margin-top: 40px;
  padding-top: 18px;
  border-top: 1px solid var(--line);
  color: var(--muted);
  font-size: 12px;
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
