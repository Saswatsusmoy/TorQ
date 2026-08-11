//! Rendering: splash, sidebar rail, search/download panels, footer hints, and
//! the help card.
//!
//! Layout contracts (all measured, nothing floats):
//! - the screen is padded 1 column on each side
//! - the top shows the 3-row wordmark, a rule, a gutter row, the body, footer
//! - the body is a sidebar rail (GUTTER + widest label incl. badge) + 1 gap
//!   + content; panels are `╭─ Title (n) ─...─╮` frames with `│ ` insets.
//!
//! Column widths are measured in terminal cells (wide glyphs like CJK count
//! 2), so rows stay aligned on any font.

use torq_core::daemon::{Status, TorrentView};
use torq_sources::TorrentResult;
use unicode_width::UnicodeWidthStr;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};

use crate::app::{App, Region, SearchMode, Section, Sort, SortField, View};
use crate::format::{
    clean_text, cut, human_bytes, human_speed, relative_time, strip_control, truncate,
};
use crate::logo;
use crate::theme::{self, icon};

/// Pointer column width.
const MARK: u16 = 2;
/// Status-icon column width.
const GUTTER: u16 = 2;
/// Progress-bar sheen parameters.
const SHEEN_RADIUS: f32 = 4.5;
const SHEEN_GAP: usize = 8;
const SHEEN_SPEED: f32 = 0.45;
const SHEEN_MAX: f32 = 0.9;

const SPINNER_FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

pub fn draw(f: &mut Frame, app: &mut App) {
    f.render_widget(Block::default().style(Style::new().bg(theme::BG)), f.area());
    match app.view {
        View::Splash => draw_splash(f, app),
        View::Browser => draw_browser(f, app),
    }
}

// Browser shell

fn draw_browser(f: &mut Frame, app: &mut App) {
    let cols = f.area().width;
    let rows = f.area().height;
    let compact = rows < 18;
    let show_rule = !compact;
    let show_footer = rows >= 12;

    let mut cons = vec![Constraint::Length(3)];
    if show_rule {
        cons.push(Constraint::Length(1));
    }
    cons.push(Constraint::Length(1)); // gutter between rule and body
    cons.push(Constraint::Min(1));
    if show_footer {
        cons.push(Constraint::Length(1));
    }
    let areas = Layout::vertical(cons).split(f.area());
    let top = areas[0];
    let rule = if show_rule { Some(areas[1]) } else { None };
    let body = if show_rule { areas[3] } else { areas[2] };
    let footer = if show_footer {
        Some(if show_rule { areas[4] } else { areas[3] })
    } else {
        None
    };

    draw_top(f, top, cols, app);
    if let Some(r) = rule {
        draw_rule(f, r);
    }
    if app.help {
        // The help card replaces the body; the footer hides too.
        draw_help(f, body, cols);
        return;
    }
    let rail = rail_width();
    // The whole body sits inside the 1-column screen padding:
    // sidebar, 1 gap, then the content panels.
    let padded = Rect {
        x: 1,
        y: body.y,
        width: cols.saturating_sub(2),
        height: body.height,
    };
    let horiz = Layout::horizontal([
        Constraint::Length(rail),
        Constraint::Length(1),
        Constraint::Min(0),
    ])
    .split(padded);
    draw_sidebar(f, horiz[0], app);
    match app.section {
        Section::Downloads => draw_downloads(f, horiz[2], app, false),
        Section::Seeding => draw_downloads(f, horiz[2], app, true),
        _ => draw_search_view(f, horiz[2], app),
    }
    if let Some(ft) = footer {
        draw_footer(f, ft, cols, app);
    }
}

/// Logo (or wordmark fallback) on the left; notice / daemon base on the right.
fn draw_top(f: &mut Frame, area: Rect, cols: u16, app: &App) {
    let pad_w = cols.saturating_sub(2);
    if pad_w >= logo::LOGO_WIDTH as u16 + 2 {
        f.render_widget(
            Paragraph::new(logo::render()).style(Style::new().bg(theme::BG)),
            Rect {
                x: 1,
                y: area.y,
                width: logo::LOGO_WIDTH as u16,
                height: 3,
            },
        );
    } else {
        f.render_widget(
            Paragraph::new(Line::from(vec![Span::styled(
                "torq",
                Style::new().fg(theme::ACCENT).add_modifier(Modifier::BOLD),
            )])),
            Rect {
                x: 1,
                y: area.y,
                width: pad_w,
                height: 1,
            },
        );
    }

    // Right of the wordmark row: a notice wins over the daemon address.
    let side = if let Some(n) = &app.notice {
        vec![Span::styled(
            format!("{} {}", icon::WARN, truncate(n, 60)),
            Style::new().fg(theme::WARN),
        )]
    } else {
        vec![Span::styled(
            format!("torq @ {}", truncate(&app.base, 60)),
            Style::new().fg(theme::RULE),
        )]
    };
    let side_w = pad_w.saturating_sub(logo::LOGO_WIDTH as u16 + 2);
    if side_w > 8 {
        f.render_widget(
            Paragraph::new(Line::from(fit(side, side_w as usize)))
                .style(Style::new().bg(theme::BG)),
            Rect {
                x: 1 + logo::LOGO_WIDTH as u16 + 2,
                y: area.y,
                width: side_w,
                height: 1,
            },
        );
    }
}

fn draw_rule(f: &mut Frame, area: Rect) {
    let w = area.width.saturating_sub(2) as usize;
    f.render_widget(
        Paragraph::new(Line::from(vec![Span::styled(
            "─".repeat(w),
            Style::new().fg(theme::RULE),
        )])),
        Rect {
            x: 1,
            y: area.y,
            width: area.width.saturating_sub(2),
            height: 1,
        },
    );
}

// Sidebar rail

fn rail_width() -> u16 {
    let max = Section::ALL
        .iter()
        .map(|s| {
            let badge = matches!(s, Section::Downloads | Section::Seeding);
            str_w(s.label()) + if badge { 5 } else { 0 }
        })
        .max()
        .unwrap_or(0);
    GUTTER + max as u16
}

fn draw_sidebar(f: &mut Frame, area: Rect, app: &App) {
    let focused = app.region == Region::Sidebar;
    let mut lines: Vec<Line<'static>> = Vec::new();
    for (gi, group) in [&Section::ALL[..5], &Section::ALL[5..]].iter().enumerate() {
        if gi > 0 {
            lines.push(Line::from(vec![Span::raw(" ")]));
        }
        for s in *group {
            let selected = *s == app.section;
            let badge = match s {
                Section::Downloads => app.active_count(),
                Section::Seeding => app.seeding_count(),
                _ => 0,
            };
            let mut spans: Vec<Span<'static>> = Vec::new();
            if selected {
                let bar = Style::new().fg(if focused { theme::BRIGHT } else { theme::RULE });
                spans.push(Span::styled(icon::BAR, bar));
                spans.push(Span::raw(" "));
            } else {
                spans.push(Span::raw("  "));
            }
            let mut label_style = Style::new();
            if selected {
                label_style = label_style.fg(if focused { theme::ACCENT } else { theme::ALT });
                if focused {
                    label_style = label_style.add_modifier(Modifier::BOLD);
                }
            } else {
                label_style = label_style.add_modifier(Modifier::DIM);
            }
            spans.push(Span::styled(s.label(), label_style));
            if badge > 0 {
                spans.push(Span::styled(
                    format!(" ({badge})"),
                    Style::new().add_modifier(Modifier::DIM),
                ));
            }
            lines.push(Line::from(fit(spans, area.width as usize)));
        }
    }
    f.render_widget(
        Paragraph::new(lines).style(Style::new().bg(theme::BG)),
        area,
    );
}

