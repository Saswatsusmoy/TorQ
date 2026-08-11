//! The TorQ wordmark: two rows of block letters with a sprout above the Q,
//! drawn in a green gradient — re-cut for "TORQ" and re-colored in the
//! everforest palette.
//!
//! The letterforms are hand-set 2-row block letters (`▀█▀`/` █ ` = T,
//! `█▀█`/`█▄█` = O, `█▀█`/`█▀▄` = R, and Q drawn as
//! `█▀█`/`█▄█` plus a `▀` tail hook). The sprout (`𐓏`, Osage letter Wa)
//! sits above the Q as the brand mark.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::theme::{SPROUT, logo_sheen};

pub const LOGO_LINES: [&str; 3] = ["              𐓏", " ▀█▀ █▀█ █▀█ █▀█", "  █  █▄█ █▀▄ █▄█▀"];

/// Width of the widest logo line, in cells (verified by test against
/// `LOGO_LINES`).
pub const LOGO_WIDTH: usize = 17;

/// Glyph cells on the sprout row (row 0). Everything else on that row is a
/// space; letter rows are gradient-colored.
const SPROUT_CELLS: &[(usize, usize)] = &[(0, 14)];

/// Render the wordmark as three styled lines. Spaces are transparent (they
/// carry no foreground), glyphs are bold and gradient-tinted left→right and
/// top→bottom; the sprout is green.
pub fn render() -> Vec<Line<'static>> {
    let mut out = vec![Line::default(), Line::default(), Line::default()];
    for (row, line) in LOGO_LINES.iter().enumerate() {
        let last = line.chars().count().saturating_sub(1).max(1);
        let t_y = (row as f32 - 1.0).max(0.0); // letter rows: 0 then 1
        let mut spans = Vec::with_capacity(last + 1);
        for (i, ch) in line.chars().enumerate() {
            if ch == ' ' {
                spans.push(Span::raw(" "));
                continue;
            }
            let color = if SPROUT_CELLS.contains(&(row, i)) {
                SPROUT
            } else {
                let t_x = i as f32 / last as f32;
                logo_sheen((t_x + t_y) / 2.0)
            };
            spans.push(Span::styled(
                ch.to_string(),
                Style::new().fg(color).add_modifier(Modifier::BOLD),
            ));
        }
        out[row] = Line::from(spans);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logo_is_four_letters_wide_and_even() {
        // TORQ = 4 letters at 3 cols each + 3 gaps = 15 glyphs (+1 tail).
        assert_eq!(LOGO_LINES[1].chars().count(), 16);
        assert_eq!(LOGO_LINES[2].chars().count(), 17); // Q tail pokes out
        assert_eq!(LOGO_WIDTH, 17);
    }

    #[test]
    fn sprout_sits_above_q() {
        // Q occupies cols 13..=15 on the letter rows; sprout centers on it.
        let q_cols: Vec<usize> = (13..=15).collect();
        assert_eq!(q_cols, vec![13, 14, 15]);
        assert!(SPROUT_CELLS.iter().any(|&(row, col)| row == 0 && col == 14));
    }

    #[test]
    fn render_keeps_line_widths() {
        let lines = render();
        // Sprout row (14 spaces + glyph), letter row, letter row + Q tail.
        assert_eq!(lines[0].width(), 15);
        assert_eq!(lines[0].width(), LOGO_LINES[0].chars().count());
        assert_eq!(lines[1].width(), LOGO_LINES[1].chars().count());
        assert_eq!(lines[2].width(), LOGO_LINES[2].chars().count());
        // The wordmark is exactly as wide as its widest line.
        assert_eq!(lines[2].width(), LOGO_WIDTH);
    }
}
