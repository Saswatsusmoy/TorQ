//! Rendering: header tabs, search/results, downloads, help overlay.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Row, Table, TableState};

use crate::app::{App, Tab};

pub fn draw(f: &mut Frame, app: &mut App) {
    let [header, body, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(f.area());

    f.render_widget(header_line(app), header);
    match app.tab {
        Tab::Search => draw_search(f, body, app),
        Tab::Downloads => draw_downloads(f, body, app),
    }
    f.render_widget(footer_line(app), footer);

    if app.help {
        draw_help(f);
    }
}

fn header_line(app: &App) -> Paragraph<'static> {
    let search = if app.tab == Tab::Search {
        Span::styled(" search ", Style::new().bg(Color::Blue).fg(Color::Black))
    } else {
        Span::raw(" search ")
    };
    let downloads = if app.tab == Tab::Downloads {
        Span::styled(" downloads ", Style::new().bg(Color::Blue).fg(Color::Black))
    } else {
        Span::raw(" downloads ")
    };
    Paragraph::new(Line::from(vec![
        Span::styled("torq", Style::new().add_modifier(Modifier::BOLD)),
        Span::raw("  "),
        search,
        downloads,
        Span::raw(format!("   {}", app.base)),
    ]))
}

fn draw_search(f: &mut Frame, area: Rect, app: &App) {
    let [input_area, hint, results_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(0),
    ])
    .areas(area);

    f.render_widget(
        Paragraph::new(app.input.as_str()).block(Block::default().borders(Borders::BOTTOM)),
        input_area,
    );
    if app.tab == Tab::Search {
        f.set_cursor_position((input_area.x + app.input.len() as u16, input_area.y));
    }

    let hint_text = if app.input.is_empty() {
        "type to search, Enter to run (empty = browse latest), d = download, 1/2 = tabs, ? = help, q = quit"
    } else if app.searching {
        "searching…"
    } else {
        "Enter to search, Esc to clear, d = download"
    };
    f.render_widget(Paragraph::new(hint_text), hint);

    let rows = app.results.iter().map(|r| {
        Row::new(vec![
            truncate(&r.name, 60),
            human_bytes(r.size_bytes),
            r.seeders.to_string(),
            r.source.clone(),
        ])
    });
    let widths = [
        Constraint::Percentage(60),
        Constraint::Length(10),
        Constraint::Length(6),
        Constraint::Length(14),
    ];
    let table = Table::new(rows, widths)
        .header(Row::new(vec!["name", "size", "seeds", "source"]))
        .row_highlight_style(Style::new().add_modifier(Modifier::REVERSED));
    let mut state = TableState::default().with_selected(app.search_selected);
    f.render_stateful_widget(table, results_area, &mut state);
}

fn draw_downloads(f: &mut Frame, area: Rect, app: &App) {
    let rows = app.torrents.iter().map(|v| {
        Row::new(vec![
            truncate(&v.name, 50),
            status_str(v.status),
            format!("{:5.1}%", v.progress * 100.0),
            speed(v.download_mbps),
            v.peers.to_string(),
        ])
    });
    let widths = [
        Constraint::Percentage(50),
        Constraint::Length(12),
        Constraint::Length(8),
        Constraint::Length(10),
        Constraint::Length(6),
    ];
    let table = Table::new(rows, widths)
        .header(Row::new(vec![
            "name", "status", "progress", "down", "peers",
        ]))
        .row_highlight_style(Style::new().add_modifier(Modifier::REVERSED));
    let mut state = TableState::default().with_selected(app.dl_selected);
    f.render_stateful_widget(table, area, &mut state);
}

fn footer_line(app: &App) -> Paragraph<'static> {
    let mut spans = vec![
        Span::styled(" p ", Style::new().bg(Color::DarkGray)),
        Span::raw(" pause/resume  "),
        Span::styled(" x ", Style::new().bg(Color::DarkGray)),
        Span::raw(" remove  "),
        Span::styled(" r ", Style::new().bg(Color::DarkGray)),
        Span::raw(" refresh"),
    ];
    if let Some(n) = &app.notice {
        spans.push(Span::styled(
            format!("   ⚠ {n}"),
            Style::new().fg(Color::Yellow),
        ));
    } else if !app.offline.is_empty() {
        spans.push(Span::styled(
            format!("   offline: {}", app.offline.join(", ")),
            Style::new().fg(Color::Yellow),
        ));
    }
    Paragraph::new(Line::from(spans))
}

fn draw_help(f: &mut Frame) {
    let area = centered(f.area(), 60, 12);
    let lines = [
        "search:  type query, Enter to search (empty = browse latest)",
        "         d = add selected result, Esc = clear input",
        "downloads: p = pause/resume selected, x = remove",
        "         D = remove and delete files",
        "tabs:    1 search, 2 downloads, Tab cycles",
        "q or Ctrl-C = quit TUI (daemon keeps running)",
        "",
        "? or Esc = close this help",
    ];
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" keys ")
        .style(Style::new().bg(Color::Black));
    f.render_widget(Paragraph::new(lines.join("\n")).block(block), area);
}

fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let x = area.x + area.width.saturating_sub(w) / 2;
    let y = area.y + area.height.saturating_sub(h) / 2;
    Rect {
        x,
        y,
        width: w,
        height: h,
    }
}

fn status_str(s: torq_core::daemon::Status) -> String {
    match s {
        torq_core::daemon::Status::Downloading => "downloading",
        torq_core::daemon::Status::Queued => "queued",
        torq_core::daemon::Status::Paused => "paused",
        torq_core::daemon::Status::Completed => "seeding",
        torq_core::daemon::Status::Failed => "failed",
    }
    .into()
}

fn speed(mbps: Option<f32>) -> String {
    match mbps {
        Some(v) if v >= 0.01 => format!("{v:.1} MiB/s"),
        _ => "-".into(),
    }
}

fn human_bytes(b: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = b as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{b} B")
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let cut: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{cut}…")
    }
}
