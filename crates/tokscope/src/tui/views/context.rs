//! Level 3: the context-bloat ("why is my context window full") breakdown of
//! one drilled session.
//!
//! A multi-section, SCROLLABLE text view: the whole body is built as a
//! `Vec<Line>` and rendered through a single `Paragraph::scroll((scroll, 0))`,
//! with a fixed header/footer outside the scroll region (CLAUDE.md §8 — keep
//! MEASURED vs ESTIMATED numbers visually distinct, same labels as
//! `render::table::print_context`).

use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;
use tokscope_core::analysis::context::{ContextSource, SessionContext};
use tokscope_core::analysis::drilldown::SessionDetail;

use crate::render::fmt_count;

pub fn draw(frame: &mut Frame, detail: &SessionDetail, scroll: u16) {
    let [header_area, body_area, footer_area] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    let header = Paragraph::new(vec![
        Line::from(format!(
            "context · {} · {} · {}",
            short_id(&detail.id),
            detail.project.as_deref().unwrap_or("?"),
            detail.model.as_deref().unwrap_or("?"),
        )),
        Line::from("MEASURED = real cache tokens · ESTIMATED (~) = chars/4, never billed"),
    ]);
    frame.render_widget(header, header_area);

    let lines = body_lines(detail);
    let body = Paragraph::new(lines)
        .block(Block::default().borders(Borders::ALL).title("context"))
        .scroll((scroll, 0));
    frame.render_widget(body, body_area);

    let footer = Paragraph::new("↑/↓ or j/k scroll · Esc/Backspace back · q quit");
    frame.render_widget(footer, footer_area);
}

/// Build the scrollable body: per-session MEASURED figures, this session's
/// ESTIMATED source split, the cross-session heaviest items, the inventory,
/// and a by-MCP-server breakdown.
fn body_lines(detail: &SessionDetail) -> Vec<Line<'static>> {
    let report = &detail.context;
    let sc = report
        .sessions
        .iter()
        .find(|s| !s.sidechain)
        .or_else(|| report.sessions.first());

    let mut lines = Vec::new();

    match sc {
        Some(sc) => {
            measured_section(&mut lines, sc);
            lines.push(Line::from(""));
            by_source_section(&mut lines, sc);
        }
        None => {
            lines.push(Line::from("no context data"));
        }
    }

    lines.push(Line::from(""));
    heaviest_section(&mut lines, detail);
    lines.push(Line::from(""));
    inventory_section(&mut lines, detail);
    lines.push(Line::from(""));
    by_server_section(&mut lines, detail);

    lines
}

fn heading(text: &'static str) -> Line<'static> {
    Line::styled(text, Style::default().add_modifier(Modifier::BOLD))
}

fn measured_section(lines: &mut Vec<Line<'static>>, sc: &SessionContext) {
    lines.push(heading("MEASURED (real tokens in the window)"));

    let cold_note = if sc.cold_start {
        "(cold start)"
    } else {
        "(warm resume — unmeasurable)"
    };
    lines.push(Line::from(format!(
        "startup overhead: {} tokens {}",
        opt_count(sc.startup_overhead_tokens),
        cold_note
    )));
    lines.push(Line::from(format!(
        "peak window fill: {} tokens",
        opt_count(sc.peak_input_tokens)
    )));
    lines.push(Line::from(format!(
        "final window: {} tokens",
        opt_count(sc.final_input_tokens)
    )));
    lines.push(Line::from(format!("compactions: {}", sc.compactions)));
}

fn by_source_section(lines: &mut Vec<Line<'static>>, sc: &SessionContext) {
    lines.push(heading("BY SOURCE (estimated share of transcript content)"));
    if sc.sources.is_empty() {
        lines.push(Line::from("(no transcript content)"));
        return;
    }
    for sb in &sc.sources {
        lines.push(Line::from(format!(
            "{:<16} ~{:>8}  {:.1}%",
            source_label(sb.source),
            fmt_count(sb.est_tokens),
            sb.share * 100.0
        )));
    }
}

fn heaviest_section(lines: &mut Vec<Line<'static>>, detail: &SessionDetail) {
    lines.push(heading("HEAVIEST ITEMS (estimated)"));
    if detail.context.heaviest.is_empty() {
        lines.push(Line::from("(none)"));
        return;
    }
    for item in detail.context.heaviest.iter().take(8) {
        lines.push(Line::from(format!(
            "{:<14} {:<36}  ~{}",
            source_label(item.source),
            truncate(item.label.as_deref().unwrap_or("-"), 36),
            fmt_count(item.est_tokens)
        )));
    }
}

fn inventory_section(lines: &mut Vec<Line<'static>>, detail: &SessionDetail) {
    let report = &detail.context;
    lines.push(heading("INVENTORY"));
    let servers = if report.mcp_servers.is_empty() {
        "(none)".to_string()
    } else {
        report.mcp_servers.join(", ")
    };
    lines.push(Line::from(format!("MCP servers: {servers}")));
    lines.push(Line::from(format!(
        "deferred tools (available but NOT loaded — not bloat): {}",
        report.deferred_tools
    )));
    lines.push(Line::from(format!(
        "skills listed: {}",
        report.skills_listed
    )));
    lines.push(Line::from(format!(
        "MCP instruction blocks: {} chars",
        report.mcp_instruction_chars
    )));
}

fn by_server_section(lines: &mut Vec<Line<'static>>, detail: &SessionDetail) {
    lines.push(heading("BY MCP SERVER (estimated)"));
    if detail.context.by_server.is_empty() {
        lines.push(Line::from("(no tool calls found)"));
        return;
    }
    for sb in &detail.context.by_server {
        lines.push(Line::from(format!(
            "{:<18} calls {:>4}  ~{}",
            sb.server,
            sb.calls,
            fmt_count(sb.est_tokens)
        )));
    }
}

/// "?" for unmeasurable (`None`) — absence ≠ zero, never render as 0 (CLAUDE.md §8.5).
fn opt_count(value: Option<u64>) -> String {
    match value {
        Some(v) => fmt_count(v),
        None => "?".into(),
    }
}

fn source_label(source: ContextSource) -> &'static str {
    match source {
        ContextSource::UserPrompts => "User prompts",
        ContextSource::AssistantText => "Assistant text",
        ContextSource::Thinking => "Thinking",
        ContextSource::ToolCalls => "Tool calls",
        ContextSource::ToolResults => "Tool results",
        ContextSource::Attachments => "Attachments",
    }
}

fn short_id(id: &str) -> String {
    id.chars().take(8).collect()
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}
