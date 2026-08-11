//! TorQ visual theme — the everforest palette (sainnhe/everforest, dark
//! medium), applied to the TUI layout. Green is the accent family:
//! selection/pointers are green, success/checks aqua, errors red, warnings
//! yellow, and chrome falls back to the greys.

use ratatui::style::Color;

/// Canvas background: everforest `bg_dim`.
pub const BG: Color = Color::Rgb(0x23, 0x2a, 0x2e);
/// Primary accent — selection, pointers, active borders (everforest `green`).
pub const ACCENT: Color = Color::Rgb(0xa7, 0xc0, 0x80);
/// Bright accent — sidebar bar, logo highlight, sheen (everforest `aqua`).
pub const BRIGHT: Color = Color::Rgb(0x83, 0xc0, 0x92);
/// Muted accent — key hints, secondary text (everforest `grey1`).
pub const ALT: Color = Color::Rgb(0x85, 0x92, 0x89);
/// Primary text (everforest `fg`).
pub const TEXT: Color = Color::Rgb(0xd3, 0xc6, 0xaa);
/// Success — done checks, healthy seed counts (everforest `aqua`).
pub const GOOD: Color = Color::Rgb(0x83, 0xc0, 0x92);
/// Warning — notices, offline sources (everforest `yellow`).
pub const WARN: Color = Color::Rgb(0xdb, 0xbc, 0x7f);
/// Danger — failures (everforest `red`).
pub const BAD: Color = Color::Rgb(0xe6, 0x7e, 0x80);
/// Rule — borders, rails, dim chrome (everforest `grey0`).
pub const RULE: Color = Color::Rgb(0x7a, 0x84, 0x78);
/// Paused/queued torrent accent (everforest `grey1`).
pub const PAUSED: Color = Color::Rgb(0x85, 0x92, 0x89);
/// The logo sprout (everforest `aqua`).
pub const SPROUT: Color = Color::Rgb(0x83, 0xc0, 0x92);
/// Deep end of the logo/progress gradient: green pulled toward the canvas.
pub const DEEP: Color = Color::Rgb(0x79, 0x8b, 0x63);
/// Dark end of the logo gradient.
pub const SHADE: Color = Color::Rgb(0x51, 0x5e, 0x4b);
/// Progress-bar sheen peak: warm near-white.
pub const SHEEN_PEAK: Color = Color::Rgb(0xf4, 0xef, 0xdd);
/// Logo highlight: warm cream between the canvas and the aqua ramp.
pub const HIGHLIGHT: Color = Color::Rgb(0xf0, 0xea, 0xd6);

/// Terminal glyphs shared across views.
pub mod icon {
    pub const DONE: &str = "✓";
    pub const ERROR: &str = "✗";
    pub const PENDING: &str = "·";
    pub const POINTER: &str = "❯";
    pub const DOT: &str = "·";
    pub const BAR: &str = "▌";
    pub const DOWN: &str = "↓";
    pub const UP: &str = "↑";
    pub const PEER: &str = "•";
    pub const PAUSE: &str = "⏸";
    pub const WARN: &str = "⚠";
}

/// Per-source tag + color, re-mapped onto the everforest palette. `id` may
/// be a pasted magnet / bare infohash (empty)
/// or a plugin id — fall back to a neutral tag.
pub fn source_style(id: &str) -> (&'static str, Color) {
    match id {
        "fitgirl" => ("FG", ACCENT),
        "yts" => ("YTS", GOOD),
        "eztv" => ("EZTV", WARN),
        "nyaa" => ("NYAA", BRIGHT),
        "subsplease" => ("SUB", ALT),
        "tpb-movies" | "tpb-tv" => ("TPB", Color::Rgb(0x83, 0xc0, 0x92)),
        "x1337-movies" | "x1337-tv" => ("1337", Color::Rgb(0xe6, 0x98, 0x75)),
        "bittorrented" => ("BT", Color::Rgb(0x7f, 0xbb, 0xb3)),
        _ => ("•", ALT),
    }
}

/// Linear interpolation between two RGB colors at `t` in 0..=1.
pub fn lerp_rgb(a: Color, b: Color, t: f32) -> Color {
    let (ar, ag, ab) = rgb(a);
    let (br, bg, bb) = rgb(b);
    let t = t.clamp(0.0, 1.0);
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color::Rgb(l(ar, br), l(ag, bg), l(ab, bb))
}

/// Three-stop ramp: deep → mid → bright.
pub fn ramp(t: f32, deep: Color, mid: Color, bright: Color) -> Color {
    if t <= 0.5 {
        lerp_rgb(deep, mid, t / 0.5)
    } else {
        lerp_rgb(mid, bright, (t - 0.5) / 0.5)
    }
}

/// Logo gradient: warm cream → aqua → green → deep green → shade
/// (everforest colors). `t` in 0..=1 is the
/// character's position across the wordmark (x and y averaged, so the
/// gradient also runs down the two letter rows).
pub fn logo_sheen(t: f32) -> Color {
    let t = t.clamp(0.0, 1.0);
    if t < 0.15 {
        lerp_rgb(HIGHLIGHT, BRIGHT, t / 0.15)
    } else if t < 0.40 {
        lerp_rgb(BRIGHT, ACCENT, (t - 0.15) / 0.25)
    } else if t < 0.70 {
        lerp_rgb(ACCENT, DEEP, (t - 0.40) / 0.30)
    } else {
        lerp_rgb(DEEP, SHADE, (t - 0.70) / 0.30)
    }
}

fn rgb(c: Color) -> (u8, u8, u8) {
    match c {
        Color::Rgb(r, g, b) => (r, g, b),
        _ => (0, 0, 0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_style_covers_builtins_and_falls_back() {
        assert_eq!(source_style("yts").0, "YTS");
        assert_eq!(source_style("fitgirl").0, "FG");
        assert_eq!(source_style("x1337-tv").0, "1337");
        assert_eq!(source_style("subsplease").0, "SUB");
        // Unknown/plugin ids get the neutral tag, never a panic.
        assert_eq!(source_style("").0, "•");
        assert_eq!(source_style("some-plugin").0, "•");
    }

    #[test]
    fn lerp_endpoints_are_exact() {
        assert_eq!(lerp_rgb(ACCENT, BRIGHT, 0.0), ACCENT);
        assert_eq!(lerp_rgb(ACCENT, BRIGHT, 1.0), BRIGHT);
    }

    #[test]
    fn logo_sheen_spans_all_stops() {
        // Ramp endpoints: white → ... → shade, monotonic in luminance-ish.
        let a = logo_sheen(0.0);
        let b = logo_sheen(1.0);
        assert_ne!(a, b);
    }
}
