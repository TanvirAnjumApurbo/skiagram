//! Human-readable plain-table summary (`comfy-table`).

use comfy_table::{presets::UTF8_FULL_CONDENSED, Cell, CellAlignment, ContentArrangement, Table};
use owo_colors::{OwoColorize, Stream};
use tokscope_core::analysis::aggregate::{Rollup, Summary};

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
    if d.thinking_suspect_requests > 0 {
        println!(
            "{}",
            format!(
                "thinking-token reconciliation: {} request(s) look UNDERCOUNTED \
                 (thinking present but excluded from output_tokens) — totals are a lower bound",
                d.thinking_suspect_requests
            )
            .if_supports_color(Stream::Stdout, |t| t.yellow())
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
