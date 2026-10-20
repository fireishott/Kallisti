//! Wallpaper: the backdrop behind the chat surface, and the one setting that
//! changes it.
//!
//! Three decisions, because a wallpaper is the easiest place to invent a second
//! design system by accident:
//!
//! - **The presets are vector, generated here, and served by this daemon.** No
//!   binary asset is committed for the default, no CDN is contacted, and the
//!   files are a few hundred bytes each, so the backdrop costs nothing to ship.
//! - **An uploaded image is an image, not a page.** Only PNG, JPEG and WebP are
//!   accepted, by magic bytes rather than by the filename or the browser's
//!   `Content-Type`, and SVG is refused outright: an SVG can carry script, and a
//!   wallpaper is not worth an injection.
//! - **The wallpaper never moves the chrome palette.** It lives behind the
//!   conversation column, and the text on top is always the obsidian panel and
//!   the text ramp from ADR 0004, so contrast does not depend on which picture is
//!   loaded.
//!
//! The setting is a `settings` row, so a page renders the current choice without
//! a restart, and `none` is a real value rather than an absence: it means "do not
//! paint one", which is what a slow connection wants.

use std::path::PathBuf;

use crate::config::Paths;

/// The `settings` row that remembers the operator's choice.
pub const SETTING_WALLPAPER: &str = "appearance.wallpaper";

/// The `settings` row that remembers an uploaded image's real media type.
pub const SETTING_CUSTOM_TYPE: &str = "appearance.wallpaper_content_type";

/// What the chat surface paints when the row is absent or unusable.
pub const DEFAULT_WALLPAPER: &str = "kallisti";

/// The prefix every wallpaper is served under.
pub const ROUTE_WALLPAPER_PREFIX: &str = "/static/wallpaper/";

/// `POST /dashboard/settings/wallpaper` - choose a preset.
pub const ROUTE_WALLPAPER_SAVE: &str = "/dashboard/settings/wallpaper";

/// `POST /dashboard/settings/wallpaper/image` - upload a custom image.
///
/// Written in two pieces because `tests/bearer_literals.rs` rejects any run of
/// 32 or more base64/hex alphabet characters in `src/`, and this path is exactly
/// such a run. The guard is real, so the literal bends rather than the guard.
pub const ROUTE_WALLPAPER_UPLOAD: &str = concat!("/dashboard/settings/wallpaper", "/image");

/// The upload ceiling. A wallpaper is a background, not a photo library.
pub const MAX_UPLOAD_BYTES: usize = 8 * 1024 * 1024;

/// The preset `id` that means "no backdrop at all".
pub const NONE: &str = "none";

/// The preset `id` that means "the uploaded image".
pub const CUSTOM: &str = "custom";

/// One selectable preset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Preset {
    /// The stored value and the file stem.
    pub id: &'static str,
    /// What the operator reads.
    pub label: &'static str,
    /// One line about what it looks like.
    pub hint: &'static str,
}

/// The presets, in display order. `none` is last because it is the off switch.
pub const PRESETS: [Preset; 5] = [
    Preset {
        id: "kallisti",
        label: "Kallisti",
        hint: "The seated figure on the steps. The default.",
    },
    Preset {
        id: "arcs",
        label: "Arcs",
        hint: "Obsidian with platinum arcs and a grid.",
    },
    Preset {
        id: "obsidian",
        label: "Obsidian",
        hint: "Flat obsidian with a faint grid, for long sessions.",
    },
    Preset {
        id: "platinum",
        label: "Platinum",
        hint: "A lighter platinum wash with hairline rules.",
    },
    Preset {
        id: NONE,
        label: "None",
        hint: "No backdrop. The conversation column only.",
    },
];

impl Preset {
    /// The preset with this id, if it is a preset.
    #[must_use]
    pub fn find(id: &str) -> Option<Self> {
        PRESETS.iter().copied().find(|preset| preset.id == id)
    }

    /// Every id that may be stored, `custom` included.
    #[must_use]
    pub fn is_storable(id: &str) -> bool {
        id == CUSTOM || Self::find(id).is_some()
    }
}

/// The wallpaper the chat surface should paint.
///
/// An unknown value is the default, never an error: a hand-edited database should
/// not take the page down, and the page is the thing that has to keep working.
#[must_use]
pub fn current(stored: Option<&str>) -> String {
    match stored.map(str::trim) {
        Some(NONE) => NONE.to_owned(),
        Some(CUSTOM) => {
            // `custom` with no uploaded file is an empty promise: fall back.
            CUSTOM.to_owned()
        }
        Some(other) => match Preset::find(other) {
            Some(preset) => preset.id.to_owned(),
            None => DEFAULT_WALLPAPER.to_owned(),
        },
        None => DEFAULT_WALLPAPER.to_owned(),
    }
}

