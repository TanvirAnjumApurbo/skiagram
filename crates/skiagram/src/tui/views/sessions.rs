//! Level 1: scrollable session list — project, model, tokens, est. cost.
//!
//! Rows come from the drill-down [`SessionDetail`]s (same folded, sub-agent
//! inclusive accounting as `summary`); the header totals come from the
//! [`Summary`]. `Enter` drills into the highlighted session's turns.

use ratatui::layout::{Alignment, Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Row, Table, TableState};
use ratatui::Frame;
use skiagram_core::analysis::aggregate::Summary;
use skiagram_core::analysis::drilldown::SessionDetail;

use crate::render::{banner, fmt_cost, fmt_count};

pub fn draw(
    frame: &mut Frame,
    details: &[SessionDetail],
    summary: &Summary,
    state: &mut TableState,
) {
    let area = frame.area();
    // Show the dot-matrix wordmark atop the landing screen, but only when there's
    // vertical room for it plus the stats header, a usable table, and the footer.
    // It degrades away on short terminals (and never appears once you drill in).
    let banner_h: u16 = if area.height >= 24 {
        banner::WORDMARK_ROWS as u16 + 1 // block rows + tagline
    } else {
        0
    };

    let [banner_area, header_area, body_area, footer_area] = Layout::vertical([
        Constraint::Length(banner_h),
        Constraint::Length(2),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .areas(area);

    if banner_h > 0 {
        frame.render_widget(
            Paragraph::new(banner_lines()).alignment(Alignment::Center),
            banner_area,
        );
    }

    let totals = &summary.totals;
    let header = Paragraph::new(vec![
        Line::from(format!(
            "skiagram · {} · {} session file(s) · {} tokens · {} (deduplicated)",
            summary.agent,
            summary.sessions_parsed,
            fmt_count(totals.total_tokens()),
            fmt_cost(totals.cost_usd),
        )),
        Line::from(format!(
            "requests {} · dup lines collapsed {} · sub-agent tokens {}",
            fmt_count(totals.requests),
            fmt_count(summary.dedup.duplicate_lines_collapsed),
            fmt_count(summary.sidechain_totals.total_tokens()),
        )),
    ]);
    frame.render_widget(header, header_area);

    if details.is_empty() {
        frame.render_widget(
            Paragraph::new("no sessions found").block(Block::default().borders(Borders::ALL)),
            body_area,
        );
    } else {
        let rows = details.iter().map(|d| {
            Row::new(vec![
                d.project.clone().unwrap_or_else(|| "?".into()),
                d.id.chars().take(8).collect::<String>(),
                d.model.clone().unwrap_or_else(|| "?".into()),
                fmt_count(d.rollup.requests),
                fmt_count(d.rollup.total_tokens()),
                fmt_count(d.sub_agents),
                fmt_cost(d.rollup.cost_usd),
            ])
        });
        let table = Table::new(
            rows,
            [
                Constraint::Percentage(26),
                Constraint::Length(9),
                Constraint::Percentage(24),
                Constraint::Length(9),
                Constraint::Length(13),
                Constraint::Length(7),
                Constraint::Length(10),
            ],
        )
        .header(
            Row::new(vec![
                "Project",
                "Session",
                "Model",
                "Requests",
                "Tokens",
                "Agents",
                "Est. cost",
            ])
            .style(Style::default().add_modifier(Modifier::BOLD)),
        )
        .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .highlight_symbol("▶ ")
        .block(Block::default().borders(Borders::ALL).title("sessions"));
        frame.render_stateful_widget(table, body_area, state);
    }

    let footer =
        Paragraph::new("↑/↓ or j/k scroll · g/G home/end · Enter drill into turns · q quit");
    frame.render_widget(footer, footer_area);
}

/// The block wordmark as colored `ratatui` lines: each lit cell gets its
/// red→orange gradient color by column (shared with the CLI banner), spaces stay
/// blank, and the slogan trails dim/italic underneath.
fn banner_lines() -> Vec<Line<'static>> {
    let rows = banner::wordmark_rows();
    let width = rows.first().map_or(1, |r| r.chars().count()).max(1);
    let mut lines: Vec<Line<'static>> = rows
        .iter()
        .map(|row| {
            let spans: Vec<Span> = row
                .chars()
                .enumerate()
                .map(|(i, ch)| {
                    if ch == ' ' {
                        return Span::raw(" ");
                    }
                    let t = if width <= 1 {
                        0.0
                    } else {
                        i as f32 / (width - 1) as f32
                    };
                    let (r, g, b) = banner::gradient_rgb(t);
                    Span::styled(ch.to_string(), Style::default().fg(Color::Rgb(r, g, b)))
                })
                .collect();
            Line::from(spans)
        })
        .collect();
    lines.push(Line::from(Span::styled(
        banner::TAGLINE,
        Style::default()
            .fg(Color::Rgb(150, 150, 160))
            .add_modifier(Modifier::ITALIC),
    )));
    lines
}
