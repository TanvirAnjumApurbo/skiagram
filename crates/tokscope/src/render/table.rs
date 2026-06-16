//! Human-readable plain-table summary (`comfy-table`).

use comfy_table::{presets::UTF8_FULL_CONDENSED, Cell, CellAlignment, ContentArrangement, Table};
use owo_colors::{OwoColorize, Stream};
use tokscope_core::analysis::aggregate::{Rollup, Summary};
use tokscope_core::analysis::context::{ContextReport, ContextSource, SessionContext};

use super::{fmt_cost, fmt_count};

pub fn print(summary: &Summary) {
    let since = summary
        .since
        .map(|d| format!(" · since {d} (UTC)"))
        .unwrap_or_default();
    println!(
        "{} — {} · {} session file(s){}",
        "tokscope".if_supports_color(Stream::Stdout, |t| t.bold()),
        summary.agent,
        summary.sessions_parsed,
        since
    );
    println!("local-only · read-only · costs estimated from the embedded pricing snapshot\n");

    if summary.totals.requests == 0 {
        println!("no usage data found — nothing to report.");
        warnings(summary);
        return;
    }

    totals(summary);
    accounting_notes(summary);
    by_model(summary);
    by_day(summary);
    top_sessions(summary);
    top_tools(summary);
    warnings(summary);
}

fn section(title: &str) {
    println!(
        "\n{}",
        title.if_supports_color(Stream::Stdout, |t| t.bold())
    );
}

fn base_table(headers: &[&str]) -> Table {
    let mut t = Table::new();
    t.load_preset(UTF8_FULL_CONDENSED)
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_header(headers.to_vec());
    t
}

fn num(value: String) -> Cell {
    Cell::new(value).set_alignment(CellAlignment::Right)
}

fn rollup_cells(r: &Rollup) -> Vec<Cell> {
    vec![
        num(fmt_count(r.requests)),
        num(fmt_count(r.input)),
        num(fmt_count(r.output)),
        num(fmt_count(r.cache_read)),
        num(fmt_count(r.cache_creation)),
        num(fmt_count(r.total_tokens())),
        num(cost_or_unknown(r)),
    ]
}

/// §8.5/§8.7: a rollup where nothing could be priced shows "?", not $0.00.
fn cost_or_unknown(r: &Rollup) -> String {
    if r.unpriced_requests == r.requests && r.requests > 0 {
        "?".into()
    } else if r.unpriced_requests > 0 {
        format!("≥{}", fmt_cost(r.cost_usd))
    } else {
        fmt_cost(r.cost_usd)
    }
}

const ROLLUP_HEADERS: [&str; 7] = [
    "Requests",
    "Input",
    "Output",
    "Cache read",
    "Cache write",
    "Total tokens",
    "Est. cost",
];

fn totals(summary: &Summary) {
    section("TOTALS (deduplicated)");
    let mut t = base_table(&ROLLUP_HEADERS);
    t.add_row(rollup_cells(&summary.totals));
    println!("{t}");
}

fn accounting_notes(summary: &Summary) {
    let d = &summary.dedup;
    if d.duplicate_lines_collapsed > 0 {
        let real = summary.totals.total_tokens();
        let naive = d.naive_known_tokens;
        let overcount = if real > 0 && naive > real {
            format!(
                " — naive per-line summing would report {} tokens (+{:.0}% overcount avoided)",
                fmt_count(naive),
                (naive as f64 / real as f64 - 1.0) * 100.0
            )
        } else {
            String::new()
        };
        println!(
            "requestId dedup: collapsed {} duplicate line(s) into {} request(s){}",
            fmt_count(d.duplicate_lines_collapsed),
            fmt_count(summary.totals.requests),
            overcount
        );
    }
    if d.requests_with_thinking > 0 {
        let est = d.thinking_tokens_estimate();
        let enc = d.requests_with_encrypted_thinking;
        let detail = if enc > 0 {
            format!(
                " — {} had encrypted/unmeasurable thinking, so the visible ≈{} est. \
                 token(s) is a lower bound",
                fmt_count(enc),
                fmt_count(est),
            )
        } else {
            format!(" — visible thinking ≈{} est. token(s)", fmt_count(est))
        };
        println!(
            "extended thinking: used in {} of {} request(s); already counted inside Output above{}",
            fmt_count(d.requests_with_thinking),
            fmt_count(summary.totals.requests),
            detail,
        );
    }
    if summary.sidechain_totals.requests > 0 {
        let s = &summary.sidechain_totals;
        println!(
            "sub-agent share: {} tokens across {} request(s) ({}) — attributed to parent sessions",
            fmt_count(s.total_tokens()),
            fmt_count(s.requests),
            cost_or_unknown(s)
        );
    }
    if summary.compactions > 0 {
        println!(
            "context compactions: {} (the window filled up that many times)",
            summary.compactions
        );
    }
}