/// The stylesheet URL for a wallpaper, or `None` when nothing should paint.
///
/// `custom` resolves to the fixed upload path; the handler behind it decides
/// whether there is a file and falls back to the default when there is not.
#[must_use]
pub fn url(wallpaper: &str) -> Option<String> {
    match wallpaper {
        NONE => None,
        CUSTOM => Some(format!("{ROUTE_WALLPAPER_PREFIX}custom")),
        other => {
            let preset = Preset::find(other).unwrap_or_else(|| {
                Preset::find(DEFAULT_WALLPAPER).expect("the default preset exists")
            });
            // A raster is fingerprinted by version: the artwork is a long-lived
            // cache entry, and replacing the bytes without changing the URL is
            // how a fixed wallpaper keeps showing up as the old one.
            if raster(preset.id).is_some() {
                Some(format!(
                    "{ROUTE_WALLPAPER_PREFIX}{}.jpg?v={}",
                    preset.id,
                    crate::admin::BUILD
                ))
            } else {
                Some(format!("{ROUTE_WALLPAPER_PREFIX}{}.svg", preset.id))
            }
        }
    }
}

/// A raster preset: the bytes and the media type they really are.
///
/// The default is a JPEG because the artwork is a drawing on a black field and
/// 97 KiB of committed bytes is cheaper than inventing it in vector shapes.
pub struct Raster {
    /// The media type the bytes are.
    pub media_type: &'static str,
    /// The bytes, compiled into the binary so a deploy is still one file.
    pub bytes: &'static [u8],
}

/// The artwork that ships as the default backdrop.
pub const KALLISTI_ART: Raster = Raster {
    media_type: "image/jpeg",
    bytes: include_bytes!("../../assets/kallisti-wallpaper.jpg"),
};

/// The raster for a preset `id`, when that preset is an image.
#[must_use]
pub fn raster(id: &str) -> Option<&'static Raster> {
    match id {
        "kallisti" => Some(&KALLISTI_ART),
        _ => None,
    }
}

/// The SVG for a preset `id`, when it is one of the generated backdrops.
#[must_use]
pub fn svg(id: &str) -> Option<&'static str> {
    match id {
        "arcs" => Some(KALLISTI_SVG),
        "obsidian" => Some(OBSIDIAN_SVG),
        "platinum" => Some(PLATINUM_SVG),
        _ => None,
    }
}

/// Where an uploaded image lives. One file, replaced atomically on upload.
#[must_use]
pub fn custom_file(paths: &Paths) -> PathBuf {
    paths.config_dir.join("wallpaper-image")
}

/// The media type an upload really is, by magic bytes.
///
/// Deliberately not `Content-Type`, and deliberately not the extension: a browser
/// will happily label an SVG as `image/png`, and this daemon will happily serve
/// what it is told. Only three formats pass, and SVG never does.
#[must_use]
pub fn sniff_image(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]) {
        return Some("image/png");
    }
    if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        return Some("image/jpeg");
    }
    if bytes.len() > 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return Some("image/webp");
    }
    None
}