// Search view: search bar + results/details panel

fn draw_search_view(f: &mut Frame, area: Rect, app: &mut App) {
    let [bar, results] = Layout::vertical([Constraint::Length(3), Constraint::Min(0)]).areas(area);
    draw_search_bar(f, bar, app);
    // 1-row gap between the search bar and the results panel.
    let results = Rect {
        x: results.x,
        y: results.y + 1,
        width: results.width,
        height: results.height.saturating_sub(1),
    };
    draw_results(f, results, app);
}

fn draw_search_bar(f: &mut Frame, area: Rect, app: &mut App) {
    let editing = app.view == View::Splash || app.mode == SearchMode::Editing;
    let inner_w = area.width.saturating_sub(4) as usize;
    let mut spans = vec![
        Span::styled(icon::POINTER, Style::new().fg(theme::ACCENT)),
        Span::raw(" "),
    ];
    let shown = if editing && !app.edit.is_empty() {
        Some(clean_text(&app.edit))
    } else if !editing && !app.query.is_empty() {
        Some(clean_text(&app.query))
    } else {
        None
    };
    match shown {
        Some(text) => spans.push(Span::styled(text, Style::new().fg(theme::TEXT))),
        None => spans.push(Span::styled(
            "Search or paste a magnet link…",
            Style::new().fg(theme::RULE),
        )),
    }
    draw_panel(
        f,
        area,
        "search",
        None,
        editing,
        vec![Line::from(fit(spans, inner_w))],
    );
    if editing {
        let text_w = str_w(&app.edit);
        let x = area.x + 4 + text_w as u16;
        f.set_cursor_position((x.min(area.x + area.width.saturating_sub(2)), area.y + 1));
    }
}

fn draw_results(f: &mut Frame, area: Rect, app: &App) {
    let results = app.visible_results();
    let browsing = app.query.trim().is_empty();
    let focused = app.region == Region::Content && app.mode != SearchMode::Editing;
    let inner_w = area.width.saturating_sub(4) as usize;
    let inner_h = area.height.saturating_sub(2) as usize;

    let mut lines: Vec<Line<'static>> = Vec::new();
    if app.mode == SearchMode::Detail && app.detail.is_some() {
        lines.extend(detail_lines(app.detail.as_ref().expect("checked"), inner_w));
    } else {
        lines.push(Line::from(fit(
            results_status(app, results.len(), browsing),
            inner_w,
        )));
        if !results.is_empty() {
            // Header, separator, then rows — the table grid.
            let list_h = inner_h.saturating_sub(3).max(1);
            let start = window_start(app.cursor, results.len(), list_h);
            let name_w = inner_w.saturating_sub(1 + 10 + 1 + 7 + 1 + 5 + 1 + 5);
            lines.push(Line::from(fit(header_spans(name_w, app.sort), inner_w)));
            lines.push(Line::from(fit(table_separator(name_w), inner_w)));
            for (i, r) in results.iter().enumerate().skip(start).take(list_h) {
                let here = i == app.cursor && focused;
                lines.push(Line::from(fit(result_row(r, name_w, here), inner_w)));
            }
        }
    }

    let (title, count) = if app.mode == SearchMode::Detail && app.detail.is_some() {
        ("details", None)
    } else if browsing {
        ("latest", Some(results.len()))
    } else {
        ("results", Some(results.len()))
    };
    draw_panel(f, area, title, count, focused, lines);
}

fn results_status(app: &App, results_len: usize, browsing: bool) -> Vec<Span<'static>> {
    let dim = Style::new().add_modifier(Modifier::DIM);
    if app.searching {
        let frame = SPINNER_FRAMES[(app.tick / 8) as usize % SPINNER_FRAMES.len()];
        let label = if browsing {
            "Loading…"
        } else {
            "Searching…"
        };
        return vec![
            Span::styled(frame, Style::new().fg(theme::ACCENT)),
            Span::styled(format!(" {label}"), dim),
        ];
    }
    if results_len == 0 {
        if !app.offline.is_empty() && app.offline.len() >= crate::app::source_count() {
            return vec![Span::styled(
                "Couldn't reach any source. They may be down.",
                Style::new().fg(theme::WARN),
            )];
        }
        let msg = if browsing {
            "Nothing new right now.".to_string()
        } else {
            format!("No results for \"{}\".", truncate(&app.query, 28))
        };
        return vec![Span::styled(msg, dim)];
    }
    let head = if browsing {
        "newest across all sources".to_string()
    } else {
        format!(
            "{} result{}",
            results_len,
            if results_len == 1 { "" } else { "s" }
        )
    };
    let mut out = vec![Span::styled(head, dim)];
    if !app.offline.is_empty() {
        out.push(Span::styled(
            format!(
                " ({} source{} down)",
                app.offline.len(),
                if app.offline.len() == 1 { "" } else { "s" }
            ),
            dim,
        ));
    }
    if app.sort != Sort::None {
        out.push(Span::styled(format!(" · sort: {}", app.sort.label()), dim));
    }
    out
}

/// Table header row with `│` column separators: `│ Name │ Size │ Seeds │ Lch │ Src │`.
fn header_spans(name_w: usize, sort: Sort) -> Vec<Span<'static>> {
    let st = Style::new().add_modifier(Modifier::BOLD | Modifier::DIM);
    let grid = Style::new().fg(theme::RULE);
    let mut out = vec![Span::styled(format!("{:<name_w$}", "Name"), st)];
    out.push(Span::styled("│", grid));
    out.push(Span::styled(
        format!(" {:>9}", sort_mark(sort, SortField::Size, "Size")),
        st,
    ));
    out.push(Span::styled("│", grid));
    out.push(Span::styled(
        format!(" {:>6}", sort_mark(sort, SortField::Seeders, "Seeds")),
        st,
    ));
    out.push(Span::styled("│", grid));
    out.push(Span::styled(format!(" {:>4}", "Lch"), st));
    out.push(Span::styled("│", grid));
    out.push(Span::styled(
        format!(" {:>4}", sort_mark(sort, SortField::Source, "Src")),
        st,
    ));
    out
}

/// Horizontal rule between the header and rows, aligned to the column grid.
fn table_separator(name_w: usize) -> Vec<Span<'static>> {
    let grid = Style::new().fg(theme::RULE);
    let line = format!(
        "{}{}{}{}{}{}{}{}{}",
        "─".repeat(name_w),
        "┼",
        "─".repeat(10),
        "┼",
        "─".repeat(7),
        "┼",
        "─".repeat(5),
        "┼",
        "─".repeat(5)
    );
    vec![Span::styled(line, grid)]
}

/// Column label with a sort arrow when `sort` targets that field.
fn sort_mark(sort: Sort, field: SortField, label: &str) -> String {
    let arrow = if sort != Sort::None && sort.field_matches(field) {
        sort.arrow().map(|a| a.to_string()).unwrap_or_default()
    } else {
        String::new()
    };
    format!("{label}{arrow}")
}