fn by_model(summary: &Summary) {
    section("BY MODEL");
    let mut t = base_table(&{
        let mut h = vec!["Model"];
        h.extend(ROLLUP_HEADERS);
        h
    });
    for (model, rollup) in &summary.by_model {
        let mut cells = vec![Cell::new(model)];
        cells.extend(rollup_cells(rollup));
        t.add_row(cells);
    }
    println!("{t}");
}

fn by_day(summary: &Summary) {
    section("BY DAY (UTC)");
    let mut t = base_table(&{
        let mut h = vec!["Day"];
        h.extend(ROLLUP_HEADERS);
        h
    });
    for (day, rollup) in &summary.by_day {
        let mut cells = vec![Cell::new(day)];
        cells.extend(rollup_cells(rollup));
        t.add_row(cells);
    }
    println!("{t}");
}

fn top_sessions(summary: &Summary) {
    section("TOP SESSIONS (sub-agent spend folded into parents)");
    let mut t = base_table(&[
        "Project",
        "Session",
        "Model",
        "Requests",
        "Total tokens",
        "Sub-agents",
        "Est. cost",
    ]);
    for s in summary.by_session.iter().take(10) {
        t.add_row(vec![
            Cell::new(s.project.as_deref().unwrap_or("?")),
            Cell::new(short_id(&s.id)),
            Cell::new(s.model.as_deref().unwrap_or("?")),
            num(fmt_count(s.rollup.requests)),
            num(fmt_count(s.rollup.total_tokens())),
            num(fmt_count(s.sub_agents)),
            num(cost_or_unknown(&s.rollup)),
        ]);
    }
    println!("{t}");
}

fn top_tools(summary: &Summary) {
    if summary.by_tool.is_empty() {
        return;
    }
    section("TOP TOOLS");
    let mut tools: Vec<_> = summary.by_tool.iter().collect();
    tools.sort_by(|a, b| b.1.calls.cmp(&a.1.calls).then_with(|| a.0.cmp(b.0)));
    let mut t = base_table(&["Tool", "MCP server", "Calls", "Input bytes"]);
    for (name, stat) in tools.into_iter().take(10) {
        t.add_row(vec![
            Cell::new(name),
            Cell::new(stat.server.as_deref().unwrap_or("-")),
            num(fmt_count(stat.calls)),
            num(fmt_count(stat.input_bytes)),
        ]);
    }
    println!("{t}");
}

fn warnings(summary: &Summary) {
    let mut notes = Vec::new();
    if !summary.unpriced_models.is_empty() {
        let models: Vec<_> = summary.unpriced_models.iter().cloned().collect();
        notes.push(format!(
            "unpriced models (not in the embedded snapshot; cost shown is a lower bound): {}",
            models.join(", ")
        ));
    }
    if summary.totals.incomplete_requests > 0 {
        notes.push(format!(
            "{} request(s) had unknown usage fields — unknown ≠ zero; totals are lower bounds",
            fmt_count(summary.totals.incomplete_requests)
        ));
    }
    if summary.sessions_failed > 0 {
        notes.push(format!(
            "{} session file(s) failed to parse entirely (re-run with TOKSCOPE_LOG=warn for paths)",
            summary.sessions_failed
        ));
    }
    if summary.skipped_lines > 0 {
        notes.push(format!(
            "{} unrecognized/corrupt line(s) skipped leniently",
            fmt_count(summary.skipped_lines)
        ));
    }
    if notes.is_empty() {
        return;
    }
    section("NOTES");
    for n in notes {
        println!("• {}", n.if_supports_color(Stream::Stdout, |t| t.yellow()));
    }
}

fn short_id(id: &str) -> String {
    id.chars().take(8).collect()
}

// ---------------------------------------------------------------------------
// `tokscope context` — context-bloat attribution (CLAUDE.md §2.2 / v0.2).
// ---------------------------------------------------------------------------

/// Print a [`ContextReport`]. Two kinds of number, kept visually distinct
/// (CLAUDE.md §8): **measured** real cache/usage tokens vs. **estimated**
/// (`~`-prefixed) shares of on-disk transcript content — the latter are
/// never billed and exist only to show *relative* composition.
pub fn print_context(report: &ContextReport) {
    let since = report
        .since
        .map(|d| format!(" · since {d} (UTC)"))
        .unwrap_or_default();
    println!(
        "{} context — {} · {} session(s) profiled{}",
        "tokscope".if_supports_color(Stream::Stdout, |t| t.bold()),
        report.agent,
        report.sessions_profiled,
        since
    );
    println!(
        "MEASURED from cache tokens (real, billed) · ESTIMATED splits from transcript content \
         (≈chars/4, never billed)\n"
    );

    if report.sessions_profiled == 0 {
        println!("no sessions found — nothing to report.");
        return;
    }

    context_overhead(report);
    by_source(report);
    by_server(report);
    heaviest(report);
    inventory(report);
    top_sessions_by_peak(report);
}

