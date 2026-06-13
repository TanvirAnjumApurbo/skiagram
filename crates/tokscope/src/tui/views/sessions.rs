//! Scrollable session list: project, model, tokens, est. cost.

use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Paragraph, Row, Table, TableState};
use ratatui::Frame;
use tokscope_core::analysis::aggregate::Summary;

use crate::render::{fmt_cost, fmt_count};

pub fn draw(frame: &mut Frame, summary: &Summary, state: &mut TableState) {
    let [header_area, body_area, footer_area] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    let totals = &summary.totals;
    let header = Paragraph::new(vec![
        Line::from(format!(
            "tokscope · {} · {} session file(s) · {} tokens · {} (deduplicated)",
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

    if summary.by_session.is_empty() {
        frame.render_widget(
            Paragraph::new("no sessions found").block(Block::default().borders(Borders::ALL)),
            body_area,
        );
    } else {
        let rows = summary.by_session.iter().map(|s| {
            Row::new(vec![
                s.project.clone().unwrap_or_else(|| "?".into()),
                s.id.chars().take(8).collect::<String>(),
                s.model.clone().unwrap_or_else(|| "?".into()),
                fmt_count(s.rollup.requests),
                fmt_count(s.rollup.total_tokens()),
                fmt_count(s.sub_agents),
                fmt_cost(s.rollup.cost_usd),
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
        Paragraph::new("↑/↓ or j/k scroll · g/G home/end · Enter drill-down (TODO v0.2) · q quit");
    frame.render_widget(footer, footer_area);
}
