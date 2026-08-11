//! Display formatting: byte sizes, speeds, relative times, and safe text
//! truncation.

/// "1.96 GB", "920 B" (2 decimals above B).
pub fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{bytes} B")
    } else {
        format!("{v:.2} {}", UNITS[u])
    }
}

/// "7.7 MB/s", "12 MB/s", "920 B/s".
pub fn human_speed(bytes_per_sec: Option<f32>) -> String {
    match bytes_per_sec {
        Some(v) if v > 0.0 && v.is_finite() => {
            const UNITS: [&str; 4] = ["B/s", "KB/s", "MB/s", "GB/s"];
            let mut n = v as f64;
            let mut i = 0;
            while n >= 1024.0 && i < UNITS.len() - 1 {
                n /= 1024.0;
                i += 1;
            }
            if i == 0 {
                format!("{:.0} {}", n, UNITS[i])
            } else if n < 10.0 {
                format!("{n:.1} {}", UNITS[i])
            } else {
                format!("{n:.0} {}", UNITS[i])
            }
        }
        _ => "-".into(),
    }
}

/// "now", "5m ago", "2hr 3m ago", "1d 4hr ago".
pub fn relative_time(unix_secs: i64) -> String {
    if unix_secs <= 0 {
        return "-".into();
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let diff = (now - unix_secs).max(0);
    if diff < 60 {
        return "now".into();
    }
    let m = diff / 60;
    if m < 60 {
        return format!("{m}m ago");
    }
    let h = m / 60;
    if h < 24 {
        let rm = m % 60;
        return if rm > 0 {
            format!("{h}hr {rm}m ago")
        } else {
            format!("{h}hr ago")
        };
    }
    let d = h / 24;
    let rh = h % 24;
    if rh > 0 {
        format!("{d}d {rh}hr ago")
    } else {
        format!("{d}d ago")
    }
}

/// "1240", "12k", "1.2m".
pub fn count(n: u32) -> String {
    if n < 10_000 {
        return n.to_string();
    }
    let k = (n as f64 / 1_000.0).round();
    if k < 1_000.0 {
        return format!("{k:.0}k");
    }
    let m = n as f64 / 1_000_000.0;
    if m < 10.0 {
        let s = format!("{m:.1}m");
        return s.replace(".0m", "m");
    }
    format!("{:.0}m", m)
}

/// Cell-width truncation with an ellipsis (used for echoed queries and long
/// names in detail rows). Wide glyphs (CJK etc.) count 2 cells. A string that
/// fits in `max` cells is returned untouched.
pub fn truncate(s: &str, max: usize) -> String {
    use unicode_width::UnicodeWidthChar;
    use unicode_width::UnicodeWidthStr;
    if s.width() <= max {
        return s.to_string();
    }
    let mut w = 0usize;
    let mut end = 0usize;
    for (i, c) in s.char_indices() {
        let cw = c.width().unwrap_or(0);
        if w + cw > max.saturating_sub(1) {
            break;
        }
        w += cw;
        end = i + c.len_utf8();
    }
    format!("{}…", &s[..end])
}

/// Hard cut to `max` cells without an ellipsis — fixed-width table cells.
pub fn cut(s: &str, max: usize) -> String {
    use unicode_width::UnicodeWidthChar;
    let mut w = 0usize;
    let mut end = 0usize;
    for (i, c) in s.char_indices() {
        let cw = c.width().unwrap_or(0);
        if w + cw > max {
            break;
        }
        w += cw;
        end = i + c.len_utf8();
    }
    s[..end].to_string()
}

/// Sanitize attacker-influenced names: strip control chars, collapse
/// whitespace, fall back to "Untitled".
pub fn clean_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_space = true; // also trims leading whitespace
    for c in s.chars() {
        if c.is_control() {
            continue;
        }
        if c.is_whitespace() {
            if !prev_space {
                out.push(' ');
            }
            prev_space = true;
        } else {
            out.push(c);
            prev_space = false;
        }
    }
    let cleaned = out.trim_end();
    if cleaned.is_empty() {
        "Untitled".into()
    } else {
        cleaned.to_string()
    }
}

/// Strip control/escape-capable characters from strings printed verbatim
/// (info hashes, magnet URIs) so a hostile source can't smuggle e.g. an
/// OSC-52 clipboard write or CSI sequence into the terminal.
/// Preserves everything else exactly.
pub fn strip_control(s: &str) -> String {
    s.chars()
        .filter(|c| {
            let cp = *c as u32;
            !(cp <= 0x1f || cp == 0x7f || (0x80..=0x9f).contains(&cp))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_round_trip() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(920), "920 B");
        assert_eq!(human_bytes(1024), "1.00 KB");
        assert_eq!(human_bytes(1_960_000_000), "1.83 GB");
    }

    #[test]
    fn speed_formats() {
        assert_eq!(human_speed(None), "-");
        assert_eq!(human_speed(Some(0.0)), "-");
        assert_eq!(human_speed(Some(920.0)), "920 B/s");
        assert_eq!(human_speed(Some(7.7 * 1024.0 * 1024.0)), "7.7 MB/s");
        assert_eq!(human_speed(Some(12.0 * 1024.0 * 1024.0)), "12 MB/s");
    }

    #[test]
    fn relative_time_buckets() {
        assert_eq!(relative_time(0), "-");
        // Deterministic: "now" for anything within a minute of now.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        assert_eq!(relative_time(now), "now");
        assert_eq!(relative_time(now - 300), "5m ago");
        assert_eq!(relative_time(now - 3600 - 120), "1hr 2m ago");
        assert_eq!(relative_time(now - 2 * 3600), "2hr ago");
        assert_eq!(relative_time(now - 25 * 3600 - 4 * 3600), "1d 5hr ago");
        assert_eq!(relative_time(now - 2 * 86400), "2d ago");
    }

    #[test]
    fn counts_compact() {
        assert_eq!(count(0), "0");
        assert_eq!(count(1240), "1240");
        assert_eq!(count(12_400), "12k");
        assert_eq!(count(1_200_000), "1.2m");
        assert_eq!(count(12_000_000), "12m");
    }

    #[test]
    fn truncation_shapes() {
        assert_eq!(truncate("abcde", 5), "abcde");
        assert_eq!(truncate("abcdef", 5), "abcd…");
        assert_eq!(cut("abcdef", 5), "abcde");
        assert_eq!(cut("aé", 1), "a");
    }

    #[test]
    fn clean_text_sanitizes() {
        assert_eq!(
            clean_text("  Oppenheimer \t (2023)  "),
            "Oppenheimer (2023)"
        );
        assert_eq!(clean_text("a\u{1b}b"), "ab");
        assert_eq!(clean_text("\u{00a0}\u{00a0}"), "Untitled");
    }

    #[test]
    fn strip_control_removes_escape_capable() {
        assert_eq!(strip_control("ab\u{1b}]52;c\u{07}"), "ab]52;c");
        assert_eq!(
            strip_control("magnet:?xt=urn:btih:abc"),
            "magnet:?xt=urn:btih:abc"
        );
        // DEL and C1 controls are stripped too.
        assert_eq!(strip_control("x\u{7f}y\u{9b}"), "xy");
    }
}