fn result_row(r: &TorrentResult, name_w: usize, here: bool) -> Vec<Span<'static>> {
    let (tag, tag_color) = theme::source_style(&r.source);
    let dim = Style::new().add_modifier(Modifier::DIM);
    let bold = Style::new().add_modifier(Modifier::BOLD);
    let grid = Style::new().fg(theme::RULE);

    let mut out: Vec<Span<'static>> = Vec::new();
    // Name cell: pointer + name, left-aligned, `│` on the right.
    let name_style = if here {
        Style::new().fg(theme::ACCENT).add_modifier(Modifier::BOLD)
    } else {
        dim
    };
    let pointer = if here {
        format!("{} ", icon::POINTER)
    } else {
        "  ".to_string()
    };
    let name = format!(
        "{pointer}{}",
        cut(&clean_text(&r.name), name_w.saturating_sub(2))
    );
    out.push(Span::styled(format!("{name:<name_w$}"), name_style));
    out.push(Span::styled("│", grid));
    // Size: right-aligned, 10 cells including the leading space.
    let size = if r.size_bytes > 0 {
        human_bytes(r.size_bytes)
    } else {
        "-".into()
    };
    out.push(Span::styled(
        format!(" {:>9}", cut(&size, 9)),
        if here { bold } else { dim },
    ));
    out.push(Span::styled("│", grid));
    // Seeders: green when alive.
    let seeds = crate::format::count(r.seeders);
    let seed_style = if here {
        if r.seeders > 0 {
            Style::new().fg(theme::GOOD).add_modifier(Modifier::BOLD)
        } else {
            bold
        }
    } else if r.seeders > 0 {
        Style::new().fg(theme::GOOD).add_modifier(Modifier::DIM)
    } else {
        dim
    };
    out.push(Span::styled(format!(" {:>6}", cut(&seeds, 6)), seed_style));
    out.push(Span::styled("│", grid));
    // Leechers: plain count.
    let leech = crate::format::count(r.leechers);
    out.push(Span::styled(
        format!(" {:>4}", cut(&leech, 4)),
        if here { bold } else { dim },
    ));
    out.push(Span::styled("│", grid));
    // Source tag.
    let src_style = if here {
        Style::new().fg(tag_color).add_modifier(Modifier::BOLD)
    } else {
        Style::new().fg(tag_color).add_modifier(Modifier::DIM)
    };
    out.push(Span::styled(format!(" {:>4}", cut(tag, 4)), src_style));
    out
}

fn detail_lines(r: &TorrentResult, inner_w: usize) -> Vec<Line<'static>> {
    let (tag, tag_color) = theme::source_style(&r.source);
    let dim = Style::new().add_modifier(Modifier::DIM);
    let alt = Style::new().fg(theme::ALT).add_modifier(Modifier::DIM);
    let value_w = inner_w.saturating_sub(10);

    let mut lines = Vec::new();

    let name_w = inner_w.saturating_sub(2 + 4);
    let mut name_spans = vec![Span::styled(
        cut(&clean_text(&r.name), name_w),
        Style::new().fg(theme::TEXT).add_modifier(Modifier::BOLD),
    )];
    name_spans.push(Span::raw("  "));
    name_spans.push(Span::styled(
        tag,
        Style::new().fg(tag_color).add_modifier(Modifier::BOLD),
    ));
    lines.push(Line::from(fit(name_spans, inner_w)));

    lines.push(Line::from(vec![Span::styled(
        "─".repeat(inner_w),
        Style::new().fg(theme::RULE),
    )]));
    lines.push(Line::from(vec![Span::raw(" ")]));

    let row = |label: &str, value: Vec<Span<'static>>| -> Line<'static> {
        let mut spans = vec![Span::styled(pad_right(label, 9), dim)];
        spans.extend(value);
        Line::from(fit(spans, inner_w))
    };

    lines.push(row(
        "Size",
        vec![if r.size_bytes > 0 {
            Span::styled(human_bytes(r.size_bytes), Style::new().fg(theme::TEXT))
        } else {
            Span::styled("unknown", dim)
        }],
    ));
    let health = if r.seeders > 0 || r.leechers > 0 {
        let mut spans = vec![Span::styled(
            crate::format::count(r.seeders),
            if r.seeders > 0 {
                Style::new().fg(theme::GOOD).add_modifier(Modifier::BOLD)
            } else {
                Style::new().fg(theme::TEXT)
            },
        )];
        spans.push(Span::styled(
            format!(" seeders {} {} leechers", icon::DOT, r.leechers),
            dim,
        ));
        spans
    } else {
        vec![Span::styled("unknown", dim)]
    };
    lines.push(row("Health", health));
    if let Some(n) = r.num_files {
        lines.push(row("Files", vec![Span::styled(n.to_string(), dim)]));
    }
    if r.added.is_some_and(|a| a > 0) {
        lines.push(row(
            "Added",
            vec![Span::styled(relative_time(r.added.unwrap_or(0)), dim)],
        ));
    }
    // Hash + Magnet: strip control chars so a hostile source can't inject
    // escape sequences into the terminal.
    lines.push(row(
        "Hash",
        vec![Span::styled(
            cut(&strip_control(&r.info_hash), value_w),
            alt,
        )],
    ));
    lines.push(row(
        "Magnet",
        vec![Span::styled(cut(&strip_control(&r.magnet), value_w), alt)],
    ));

    lines.push(Line::from(vec![Span::raw(" ")]));
    let hint = vec![
        Span::styled(
            "d",
            Style::new().fg(theme::ACCENT).add_modifier(Modifier::BOLD),
        ),
        Span::styled(" Download", Style::new().fg(theme::TEXT)),
        Span::styled(format!("  {}  ", icon::DOT), dim),
        Span::styled("esc", Style::new().fg(theme::ALT)),
        Span::styled(" back", dim),
    ];
    lines.push(Line::from(fit(hint, inner_w)));
    lines
}

// Downloads / Seeding

fn draw_downloads(f: &mut Frame, area: Rect, app: &App, seeding: bool) {
    let torrents = app.visible_torrents();
    let focused = app.region == Region::Content;
    let inner_w = area.width.saturating_sub(4) as usize;
    let inner_h = area.height.saturating_sub(2) as usize;
    let mut lines: Vec<Line<'static>> = Vec::new();

    if torrents.is_empty() {
        let msg = if seeding {
            "Nothing seeding right now."
        } else {
            "No downloads yet. Find something and press d to grab it."
        };
        lines.push(Line::from(vec![Span::styled(
            msg,
            Style::new().add_modifier(Modifier::DIM),
        )]));
    } else if seeding {
        let start = window_start(app.dl_cursor, torrents.len(), inner_h);
        for (i, t) in torrents.iter().enumerate().skip(start).take(inner_h) {
            let here = i == app.dl_cursor && focused;
            lines.push(Line::from(fit(seed_row(t, inner_w, here), inner_w)));
        }
    } else {
        // Two rows per active item: name + progress.
        let items = inner_h / 2;
        let start = window_start(app.dl_cursor, torrents.len(), items.max(1));
        for (i, t) in torrents.iter().enumerate().skip(start).take(items.max(1)) {
            let here = i == app.dl_cursor && focused;
            lines.push(Line::from(fit(dl_name_row(t, inner_w, here), inner_w)));
            lines.push(Line::from(fit(
                dl_progress_row(t, inner_w, here, app.tick),
                inner_w,
            )));
        }
    }

    let count = if torrents.is_empty() {
        None
    } else {
        Some(torrents.len())
    };
    draw_panel(
        f,
        area,
        if seeding { "seeding" } else { "downloads" },
        count,
        focused,
        lines,
    );
}

fn status_color(t: &TorrentView) -> Color {
    match t.status {
        Status::Failed => theme::BAD,
        Status::Paused | Status::Queued => theme::PAUSED,
        _ => theme::ACCENT,
    }
}