/// "?" for unmeasurable (`None`) — absence ≠ zero, never render as 0 (§8.5).
fn opt_count(value: Option<u64>) -> String {
    match value {
        Some(v) => fmt_count(v),
        None => "?".into(),
    }
}

/// `~1,234` — an ESTIMATED token figure, never billed.
fn est_count(tokens: u64) -> String {
    format!("~{}", fmt_count(tokens))
}

fn pct(share: f64) -> String {
    format!("{:.1}%", share * 100.0)
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

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

fn context_overhead(report: &ContextReport) {
    section("CONTEXT OVERHEAD (measured)");
    match &report.max_startup_overhead {
        Some((id, tokens)) => println!(
            "startup overhead: {} tokens (session {}) — system prompt + tool defs + memory + \
             first turn, sitting in the window before you type",
            fmt_count(*tokens),
            short_id(id)
        ),
        None => println!(
            "startup overhead: ? (no cold-start session found — every session resumed warm)"
        ),
    }

    let peak = report
        .sessions
        .iter()
        .filter_map(|s| s.peak_input_tokens)
        .max();
    match peak {
        Some(tokens) => println!(
            "peak window fill: {} tokens — the fullest the context window got in any session",
            fmt_count(tokens)
        ),
        None => println!("peak window fill: ? (no usage data found)"),
    }
}

fn by_source(report: &ContextReport) {
    section("BY SOURCE (estimated share of transcript content)");
    if report.by_source.is_empty() {
        println!("(no transcript content found)");
        return;
    }
    let mut t = base_table(&["Source", "Est. tokens", "Share"]);
    for sb in &report.by_source {
        t.add_row(vec![
            Cell::new(source_label(sb.source)),
            num(est_count(sb.est_tokens)),
            num(pct(sb.share)),
        ]);
    }
    println!("{t}");
}

fn by_server(report: &ContextReport) {
    section("BY MCP SERVER (estimated)");
    if report.by_server.is_empty() {
        println!("(no tool calls found)");
        return;
    }
    let mut t = base_table(&[
        "Server",
        "Calls",
        "Call chars",
        "Result chars",
        "Total est. tokens",
    ]);
    for sb in &report.by_server {
        t.add_row(vec![
            Cell::new(&sb.server),
            num(fmt_count(sb.calls)),
            num(fmt_count(sb.call_chars)),
            num(fmt_count(sb.result_chars)),
            num(est_count(sb.est_tokens)),
        ]);
    }
    println!("{t}");
}

fn heaviest(report: &ContextReport) {
    section("HEAVIEST CONTEXT ITEMS (estimated)");
    if report.heaviest.is_empty() {
        println!("(no transcript content found)");
        return;
    }
    let mut t = base_table(&["Source", "Label", "Est. tokens", "Session"]);
    for item in report.heaviest.iter().take(10) {
        t.add_row(vec![
            Cell::new(source_label(item.source)),
            Cell::new(truncate(item.label.as_deref().unwrap_or("-"), 40)),
            num(est_count(item.est_tokens)),
            Cell::new(short_id(&item.session_id)),
        ]);
    }
    println!("{t}");
}

fn inventory(report: &ContextReport) {
    section("INVENTORY");
    println!(
        "MCP servers in use: {}",
        if report.mcp_servers.is_empty() {
            "(none)".to_string()
        } else {
            report.mcp_servers.join(", ")
        }
    );
    println!(
        "deferred tools (available but NOT loaded — not bloat): {}",
        fmt_count(report.deferred_tools)
    );
    println!("skills listed: {}", fmt_count(report.skills_listed));
    println!(
        "MCP instruction blocks: {} chars",
        fmt_count(report.mcp_instruction_chars)
    );
    println!(
        "attachment content (deferred-tool/skill listings, MCP instructions, IDE/file context, \
         reminders): {} chars",
        fmt_count(report.attachment_chars)
    );
    println!(
        "context compactions: {} (the window filled up that many times)",
        report.compactions
    );
}

fn top_sessions_by_peak(report: &ContextReport) {
    section("TOP SESSIONS BY PEAK WINDOW");
    if report.sessions.is_empty() {
        println!("(no sessions found)");
        return;
    }
    let mut t = base_table(&[
        "Session",
        "Project",
        "Model",
        "Cold start",
        "Startup overhead",
        "Peak",
        "Final",
    ]);
    for s in report.sessions.iter().take(10) {
        t.add_row(session_context_row(s));
    }
    println!("{t}");
}

fn session_context_row(s: &SessionContext) -> Vec<Cell> {
    vec![
        Cell::new(short_id(&s.id)),
        Cell::new(s.project.as_deref().unwrap_or("?")),
        Cell::new(s.model.as_deref().unwrap_or("?")),
        Cell::new(if s.cold_start { "yes" } else { "no" }),
        num(opt_count(s.startup_overhead_tokens)),
        num(opt_count(s.peak_input_tokens)),
        num(opt_count(s.final_input_tokens)),
    ]
}
