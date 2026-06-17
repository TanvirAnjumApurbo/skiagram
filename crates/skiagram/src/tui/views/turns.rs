//! Level 2: the deduplicated turns of one drilled session.
//!
//! One row per API request (post-dedup, CLAUDE.md §8.1), navigable via
//! `state`; `Enter` descends to the context view, `Esc`/`Backspace` returns to
//! the session list.

use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Paragraph, Row, Table, TableState};
use ratatui::Frame;
use skiagram_core::analysis::drilldown::SessionDetail;

use crate::render::{fmt_cost, fmt_count};

/// Truncate `s` to at most `max` chars, char-safe (no panics on multi-byte
/// boundaries).
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max).collect()
    }
}

/// `Some(n)` -> thousands-separated count; `None` -> `"?"` (absence ≠ zero,
/// CLAUDE.md §8.5).
fn fmt_known(n: Option<u64>) -> String {
    n.map(fmt_count).unwrap_or_else(|| "?".into())
}

/// `HH:MM:SS` from the RFC-3339 `Display` of a `Timestamp` (e.g.
/// `2026-06-02T10:00:00Z` -> `10:00:00`); `"—"` when unknown.
fn fmt_time(ts: Option<jiff::Timestamp>) -> String {
    match ts {
        Some(t) => {
            let s = format!("{t}");
            s.chars().skip(11).take(8).collect()
        }
        None => "—".into(),
    }
}

/// Space-joined status markers: `↳sub` for sub-agent turns, `think?` when thinking
/// was encrypted/unmeasurable, else `think` when thinking was present (its tokens
/// are already inside Output).
fn fmt_flags(sidechain: bool, has_thinking: bool, thinking_encrypted: bool) -> String {
    let mut flags = Vec::new();
    if sidechain {
        flags.push("↳sub");
    }
    if thinking_encrypted {
        flags.push("think?");
    } else if has_thinking {
        flags.push("think");
    }
    flags.join(" ")
}

pub fn draw(frame: &mut Frame, detail: &SessionDetail, state: &mut TableState) {
    let [header_area, body_area, footer_area] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    let header = Paragraph::new(vec![
        Line::from(format!(
            "session {} · {} · {}",
            detail.id.chars().take(8).collect::<String>(),
            detail.project.clone().unwrap_or_else(|| "?".into()),
            detail.model.clone().unwrap_or_else(|| "?".into()),
        )),
        Line::from(format!(
            "requests {} · {} tokens · {} · sub-agents {}",
            fmt_count(detail.rollup.requests),
            fmt_count(detail.rollup.total_tokens()),
            fmt_cost(detail.rollup.cost_usd),
            fmt_count(detail.sub_agents),
        )),
    ]);
    frame.render_widget(header, header_area);

    if detail.turns.is_empty() {
        frame.render_widget(
            Paragraph::new("no turns in this session")
                .block(Block::default().borders(Borders::ALL).title("turns")),
            body_area,
        );
    } else {
        let rows = detail.turns.iter().map(|t| {
            let detail_text = t.summary.clone().unwrap_or_else(|| t.tools.join(","));
            Row::new(vec![
                fmt_time(t.ts),
                t.model.clone().unwrap_or_else(|| "?".into()),
                fmt_known(t.usage.input),
                fmt_known(t.usage.output),
                fmt_known(t.usage.cache_read),
                fmt_known(t.usage.cache_creation),
                t.cost_usd.map(fmt_cost).unwrap_or_else(|| "?".into()),
                fmt_flags(t.sidechain, t.has_thinking, t.thinking_encrypted),
                truncate(&detail_text, 36),
            ])
        });
        let table = Table::new(
            rows,
            [
                Constraint::Length(8),
                Constraint::Length(14),
                Constraint::Length(9),
                Constraint::Length(9),
                Constraint::Length(9),
                Constraint::Length(9),
                Constraint::Length(10),
                Constraint::Length(8),
                Constraint::Min(10),
            ],
        )
        .header(
            Row::new(vec![
                "Time",
                "Model",
                "Input",
                "Output",
                "Cache rd",
                "Cache wr",
                "Est. cost",
                "Flags",
                "Detail",
            ])
            .style(Style::default().add_modifier(Modifier::BOLD)),
        )
        .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .highlight_symbol("▶ ")
        .block(Block::default().borders(Borders::ALL).title("turns"));
        frame.render_stateful_widget(table, body_area, state);
    }

    let footer = Paragraph::new("↑/↓ or j/k turns · Enter context · Esc/Backspace back · q quit");
    frame.render_widget(footer, footer_area);
}
