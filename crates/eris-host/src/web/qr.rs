//! The pairing code, as something a camera can read.
//!
//! The legacy app has always paired by scanning a QR, and the payload it parses is
//! compact JSON:
//!
//! ```json
//! {"code":"ABCD1234","relay":"http://host:7341"}
//! ```
//!
//! That shape is kept rather than invented, because the client on the other side
//! of the scan is already written against it. What changed is where `relay` comes
//! from: the daemon's own configured device bind, so a Pro customer scanning the
//! code gets the address their phone can actually reach.

use serde::Serialize;

/// What the QR encodes. Field names are the client's contract, not a preference.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Pairing {
    /// The single-use pairing code.
    pub code: String,
    /// The device API base URL the app should call.
    pub relay: String,
}

impl Pairing {
    /// Build the payload for one code and one reachable address.
    #[must_use]
    pub fn new(code: &str, relay: &str) -> Self {
        Self {
            code: code.to_owned(),
            relay: relay.to_owned(),
        }
    }

    /// The compact JSON a scan delivers.
    ///
    /// Compact on purpose: every byte is a QR module, and the legacy writer used
    /// `separators=(",", ":")`. Whitespace here would not break a parser, but it
    /// would make the code denser for no reason.
    #[must_use]
    pub fn payload(&self) -> String {
        serde_json::json!({ "code": self.code, "relay": self.relay }).to_string()
    }
}

/// An SVG rendering of one payload, as a self-contained `<svg>` fragment.
///
/// Drawn as rects rather than a PNG so the page carries no binary and no extra
/// dependency on an image encoder. Quiet zones are included: a code scanned off a
/// dark page needs its margin, and a QR without one is the most common way a code
/// "does not work".
#[must_use]
pub fn svg(payload: &str, module_px: u32) -> Option<String> {
    let code = qrcode::QrCode::new(payload.as_bytes()).ok()?;
    let width = code.width();
    let quiet = 2u32;
    let side = u32::try_from(width).ok()? + quiet * 2;
    let size = side * module_px;

    let mut out = String::with_capacity(4096);
    out.push_str(&format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{size}\" height=\"{size}\" \
         viewBox=\"0 0 {size} {size}\" role=\"img\" aria-label=\"Pairing code\">"
    ));
    out.push_str("<rect width=\"100%\" height=\"100%\" fill=\"#ffffff\"/>");
    let colors = code.to_colors();
    for (index, color) in colors.iter().enumerate() {
        if *color != qrcode::Color::Dark {
            continue;
        }
        let x = u32::try_from(index % width).ok()? + quiet;
        let y = u32::try_from(index / width).ok()? + quiet;
        out.push_str(&format!(
            "<rect x=\"{}\" y=\"{}\" width=\"{module_px}\" height=\"{module_px}\" fill=\"#000000\"/>",
            x * module_px,
            y * module_px
        ));
    }
    out.push_str("</svg>");
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_payload_is_the_legacy_shape_compactly() {
        let payload = Pairing::new("ABCD1234", "http://100.64.0.10:7341").payload();
        assert_eq!(
            payload,
            r#"{"code":"ABCD1234","relay":"http://100.64.0.10:7341"}"#
        );
        assert!(!payload.contains(": "), "no whitespace: {payload}");
    }

    #[test]
    fn a_scan_renders_as_svg_with_a_quiet_zone() {
        let svg = svg(
            &Pairing::new("ABCD1234", "http://127.0.0.1:7341").payload(),
            6,
        )
        .expect("a short payload always fits");
        assert!(svg.starts_with("<svg"), "self contained svg");
        assert!(svg.contains("role=\"img\""), "announced to a screen reader");
        assert!(svg.contains("fill=\"#ffffff\""), "a quiet zone exists");
        assert!(svg.contains("fill=\"#000000\""), "modules are drawn");
        assert!(!svg.contains("<image"), "no embedded raster");
    }

    #[test]
    fn the_svg_grows_with_the_module_size() {
        let payload = Pairing::new("ABCD1234", "http://127.0.0.1:7341").payload();
        let small = svg(&payload, 4).expect("renders");
        let large = svg(&payload, 8).expect("renders");
        let size = |s: &str| {
            s.split("width=\"")
                .nth(1)
                .and_then(|rest| rest.split('"').next())
                .and_then(|value| value.parse::<u32>().ok())
                .unwrap_or(0)
        };
        assert!(
            size(&large) > size(&small),
            "a bigger module is a bigger code"
        );
    }

    #[test]
    fn a_payload_too_long_for_a_code_returns_none_rather_than_a_broken_image() {
        let huge = "x".repeat(10_000);
        assert!(svg(&huge, 4).is_none());
    }
}