fn status_icon(t: &TorrentView) -> (&'static str, Color) {
    match t.status {
        Status::Failed => (icon::ERROR, theme::BAD),
        Status::Paused => (icon::PAUSE, theme::PAUSED),
        Status::Queued => (icon::PENDING, theme::PAUSED),
        _ => (icon::DOWN, theme::ACCENT),
    }
}

fn dl_name_row(t: &TorrentView, inner_w: usize, here: bool) -> Vec<Span<'static>> {
    let dim = Style::new().add_modifier(Modifier::DIM);
    let bold = Style::new().add_modifier(Modifier::BOLD);
    let name_w = inner_w.saturating_sub(MARK as usize + GUTTER as usize + 1 + 10);
    let mut out = Vec::new();
    if here {
        out.push(Span::styled(
            format!("{} ", icon::POINTER),
            Style::new().fg(theme::ACCENT).add_modifier(Modifier::BOLD),
        ));
    } else {
        out.push(Span::raw(" ".repeat(MARK as usize)));
    }
    let (ic, ic_color) = status_icon(t);
    out.push(Span::styled(format!("{ic} "), Style::new().fg(ic_color)));
    let name_style = if here {
        Style::new().fg(theme::ACCENT).add_modifier(Modifier::BOLD)
    } else {
        dim
    };
    out.push(Span::styled(cut(&clean_text(&t.name), name_w), name_style));
    out.push(Span::raw(" "));
    let size = if t.total_bytes > 0 {
        human_bytes(t.total_bytes)
    } else {
        "-".into()
    };
    out.push(Span::styled(
        pad_left(&cut(&size, 10), 10),
        if here { bold } else { dim },
    ));
    out
}

fn dl_progress_row(t: &TorrentView, inner_w: usize, here: bool, tick: u64) -> Vec<Span<'static>> {
    let bar_w = ((inner_w as f32 * 0.4) as usize).clamp(8, 28);
    let stats_w = inner_w.saturating_sub(4 + bar_w + 2);
    let dim = Style::new().add_modifier(Modifier::DIM);
    let mut out = vec![Span::raw("    ")];
    let animate = t.status == Status::Downloading;
    out.extend(progress_bar(
        t.progress,
        bar_w as u16,
        status_color(t),
        animate,
        tick,
    ));
    out.push(Span::raw("  "));
    let stats = match t.status {
        Status::Downloading => {
            let pct = (t.progress * 100.0).round();
            let speed = t.download_mbps.map(|m| m * 1024.0 * 1024.0);
            format!(
                "{}%  {}  {}{}",
                pct,
                human_speed(speed),
                icon::PEER,
                t.peers
            )
        }
        Status::Paused => format!("paused  {}", (t.progress * 100.0).round()),
        Status::Queued => format!("queued  {}", (t.progress * 100.0).round()),
        Status::Failed => t
            .error
            .as_deref()
            .map(|e| truncate(e, 28))
            .unwrap_or_else(|| "failed".into()),
        Status::Completed => String::new(),
    };
    let stats_style = if here {
        Style::new().fg(theme::TEXT)
    } else {
        dim
    };
    out.push(Span::styled(cut(&stats, stats_w), stats_style));
    out
}

fn seed_row(t: &TorrentView, inner_w: usize, here: bool) -> Vec<Span<'static>> {
    let dim = Style::new().add_modifier(Modifier::DIM);
    let bold = Style::new().add_modifier(Modifier::BOLD);
    let name_w = inner_w.saturating_sub(MARK as usize + GUTTER as usize + 1 + 10 + 1 + 12 + 1 + 6);
    let mut out = Vec::new();
    if here {
        out.push(Span::styled(
            format!("{} ", icon::POINTER),
            Style::new().fg(theme::ACCENT).add_modifier(Modifier::BOLD),
        ));
    } else {
        out.push(Span::raw(" ".repeat(MARK as usize)));
    }
    let check_style = if here {
        Style::new().fg(theme::GOOD).add_modifier(Modifier::BOLD)
    } else {
        Style::new().fg(theme::GOOD).add_modifier(Modifier::DIM)
    };
    out.push(Span::styled(format!("{} ", icon::DONE), check_style));
    let name_style = if here {
        Style::new().fg(theme::ACCENT).add_modifier(Modifier::BOLD)
    } else {
        dim
    };
    out.push(Span::styled(cut(&clean_text(&t.name), name_w), name_style));
    out.push(Span::raw(" "));
    let size = if t.total_bytes > 0 {
        human_bytes(t.total_bytes)
    } else {
        "-".into()
    };
    out.push(Span::styled(
        pad_left(&cut(&size, 10), 10),
        if here { bold } else { dim },
    ));
    out.push(Span::raw(" "));
    let up = t.upload_mbps.map(|m| m * 1024.0 * 1024.0);
    let up_str = if up.is_some_and(|u| u > 0.0) {
        format!("{} {}", icon::UP, human_speed(up))
    } else {
        "-".into()
    };
    out.push(Span::styled(
        pad_left(&cut(&up_str, 12), 12),
        if here { bold } else { dim },
    ));
    out.push(Span::raw(" "));
    out.push(Span::styled(
        pad_left(&format!("{}{}", icon::PEER, t.peers), 6),
        if here { bold } else { dim },
    ));
    out
}

/// Filled `█` gradient cells with a sheen sweep when animating, then `░` track.
fn progress_bar(
    pct: f32,
    width: u16,
    color: Color,
    animate: bool,
    tick: u64,
) -> Vec<Span<'static>> {
    let width = width.max(1) as usize;
    let filled = (pct.clamp(0.0, 1.0) * width as f32).round() as usize;
    let mut out: Vec<Span<'static>> = Vec::new();
    if filled > 0 {
        let denom = (width - 1).max(1) as f32;
        let deep = theme::lerp_rgb(color, Color::Rgb(0, 0, 0), 0.3);
        let bright = theme::lerp_rgb(color, theme::TEXT, 0.35);
        let (ramp_deep, ramp_mid, ramp_bright) = if animate {
            (theme::DEEP, theme::ACCENT, theme::BRIGHT)
        } else {
            (deep, color, bright)
        };
        let period = ((width as f32 + SHEEN_RADIUS * 2.0).ceil() as usize) + SHEEN_GAP;
        let center = ((tick as f32 * SHEEN_SPEED) % period as f32) - SHEEN_RADIUS;
        let mut last: Option<Color> = None;
        let mut run = 0usize;
        for i in 0..filled {
            let t = i as f32 / denom;
            let mut c = theme::ramp(t, ramp_deep, ramp_mid, ramp_bright);
            if animate {
                let d = (i as f32 - center).abs();
                if d < SHEEN_RADIUS {
                    let intensity =
                        0.5 * (1.0 + (std::f32::consts::PI * d / SHEEN_RADIUS).cos()) * SHEEN_MAX;
                    c = theme::lerp_rgb(c, theme::SHEEN_PEAK, intensity);
                }
            }
            match last {
                Some(prev) if prev == c => run += 1,
                _ => {
                    if let Some(prev) = last {
                        out.push(Span::styled("█".repeat(run), Style::new().fg(prev)));
                    }
                    last = Some(c);
                    run = 1;
                }
            }
        }
        if let Some(prev) = last {
            out.push(Span::styled("█".repeat(run), Style::new().fg(prev)));
        }
    }
    let empty = width - filled;
    if empty > 0 {
        out.push(Span::styled(
            "░".repeat(empty),
            Style::new().fg(theme::RULE),
        ));
    }
    out
}