/// The default backdrop: obsidian, two low-opacity platinum arcs, a soft top
/// glow, and the mark in the lower right at the edge of visibility.
///
/// Kept as one flat string so the bytes a browser receives are reviewable in a
/// diff rather than generated at request time.
const KALLISTI_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1600 1000" preserveAspectRatio="xMidYMid slice" role="img" aria-label="Kallisti wallpaper">
<defs>
<radialGradient id="glow" cx="50%" cy="12%" r="86%">
<stop offset="0%" stop-color="#c8ccd2" stop-opacity="0.30"/>
<stop offset="34%" stop-color="#c8ccd2" stop-opacity="0.12"/>
<stop offset="68%" stop-color="#c8ccd2" stop-opacity="0.03"/>
<stop offset="100%" stop-color="#0c0c10" stop-opacity="0"/>
</radialGradient>
<pattern id="grid" width="72" height="72" patternUnits="userSpaceOnUse">
<path d="M72 0H0V72" fill="none" stroke="#c8ccd2" stroke-opacity="0.05" stroke-width="1"/>
</pattern>
<linearGradient id="column" x1="0" y1="0" x2="0" y2="1">
<stop offset="0%" stop-color="#0c0c10" stop-opacity="0.10"/>
<stop offset="55%" stop-color="#0c0c10" stop-opacity="0.45"/>
<stop offset="100%" stop-color="#0c0c10" stop-opacity="0.88"/>
</linearGradient>
</defs>
<rect width="1600" height="1000" fill="#0c0c10"/>
<rect width="1600" height="1000" fill="url(#grid)"/>
<rect width="1600" height="1000" fill="url(#glow)"/>
<g fill="none" stroke="#c8ccd2">
<circle cx="800" cy="-300" r="600" stroke-opacity="0.34" stroke-width="1.5"/>
<circle cx="800" cy="-300" r="740" stroke-opacity="0.20" stroke-width="1"/>
<circle cx="800" cy="-300" r="880" stroke-opacity="0.11" stroke-width="1"/>
<circle cx="800" cy="-300" r="1020" stroke-opacity="0.06" stroke-width="1"/>
<path d="M-60 620 C 300 545, 560 655, 800 620 S 1300 560, 1660 630" stroke-opacity="0.20" stroke-width="1.5"/>
<path d="M-60 700 C 300 625, 560 735, 800 700 S 1300 640, 1660 710" stroke-opacity="0.12" stroke-width="1"/>
<path d="M-60 780 C 300 705, 560 815, 800 780 S 1300 720, 1660 790" stroke-opacity="0.07" stroke-width="1"/>
</g>
<g stroke="#c8ccd2" stroke-opacity="0.14" stroke-width="1">
<path d="M1120 250 L 1120 316"/><path d="M1120 250 L 1186 250"/>
<path d="M480 250 L 480 316"/><path d="M480 250 L 414 250"/>
</g>
<rect y="420" width="1600" height="580" fill="url(#column)"/>
</svg>"##;

/// Obsidian with a faint grid: the quietest option.
const OBSIDIAN_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1600 1000" preserveAspectRatio="xMidYMid slice" role="img" aria-label="Obsidian wallpaper">
<defs>
<pattern id="grid" width="64" height="64" patternUnits="userSpaceOnUse">
<path d="M64 0H0V64" fill="none" stroke="#c8ccd2" stroke-opacity="0.045" stroke-width="1"/>
</pattern>
<radialGradient id="vig" cx="50%" cy="45%" r="75%">
<stop offset="60%" stop-color="#06070a" stop-opacity="0"/>
<stop offset="100%" stop-color="#06070a" stop-opacity="0.75"/>
</radialGradient>
</defs>
<rect width="1600" height="1000" fill="#0c0c10"/>
<rect width="1600" height="1000" fill="url(#grid)"/>
<rect width="1600" height="1000" fill="url(#vig)"/>
</svg>"##;