// Footer + help

fn footer_hints(app: &App) -> Vec<(&'static str, &'static str)> {
    if app.region == Region::Sidebar {
        return vec![
            ("↑↓←→", "Move"),
            ("↵", "Open"),
            ("tab", "Switch"),
            ("?", "Keys"),
            ("q", "Quit"),
        ];
    }
    if app.is_search_section() {
        return match app.mode {
            SearchMode::Editing => vec![("↵", "Search"), ("esc", "Back")],
            SearchMode::Detail => vec![
                ("d", "Download"),
                ("esc", "Back"),
                ("tab", "Switch"),
                ("?", "Keys"),
            ],
            SearchMode::List => vec![
                ("↑↓←→", "Move"),
                ("↵", "Details"),
                ("d", "Download"),
                ("s", "Sort"),
                ("/", "Search"),
                ("tab", "Switch"),
                ("?", "Keys"),
            ],
        };
    }
    // r (refresh) is omitted from the footer: the list auto-refreshes every
    // 2s and via SSE, and the footer must fit narrow terminals.
    let mut hints = vec![
        ("↑↓←→", "Move"),
        ("p", "Pause"),
        ("x", "Remove"),
        ("P", "Play"),
        ("tab", "Switch"),
        ("?", "Keys"),
    ];
    if app.section == Section::Downloads {
        hints.insert(3, ("D", "Delete"));
    }
    hints
}

fn draw_footer(f: &mut Frame, area: Rect, cols: u16, app: &App) {
    // The help hint is reserved from truncation — it is the one users need
    // most, and the footer can outgrow narrow terminals as hints accrue.
    let mut hints = footer_hints(app);
    let help = hints.iter().position(|(k, _)| *k == "?").map(|i| hints.remove(i));

    let dim = Style::new().add_modifier(Modifier::DIM);
    let mut spans: Vec<Span<'static>> = Vec::new();
    for (i, (keys, label)) in hints.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled("   ", dim));
        }
        spans.push(Span::styled(*keys, Style::new().fg(theme::ALT)));
        spans.push(Span::styled(format!(" {label}"), dim));
    }
    let w = (cols.saturating_sub(2)) as usize;
    let help_cells = if help.is_some() { 9 } else { 0 }; // "   ? Keys"
    let mut fitted = truncate_spans(spans, w.saturating_sub(help_cells));
    if let Some((keys, label)) = help {
        fitted.push(Span::styled("   ", dim));
        fitted.push(Span::styled(keys, Style::new().fg(theme::ALT)));
        fitted.push(Span::styled(format!(" {label}"), dim));
    }
    while str_w_of(&fitted) < w {
        fitted.push(Span::raw(" "));
    }
    f.render_widget(
        Paragraph::new(Line::from(fitted)).style(Style::new().bg(theme::BG)),
        Rect {
            x: 1,
            y: area.y,
            width: cols.saturating_sub(2),
            height: 1,
        },
    );
}

/// Total cell width of a span list.
fn str_w_of(spans: &[Span<'_>]) -> usize {
    spans.iter().map(|s| str_w(&s.content)).sum()
}

/// Truncate spans to `w` cells without padding (fit() pads; callers that
/// append reserved hints need the raw truncation).
fn truncate_spans(spans: Vec<Span<'static>>, w: usize) -> Vec<Span<'static>> {
    let mut out: Vec<Span<'static>> = Vec::new();
    let mut used = 0usize;
    for sp in spans {
        if used >= w {
            break;
        }
        let n = str_w(&sp.content);
        if n <= w - used {
            out.push(sp);
            used += n;
        } else {
            let cut: String = sp.content.chars().take(w - used).collect();
            out.push(Span::styled(cut, sp.style));
            break;
        }
    }
    out
}

struct HelpGroup {
    title: &'static str,
    hints: &'static [(&'static str, &'static str)],
}

const HELP_GROUPS: [HelpGroup; 3] = [
    HelpGroup {
        title: "Navigate",
        hints: &[
            ("↑↓←→ / hjkl", "Navigate panes and lists"),
            ("↵", "Open"),
            ("tab", "Switch pane"),
            ("esc", "Back"),
            ("?", "Keys"),
            ("q", "Quit"),
        ],
    },
    HelpGroup {
        title: "Search",
        hints: &[
            ("/", "Edit search"),
            ("↵", "Open details"),
            ("d", "Download"),
            ("s", "Sort results"),
            ("esc", "Back"),
        ],
    },
    HelpGroup {
        title: "Downloads",
        hints: &[
            ("p", "Pause/resume"),
            ("x", "Remove from queue"),
            ("D", "Remove and delete files"),
            ("r", "Refresh"),
            ("P", "Play in player"),
        ],
    },
];

/// Render a help group as lines: a bold title, then key/label rows.
fn build_group(g: &HelpGroup, key_w: usize) -> Vec<Line<'static>> {
    let mut out = vec![Line::from(vec![Span::styled(
        format!("  {}", g.title),
        Style::new().add_modifier(Modifier::BOLD),
    )])];
    for (keys, label) in g.hints {
        out.push(Line::from(vec![
            Span::styled(pad_right(keys, key_w), Style::new().fg(theme::ALT)),
            Span::styled(*label, Style::new().add_modifier(Modifier::DIM)),
        ]));
    }
    out
}

/// Merge two help columns into rows, right-padding the left to `offset` cells.
fn merge_columns(
    left: Vec<Line<'static>>,
    right: Vec<Line<'static>>,
    offset: usize,
) -> Vec<Line<'static>> {
    let rows = left.len().max(right.len());
    let mut out = Vec::with_capacity(rows);
    for i in 0..rows {
        let l = left.get(i).cloned().unwrap_or_default();
        let r = right.get(i).cloned().unwrap_or_default();
        let mut spans = fit(l.spans.to_vec(), offset);
        spans.extend(r.spans.to_vec());
        out.push(Line::from(spans));
    }
    out
}

fn draw_help(f: &mut Frame, body: Rect, cols: u16) {
    let border = theme::lerp_rgb(theme::ACCENT, theme::RULE, 0.55);
    let dim = Style::new().add_modifier(Modifier::DIM);
    let alt = Style::new().fg(theme::ALT);
    let key_w = 12usize;

    let two_col = cols >= 76;
    let mut left: Vec<Line<'static>> = vec![Line::from(vec![Span::styled(
        " Keyboard",
        Style::new().fg(theme::ACCENT).add_modifier(Modifier::BOLD),
    )])];
    left.push(Line::from(vec![Span::raw(" ")]));
    left.extend(build_group(&HELP_GROUPS[0], key_w));
    left.push(Line::from(vec![Span::raw(" ")]));
    left.extend(build_group(&HELP_GROUPS[1], key_w));

    let mut right: Vec<Line<'static>> = vec![
        Line::from(vec![Span::raw(" ")]),
        Line::from(vec![Span::raw(" ")]),
    ];
    right.extend(build_group(&HELP_GROUPS[2], key_w));

    let mut content: Vec<Line<'static>> = if two_col {
        merge_columns(left, right, 37)
    } else {
        left.push(Line::from(vec![Span::raw(" ")]));
        left.extend(build_group(&HELP_GROUPS[2], key_w));
        left
    };
    if two_col {
        content.push(Line::from(vec![Span::styled(
            "The daemon keeps downloading while the TUI is closed.",
            dim,
        )]));
        content.push(Line::from(vec![
            Span::styled("Press ", dim),
            Span::styled("?", alt),
            Span::styled(" or ", dim),
            Span::styled("esc", alt),
            Span::styled(" to close", dim),
        ]));
    } else {
        content.push(Line::from(vec![
            Span::styled("Press ", dim),
            Span::styled("?", alt),
            Span::styled(" or ", dim),
            Span::styled("esc", alt),
            Span::styled(" to close", dim),
        ]));
    }

    let card_w = if two_col { 78 } else { 52 };
    let card_w = card_w.min(cols.saturating_sub(2).max(20));
    let card_h = (content.len() + 2).min(body.height.max(12) as usize) as u16;
    let area = Rect {
        x: body.x + 1,
        y: body.y,
        width: card_w,
        height: card_h,
    };

    let w = area.width as usize;
    let bstyle = Style::new().fg(border);
    let mut framed: Vec<Line<'static>> = Vec::with_capacity(area.height as usize);
    framed.push(Line::from(vec![
        Span::styled("╭", bstyle),
        Span::styled("─".repeat(w.saturating_sub(2)), bstyle),
        Span::styled("╮", bstyle),
    ]));
    for i in 0..(area.height.saturating_sub(2)) {
        let line = content.get(i as usize).cloned().unwrap_or_default();
        let mut row = vec![Span::styled("│ ", bstyle)];
        row.extend(fit(line.spans.to_vec(), w.saturating_sub(4)));
        row.push(Span::styled(" │", bstyle));
        framed.push(Line::from(row));
    }
    framed.push(Line::from(vec![
        Span::styled("╰", bstyle),
        Span::styled("─".repeat(w.saturating_sub(2)), bstyle),
        Span::styled("╯", bstyle),
    ]));
    f.render_widget(
        Paragraph::new(framed).style(Style::new().bg(theme::BG)),
        area,
    );
}

// Splash

fn draw_splash(f: &mut Frame, app: &mut App) {
    let area = f.area();
    let cols = area.width;
    let rows = area.height;
    let bar_w = 24u16.max(cols.saturating_sub(6).min(62));
    let show_logo = cols >= logo::LOGO_WIDTH as u16 + 2;

    let h = if show_logo {
        3 + 2 + 1 + 1 + 1 + 3 + 1 + 1
    } else {
        1 + 2 + 1 + 1 + 1 + 3 + 1 + 1
    };
    let mut y = (rows.saturating_sub(h)) / 2;
    let center_x = |w: u16| area.x + area.width.saturating_sub(w) / 2;

    let line_at = |f: &mut Frame, y: u16, line: Line<'static>| {
        let w = line.width() as u16;
        f.render_widget(
            Paragraph::new(line),
            Rect {
                x: center_x(w),
                y,
                width: w,
                height: 1,
            },
        );
    };

    if show_logo {
        let w = logo::LOGO_WIDTH as u16;
        f.render_widget(
            Paragraph::new(logo::render()),
            Rect {
                x: center_x(w),
                y,
                width: w,
                height: 3,
            },
        );
        y += 3 + 2;
    } else {
        line_at(
            f,
            y,
            Line::from(vec![Span::styled(
                "torq",
                Style::new().fg(theme::ACCENT).add_modifier(Modifier::BOLD),
            )]),
        );
        y += 1 + 2;
    }
    line_at(
        f,
        y,
        Line::from(vec![Span::styled(
            "A curated, terminal-native torrent downloader.",
            Style::new().fg(theme::TEXT),
        )]),
    );
    y += 1;
    line_at(
        f,
        y,
        Line::from(vec![Span::styled(
            "games · movies · tv · anime",
            Style::new().add_modifier(Modifier::DIM),
        )]),
    );
    y += 2;

    let bar_rect = Rect {
        x: center_x(bar_w),
        y,
        width: bar_w,
        height: 3,
    };
    draw_search_bar(f, bar_rect, app);
    y += 4;

    let dim = Style::new().add_modifier(Modifier::DIM);
    let alt = Style::new().fg(theme::ALT);
    let hints = vec![
        Span::styled("↵", alt),
        Span::styled(" search", dim),
        Span::styled(format!("  {}  ", icon::DOT), dim),
        Span::styled("⇥", alt),
        Span::styled(" browse", dim),
        Span::styled(format!("  {}  ", icon::DOT), dim),
        Span::styled("^c", alt),
        Span::styled(" quit", dim),
    ];
    line_at(f, y, Line::from(hints));
}

// Shared helpers

/// Draw a rounded panel: `╭─ Title (n) ─...─╮` frame with `│ ` insets.
/// `lines` are clipped/padded to the available inner rows.
fn draw_panel(
    f: &mut Frame,
    area: Rect,
    title: &str,
    count: Option<usize>,
    focused: bool,
    lines: Vec<Line<'static>>,
) {
    let w = area.width;
    let h = area.height;
    if w < 4 || h < 2 {
        return;
    }
    let border = if focused { theme::ACCENT } else { theme::RULE };
    let bstyle = Style::new().fg(border);
    // Panel titles are capitalized ("details" → "Details").
    let mut chars = title.chars();
    let cap: String = match chars.next() {
        Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    };
    let label = match count {
        Some(n) => format!("{cap} ({n})"),
        None => cap,
    };
    let mut out: Vec<Line<'static>> = Vec::with_capacity(h as usize);
    let fill = (w as usize).saturating_sub(5 + str_w(&label));
    out.push(Line::from(vec![
        Span::styled("╭─ ", bstyle),
        Span::styled(label, bstyle.add_modifier(Modifier::BOLD)),
        Span::styled(format!(" {}", "─".repeat(fill)), bstyle),
        Span::styled("╮", bstyle),
    ]));
    let inner_w = (w - 4) as usize;
    let inner_h = (h - 2) as usize;
    for i in 0..inner_h {
        let content = lines.get(i).cloned().unwrap_or_default();
        let mut row = vec![Span::styled("│ ", bstyle)];
        row.extend(fit(content.spans.to_vec(), inner_w));
        row.push(Span::styled(" │", bstyle));
        out.push(Line::from(row));
    }
    out.push(Line::from(vec![Span::styled(
        format!("╰{}╯", "─".repeat(w.saturating_sub(2) as usize)),
        bstyle,
    )]));
    f.render_widget(Paragraph::new(out).style(Style::new().bg(theme::BG)), area);
}