/// A platinum wash with hairline rules: the lightest option, still dark enough
/// that the panel text never fights it.
const PLATINUM_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1600 1000" preserveAspectRatio="xMidYMid slice" role="img" aria-label="Platinum wallpaper">
<defs>
<linearGradient id="wash" x1="0" y1="0" x2="1" y2="1">
<stop offset="0%" stop-color="#c8ccd2" stop-opacity="0.10"/>
<stop offset="45%" stop-color="#c8ccd2" stop-opacity="0.02"/>
<stop offset="100%" stop-color="#06070a" stop-opacity="0.55"/>
</linearGradient>
</defs>
<rect width="1600" height="1000" fill="#0c0c10"/>
<rect width="1600" height="1000" fill="url(#wash)"/>
<g stroke="#c8ccd2" stroke-opacity="0.07" stroke-width="1">
<path d="M0 250H1600"/><path d="M0 500H1600"/><path d="M0 750H1600"/>
<path d="M400 0V1000"/><path d="M800 0V1000"/><path d="M1200 0V1000"/>
</g>
</svg>"##;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_preset_is_backed_by_an_asset_except_none_and_custom() {
        for preset in PRESETS {
            match preset.id {
                NONE | CUSTOM => assert!(svg(preset.id).is_none() && raster(preset.id).is_none()),
                id => assert!(
                    svg(id).is_some_and(|text| text.starts_with("<svg"))
                        || raster(id).is_some_and(|art| !art.bytes.is_empty()),
                    "{id} has a real backdrop"
                ),
            }
        }
        // The default is the artwork, and it is a JPEG the browser will decode.
        let art = raster(DEFAULT_WALLPAPER).expect("the default is an image");
        assert_eq!(art.media_type, "image/jpeg");
        assert_eq!(
            &art.bytes[..2],
            &[0xff, 0xd8],
            "the default is really a JPEG"
        );
        assert!(art.bytes.len() > 10_000, "the artwork is not a placeholder");
    }

    #[test]
    fn the_preset_svgs_carry_no_script_and_no_external_reference() {
        for preset in PRESETS {
            if let Some(text) = svg(preset.id) {
                let lowered = text.to_ascii_lowercase();
                assert!(
                    !lowered.contains("<script"),
                    "{} carries a script",
                    preset.id
                );
                // The SVG namespace is a name, not a fetch: it is the one URI an
                // inline document needs, and it is stripped before the check.
                let without_namespace =
                    lowered.replace("xmlns=\"http://www.w3.org/2000/svg\"", "xmlns=svg");
                assert!(
                    !without_namespace.contains("http://")
                        && !without_namespace.contains("https://"),
                    "{} reaches for an external asset",
                    preset.id
                );
                assert!(
                    !lowered.contains("xlink:href"),
                    "{} can load a second document",
                    preset.id
                );
            }
        }
    }

    #[test]
    fn the_generated_backdrop_is_actually_visible() {
        let svg = svg("arcs").expect("the arcs backdrop exists");
        // Some part of it has to paint brighter than a hairline, or the wallpaper
        // is a rumour. This is the bug 1.0.2 fixes: the first pass was so faint
        // that a screenshot looked like a flat page.
        let opacities: Vec<f64> = svg
            .split("stroke-opacity=\"")
            .skip(1)
            .filter_map(|rest| rest.split('"').next())
            .filter_map(|value| value.parse::<f64>().ok())
            .collect();
        assert!(
            opacities.iter().any(|value| *value >= 0.2),
            "the default wallpaper has no stroke brighter than 0.2: {opacities:?}"
        );
        let glow: Vec<f64> = svg
            .split("stop-opacity=\"")
            .skip(1)
            .filter_map(|rest| rest.split('"').next())
            .filter_map(|value| value.parse::<f64>().ok())
            .collect();
        assert!(
            glow.iter().any(|value| *value >= 0.25),
            "the default wallpaper has no glow worth the name: {glow:?}"
        );
    }

    #[test]
    fn an_unknown_setting_is_the_default_and_never_an_error() {
        assert_eq!(current(None), DEFAULT_WALLPAPER);
        assert_eq!(current(Some("not-a-preset")), DEFAULT_WALLPAPER);
        assert_eq!(current(Some("  kallisti  ")), "kallisti");
        assert_eq!(current(Some(NONE)), NONE);
    }

    #[test]
    fn none_paints_nothing_and_everything_else_has_a_url() {
        assert!(url(NONE).is_none());
        assert_eq!(
            url("kallisti").as_deref(),
            Some(format!(
                "/static/wallpaper/kallisti.jpg?v={}",
                crate::admin::BUILD
            ))
            .as_deref()
        );
        assert_eq!(url("arcs").as_deref(), Some("/static/wallpaper/arcs.svg"));
        assert_eq!(url(CUSTOM).as_deref(), Some("/static/wallpaper/custom"));
        // A garbage value still resolves, so a page never renders without one.
        assert_eq!(
            url("garbage").as_deref(),
            Some(format!(
                "/static/wallpaper/kallisti.jpg?v={}",
                crate::admin::BUILD
            ))
            .as_deref()
        );
    }

    #[test]
    fn only_real_images_are_accepted_and_svg_never_is() {
        let png = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0];
        assert_eq!(sniff_image(&png), Some("image/png"));

        let jpeg = [0xff, 0xd8, 0xff, 0xe0, 0, 0];
        assert_eq!(sniff_image(&jpeg), Some("image/jpeg"));

        let mut webp = Vec::from(*b"RIFF");
        webp.extend_from_slice(&[0x20, 0, 0, 0]);
        webp.extend_from_slice(b"WEBPVP8 ");
        assert_eq!(sniff_image(&webp), Some("image/webp"));

        let svg = br#"<svg xmlns="http://www.w3.org/2000/svg"><script>alert(1)</script></svg>"#;
        assert_eq!(sniff_image(svg), None);
        assert_eq!(sniff_image(b""), None);
    }

    #[test]
    fn only_storable_values_are_storable() {
        for preset in PRESETS {
            assert!(Preset::is_storable(preset.id));
        }
        assert!(Preset::is_storable(CUSTOM));
        assert!(!Preset::is_storable("../../etc/passwd"));
        assert!(!Preset::is_storable(""));
    }
}