/// Truncate a span run to `w` cells (wide glyphs count 2), preserving styles,
/// then right-pad with unstyled spaces so the row is exactly `w` cells.
fn fit(spans: Vec<Span<'static>>, w: usize) -> Vec<Span<'static>> {
    let mut out: Vec<Span<'static>> = Vec::new();
    let mut used = 0usize;
    for sp in spans {
        if used >= w {
            break;
        }
        let n = str_w(&sp.content);
        if n <= w - used {
            out.push(sp);
            used += n;
        } else {
            let cut: String = sp.content.chars().take(w - used).collect();
            out.push(Span::styled(cut, sp.style));
            used = w;
            break;
        }
    }
    while used < w {
        out.push(Span::raw(" "));
        used += 1;
    }
    out
}

fn pad_left(s: &str, w: usize) -> String {
    let n = str_w(s);
    if n >= w {
        s.to_string()
    } else {
        format!("{}{}", " ".repeat(w - n), s)
    }
}

fn pad_right(s: &str, w: usize) -> String {
    let n = str_w(s);
    if n >= w {
        s.to_string()
    } else {
        format!("{}{}", s, " ".repeat(w - n))
    }
}

/// Cell width of a string (wide/combining glyphs handled by unicode-width).
fn str_w(s: &str) -> usize {
    s.width()
}

/// Scroll window that keeps the cursor on screen.
fn window_start(cursor: usize, len: usize, height: usize) -> usize {
    if len <= height {
        0
    } else {
        cursor.saturating_sub(height - 1).min(len - height)
    }
}

#[cfg(test)]
mod render_tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use torq_core::daemon::{Status, TorrentView};
    use torq_sources::TorrentResult;

    fn result(name: &str, source: &str, size: u64, seeders: u32, leechers: u32) -> TorrentResult {
        TorrentResult {
            info_hash: format!("hash-{name}"),
            name: name.to_string(),
            size_bytes: size,
            seeders,
            leechers,
            num_files: Some(3),
            source: source.to_string(),
            magnet: format!("magnet:?xt=urn:btih:{name}"),
            added: Some(1_700_000_000),
        }
    }

    fn view(
        id: usize,
        status: Status,
        progress: f32,
        down: Option<f32>,
        up: Option<f32>,
    ) -> TorrentView {
        TorrentView {
            id,
            info_hash: format!("ih-{id}"),
            name: format!("Torrent {id}"),
            status,
            progress,
            total_bytes: 7_820_000_000,
            downloaded_bytes: (7_820_000_000.0 * progress) as u64,
            upload_mbps: up,
            download_mbps: down,
            peers: 41,
            error: None,
            added_at: 1_700_000_000,
        }
    }

    fn frame(app: &mut App, w: u16, h: u16) -> Buffer {
        let backend = TestBackend::new(w, h);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|f| draw(f, app)).expect("draw");
        terminal.backend().buffer().clone()
    }

    /// Row `y` as a string of cells from `from` to `to` (inclusive).
    fn row(buf: &Buffer, y: u16, from: u16, to: u16) -> String {
        (from..=to)
            .map(|x| buf.cell((x, y)).map(|c| c.symbol()).unwrap_or(" "))
            .collect()
    }

    #[test]
    fn splash_is_centered_with_logo_search_and_hints() {
        let mut app = App::new("http://127.0.0.1:8765".into());
        let buf = frame(&mut app, 80, 24);

        // Wordmark centered: 17 cells wide → x = (80-17)/2 = 31; rows carry
        // a leading space, so the first glyph sits at 32/33.
        assert_eq!(row(&buf, 6, 31, 46), " ▀█▀ █▀█ █▀█ █▀█");
        assert_eq!(row(&buf, 7, 31, 47), "  █  █▄█ █▀▄ █▄█▀");
        assert_eq!(buf.cell((45, 5)).map(|c| c.symbol()), Some("𐓏"));

        // Each text element centers itself.
        assert_eq!(
            row(&buf, 10, 17, 62),
            "A curated, terminal-native torrent downloader."
        );
        assert_eq!(row(&buf, 11, 26, 52), "games · movies · tv · anime");

        // Search panel (62 wide, centered at x=9) with the placeholder.
        assert_eq!(
            row(&buf, 13, 9, 70),
            "╭─ Search ───────────────────────────────────────────────────╮"
        );
        assert_eq!(
            row(&buf, 14, 9, 70),
            "│ ❯ Search or paste a magnet link…                           │"
        );
        assert_eq!(
            row(&buf, 15, 9, 70),
            "╰────────────────────────────────────────────────────────────╯"
        );

        // Hints row centered.
        assert_eq!(row(&buf, 17, 23, 55), "↵ search  ·  ⇥ browse  ·  ^c quit");
    }

    #[test]
    fn browser_results_layout_is_pixel_exact() {
        let mut app = App::new("http://127.0.0.1:8765".into());
        app.view = View::Browser;
        app.region = Region::Content;
        app.results = vec![
            result("Dune: Part Two (2024)", "yts", 7_820_000_000, 1240, 88),
            result("Oppenheimer (2023)", "eztv", 1_960_000_000, 540, 31),
        ];
        let buf = frame(&mut app, 80, 24);

        // Logo at the top, rule under it, all inside the 1-col padding.
        assert_eq!(row(&buf, 1, 1, 16), " ▀█▀ █▀█ █▀█ █▀█");
        assert_eq!(row(&buf, 3, 1, 78), "─".repeat(78));

        // Sidebar rail (16 wide): selected "All" gets the bar; groups split.
        assert_eq!(row(&buf, 5, 1, 16), "▌ All           ");
        assert_eq!(row(&buf, 6, 1, 16), "  Games         ");
        assert_eq!(row(&buf, 7, 1, 16), "  Movies        ");
        assert_eq!(row(&buf, 8, 1, 16), "  TV            ");
        assert_eq!(row(&buf, 9, 1, 16), "  Anime         ");
        assert_eq!(row(&buf, 10, 1, 16), "                ");
        assert_eq!(row(&buf, 11, 1, 16), "  Downloads     ");
        assert_eq!(row(&buf, 12, 1, 16), "  Seeding       ");

        // Content panel is 61 wide at x=18..78.
        assert_eq!(
            row(&buf, 5, 18, 78),
            "╭─ Search ──────────────────────────────────────────────────╮"
        );
        assert_eq!(
            row(&buf, 6, 18, 78),
            "│ ❯ Search or paste a magnet link…                          │"
        );
        assert_eq!(
            row(&buf, 7, 18, 78),
            "╰───────────────────────────────────────────────────────────╯"
        );
        assert_eq!(
            row(&buf, 9, 18, 78),
            "╭─ Latest (2) ──────────────────────────────────────────────╮"
        );
        assert_eq!(
            row(&buf, 10, 18, 78),
            "│ newest across all sources                                 │"
        );
        // Bordered table: header, separator, rows with `│` column dividers.
        assert_eq!(
            row(&buf, 11, 18, 78),
            "│ Name                      │      Size│  Seeds│  Lch│  Src │"
        );
        assert_eq!(
            row(&buf, 12, 18, 78),
            "│ ──────────────────────────┼──────────┼───────┼─────┼───── │"
        );
        // Selected row carries the ❯ pointer; numbers right-aligned per cell.
        assert_eq!(
            row(&buf, 13, 18, 78),
            "│ ❯ Dune: Part Two (2024)   │   7.28 GB│   1240│   88│  YTS │"
        );
        assert_eq!(
            row(&buf, 14, 18, 78),
            "│   Oppenheimer (2023)      │   1.83 GB│    540│   31│ EZTV │"
        );
        assert_eq!(
            row(&buf, 22, 18, 78),
            "╰───────────────────────────────────────────────────────────╯"
        );

        // Footer hints for the search list mode.
        // Footer sits inside the 1-col padding (x=1..78).
        assert_eq!(
            row(&buf, 23, 1, 78).trim_end(),
            "↑↓←→ Move   ↵ Details   d Download   s Sort   / Search   tab Switch   ? Keys"
        );
    }

    #[test]
    fn downloads_show_icon_progress_bar_and_stats() {
        let mut app = App::new("http://127.0.0.1:8765".into());
        app.view = View::Browser;
        app.section = Section::Downloads;
        app.region = Region::Content;
        app.torrents = vec![view(1, Status::Downloading, 0.64, Some(7.7), None)];
        let buf = frame(&mut app, 80, 24);

        assert_eq!(
            row(&buf, 5, 18, 78),
            "╭─ Downloads (1) ───────────────────────────────────────────╮"
        );
        // Name row: pointer + down icon + name + right-aligned size.
        assert_eq!(
            row(&buf, 6, 18, 78),
            "│ ❯ ↓ Torrent 1    7.28 GB                                  │"
        );
        // Progress row: 14 filled cells, 8 track cells, then right stats.
        assert_eq!(
            row(&buf, 7, 18, 78),
            "│     ██████████████░░░░░░░░  64%  7.7 MB/s  •41            │"
        );
        // Sidebar badge counts the active item; footer shows the delete hint.
        assert_eq!(row(&buf, 11, 1, 16), "▌ Downloads (1) ");
        assert_eq!(
            row(&buf, 23, 1, 78).trim_end(),
            "↑↓←→ Move   p Pause   x Remove   D Delete   P Play   tab Switch   ? Keys"
        );
    }

    #[test]
    fn seeding_section_lists_completed_with_check() {
        let mut app = App::new("http://127.0.0.1:8765".into());
        app.view = View::Browser;
        app.section = Section::Seeding;
        app.region = Region::Content;
        app.torrents = vec![view(9, Status::Completed, 1.0, None, Some(2.5))];
        let buf = frame(&mut app, 80, 24);

        assert_eq!(
            row(&buf, 5, 18, 78),
            "╭─ Seeding (1) ─────────────────────────────────────────────╮"
        );
        assert_eq!(
            row(&buf, 6, 18, 78),
            "│ ❯ ✓ Torrent 9    7.28 GB   ↑ 2.5 MB/s    •41              │"
        );
        assert_eq!(
            row(&buf, 23, 1, 78).trim_end(),
            "↑↓←→ Move   p Pause   x Remove   P Play   tab Switch   ? Keys"
        );
    }

    #[test]
    fn detail_view_pins_fields_and_actions() {
        let mut app = App::new("http://127.0.0.1:8765".into());
        app.view = View::Browser;
        app.region = Region::Content;
        app.results = vec![result(
            "Dune: Part Two (2024)",
            "yts",
            7_820_000_000,
            1240,
            88,
        )];
        app.mode = SearchMode::Detail;
        app.detail = app.results.first().cloned();
        let buf = frame(&mut app, 80, 24);

        assert_eq!(
            row(&buf, 9, 18, 78),
            "╭─ Details ─────────────────────────────────────────────────╮"
        );
        assert_eq!(
            row(&buf, 10, 18, 78),
            "│ Dune: Part Two (2024)  YTS                                │"
        );
        assert_eq!(
            row(&buf, 11, 18, 78),
            "│ ───────────────────────────────────────────────────────── │"
        );
        assert_eq!(
            row(&buf, 12, 18, 78),
            "│                                                           │"
        );
        // Labels left-aligned in a 9-wide box; values follow.
        assert_eq!(
            row(&buf, 13, 18, 78),
            "│ Size     7.28 GB                                          │"
        );
        assert_eq!(
            row(&buf, 14, 18, 78),
            "│ Health   1240 seeders · 88 leechers                       │"
        );
        assert_eq!(
            row(&buf, 15, 18, 78),
            "│ Files    3                                                │"
        );
        assert_eq!(
            row(&buf, 17, 18, 78),
            "│ Hash     hash-Dune: Part Two (2024)                       │"
        );
        assert_eq!(
            row(&buf, 18, 18, 78),
            "│ Magnet   magnet:?xt=urn:btih:Dune: Part Two (2024)        │"
        );
        assert_eq!(
            row(&buf, 20, 18, 78),
            "│ d Download  ·  esc back                                   │"
        );
        assert_eq!(
            row(&buf, 23, 1, 78).trim_end(),
            "d Download   esc Back   tab Switch   ? Keys"
        );
    }

    #[test]
    fn help_replaces_body_with_two_column_card() {
        let mut app = App::new("http://127.0.0.1:8765".into());
        app.view = View::Browser;
        app.help = true;
        let buf = frame(&mut app, 100, 28);

        // Card sits where the body would, inside the 1-col padding: x=1..78.
        let top = format!("╭{}╮", "─".repeat(76));
        let bottom = format!("╰{}╯", "─".repeat(76));
        assert_eq!(row(&buf, 5, 1, 78), top);
        assert_eq!(row(&buf, 24, 1, 78), bottom);
        // Body and footer are replaced, not drawn underneath.
        assert_eq!(row(&buf, 27, 0, 99).trim_end(), "");

        assert_eq!(row(&buf, 6, 3, 11), " Keyboard");
        // Navigate (left) and Downloads (right) groups align on the same row.
        assert_eq!(row(&buf, 8, 3, 12), "  Navigate");
        assert_eq!(row(&buf, 8, 40, 50), "  Downloads");
        assert_eq!(row(&buf, 9, 3, 38), "↑↓←→ / hjkl Navigate panes and lists");
        assert_eq!(row(&buf, 9, 40, 63), "p           Pause/resume");
        assert_eq!(row(&buf, 11, 40, 74), "D           Remove and delete files");
        assert_eq!(row(&buf, 16, 3, 10), "  Search");
        assert_eq!(row(&buf, 17, 3, 25), "/           Edit search");
        // Footer lines inside the card.
        assert!(row(&buf, 22, 3, 62).contains("The daemon keeps downloading"));
        assert_eq!(row(&buf, 23, 3, 30).trim_end(), "Press ? or esc to close");
    }

    #[test]
    fn everforest_colors_are_applied_to_cells() {
        use ratatui::style::Color as C;
        let mut app = App::new("http://127.0.0.1:8765".into());
        app.view = View::Browser;
        app.region = Region::Content;
        app.results = vec![result(
            "Dune: Part Two (2024)",
            "yts",
            7_820_000_000,
            1240,
            88,
        )];
        let buf = frame(&mut app, 80, 24);

        let fg = |x: u16, y: u16| buf.cell((x, y)).and_then(|c| c.style().fg);
        let bg = |x: u16, y: u16| buf.cell((x, y)).and_then(|c| c.style().bg);

        // Canvas is everforest bg_dim everywhere.
        assert_eq!(bg(0, 0), Some(C::Rgb(0x23, 0x2a, 0x2e)));
        // Unfocused panel border is grey0; the rule under the logo too.
        assert_eq!(fg(18, 5), Some(C::Rgb(0x7a, 0x84, 0x78)));
        assert_eq!(fg(1, 3), Some(C::Rgb(0x7a, 0x84, 0x78)));
        // Selected row pointer + name are the everforest green.
        assert_eq!(fg(20, 13), Some(C::Rgb(0xa7, 0xc0, 0x80)));
        // Healthy seed count is aqua (inner content starts at x=20).
        assert_eq!(fg(61, 13), Some(C::Rgb(0x83, 0xc0, 0x92)));
        // Footer key hints are grey1.
        assert_eq!(fg(1, 23), Some(C::Rgb(0x85, 0x92, 0x89)));
        // Table grid lines are grey0.
        assert_eq!(fg(46, 13), Some(C::Rgb(0x7a, 0x84, 0x78)));
    }

    #[test]
    fn narrow_terminal_does_not_panic() {
        let mut app = App::new("http://127.0.0.1:8765".into());
        app.view = View::Browser;
        app.results = vec![result(
            "Some Long Movie Name That Overflows",
            "x1337-movies",
            5_000_000_000,
            2,
            1,
        )];
        let buf = frame(&mut app, 40, 12);
        // Compact mode: no rule row; footer present (rows >= 12).
        assert_eq!(row(&buf, 3, 1, 38), " ".repeat(38));
        assert!(row(&buf, 11, 1, 39).starts_with("↑↓←→ Move"));
    }
}
