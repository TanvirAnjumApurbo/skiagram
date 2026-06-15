//! Human-readable rendering of the heuristic task-type [`ClassifyReport`]
//! (CLAUDE.md §2 "where did it go / why is context full", roadmap v0.3).
//!
//! Spend here is MEASURED: deduplicated per-request token counts priced from the
//! embedded snapshot (§8.1/§8.7). The *labels*, by contrast, are explicitly
//! heuristic — inferred from each session's tool mix + prompt keywords, with no
//! ground truth in the files — so this view stays honest the same way the core
//! does: an [`TaskType::Unknown`] bucket, a confidence score, and the matched
//! signals surfaced (§8.5). Cost figures use the "?"/"≥" convention from
//! `table.rs` — an unpriced bucket is never shown as $0 (§8.5/§8.7).
//!
//! The section-header strings (`BY TASK TYPE`, `SESSIONS`, `NOTES`) are a
//! contract with the integration tests — keep them verbatim.

use comfy_table::{presets::UTF8_FULL_CONDENSED, Cell, CellAlignment, ContentArrangement, Table};
use owo_colors::{OwoColorize, Stream};
use tokscope_core::analysis::aggregate::Rollup;
use tokscope_core::analysis::classify::{ClassifyReport, SessionClass, TaskType, TaskTypeRollup};

use super::{fmt_cost, fmt_count};

/// Print a [`ClassifyReport`] as plain tables.
pub fn print(report: &ClassifyReport) {
    let since = report
        .since
        .map(|d| format!(" · since {d} (UTC)"))
        .unwrap_or_default();
    println!(
        "{} classify — {} · {} session(s) classified{}",
        "tokscope".if_supports_color(Stream::Stdout, |t| t.bold()),
        report.agent,
        fmt_count(report.sessions_classified),
        since
    );
    println!(
        "heuristic — activity inferred from tool mix + prompt keywords; spend is \
         deduplicated & priced from the embedded snapshot\n"
    );

    if report.sessions_classified == 0 {
        println!("no usage data found — nothing to classify.");
        return;
    }

    println!(
        "totals: {} tokens · {} across {} session(s)",
        fmt_count(report.total_tokens),
        cost_total(report),
        fmt_count(report.sessions_classified),
    );

    by_task_type(report);
    sessions(report);
    notes(report);
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

/// Short session id — 8 chars, matching `table.rs`'s session columns.
fn short_id(id: &str) -> String {
    id.chars().take(8).collect()
}

/// `0.0..=1.0` share -> "12.3%".
fn pct(share: f64) -> String {
    format!("{:.1}%", share * 100.0)
}

/// Truncate to `max` characters, appending an ellipsis when cut (char-safe,
/// matching `table.rs`'s `truncate`).
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

/// Human label for a [`TaskType`] — the heuristic categories, spelled out.
fn task_label(task_type: TaskType) -> &'static str {
    match task_type {
        TaskType::Debugging => "Debugging",
        TaskType::FeatureWork => "Feature work",
        TaskType::Refactoring => "Refactoring",
        TaskType::Testing => "Testing",
        TaskType::Documentation => "Documentation",
        TaskType::Exploration => "Exploration",
        TaskType::ConfigOps => "Config / ops",
        TaskType::Unknown => "Unknown",
    }
}

/// §8.5/§8.7: a rollup where nothing could be priced shows "?", a partially
/// priced one a "≥" lower bound — never $0 for unknown. Mirrors `table.rs`'s
/// `cost_or_unknown`.
fn cost_or_unknown(r: &Rollup) -> String {
    if r.unpriced_requests == r.requests && r.requests > 0 {
        "?".into()
    } else if r.unpriced_requests > 0 {
        format!("≥{}", fmt_cost(r.cost_usd))
    } else {
        fmt_cost(r.cost_usd)
    }
}

/// Grand-total cost, marked as a lower bound when any classified session was
/// unpriced.
fn cost_total(report: &ClassifyReport) -> String {
    if report.has_unpriced {
        format!("≥{}", fmt_cost(report.total_cost_usd))
    } else {
        fmt_cost(report.total_cost_usd)
    }
}

fn by_task_type(report: &ClassifyReport) {
    section("BY TASK TYPE");
    if report.by_task_type.is_empty() {
        println!("(nothing classified)");
        return;
    }
    let mut t = base_table(&[
        "Task type",
        "Sessions",
        "Requests",
        "Total tokens",
        "Token %",
        "Est. cost",
    ]);
    for b in &report.by_task_type {
        t.add_row(task_type_row(b));
    }
    println!("{t}");
}

fn task_type_row(b: &TaskTypeRollup) -> Vec<Cell> {
    vec![
        Cell::new(task_label(b.task_type)),
        num(fmt_count(b.sessions)),
        num(fmt_count(b.rollup.requests)),
        num(fmt_count(b.rollup.total_tokens())),
        num(pct(b.token_share)),
        num(cost_or_unknown(&b.rollup)),
    ]
}

fn sessions(report: &ClassifyReport) {
    section("SESSIONS");
    if report.sessions.is_empty() {
        println!("(none)");
        return;
    }
    let mut t = base_table(&[
        "Session",
        "Project",
        "Task type",
        "Confidence",
        "Tokens",
        "Est. cost",
        "Signals",
    ]);
    for s in report.sessions.iter().take(15) {
        t.add_row(session_row(s));
    }
    println!("{t}");
}

fn session_row(s: &SessionClass) -> Vec<Cell> {
    vec![
        Cell::new(short_id(&s.id)),
        Cell::new(truncate(s.project.as_deref().unwrap_or("?"), 24)),
        Cell::new(task_label(s.task_type)),
        num(pct(s.confidence)),
        num(fmt_count(s.rollup.total_tokens())),
        num(cost_or_unknown(&s.rollup)),
        Cell::new(truncate(&s.signals.join("; "), 40)),
    ]
}

fn notes(report: &ClassifyReport) {
    let mut notes = Vec::new();
    if report.has_unpriced {
        notes.push(
            "some classified sessions use an unpriced model (not in the embedded \
             snapshot); cost figures are lower bounds"
                .to_string(),
        );
    }
    if notes.is_empty() {
        return;
    }
    section("NOTES");
    for n in notes {
        println!("• {}", n.if_supports_color(Stream::Stdout, |t| t.yellow()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A priced rollup with the given request count and cost.
    fn priced_rollup(requests: u64, cost_usd: f64) -> Rollup {
        Rollup {
            requests,
            input: 100 * requests,
            output: 10 * requests,
            cost_usd,
            ..Rollup::default()
        }
    }

    #[test]
    fn task_label_covers_every_variant() {
        assert_eq!(task_label(TaskType::Debugging), "Debugging");
        assert_eq!(task_label(TaskType::FeatureWork), "Feature work");
        assert_eq!(task_label(TaskType::Refactoring), "Refactoring");
        assert_eq!(task_label(TaskType::Testing), "Testing");
        assert_eq!(task_label(TaskType::Documentation), "Documentation");
        assert_eq!(task_label(TaskType::Exploration), "Exploration");
        assert_eq!(task_label(TaskType::ConfigOps), "Config / ops");
        assert_eq!(task_label(TaskType::Unknown), "Unknown");
    }

    #[test]
    fn pct_formats_share_as_one_decimal_percent() {
        assert_eq!(pct(0.0), "0.0%");
        assert_eq!(pct(0.5), "50.0%");
        assert_eq!(pct(1.0), "100.0%");
        // Confidence is rendered the same way.
        assert_eq!(pct(0.875), "87.5%");
    }

    #[test]
    fn cost_or_unknown_uses_question_mark_when_fully_unpriced() {
        let r = Rollup {
            requests: 3,
            unpriced_requests: 3,
            ..Rollup::default()
        };
        assert_eq!(cost_or_unknown(&r), "?");
    }

    #[test]
    fn cost_or_unknown_uses_lower_bound_when_partially_unpriced() {
        let r = Rollup {
            requests: 3,
            unpriced_requests: 1,
            cost_usd: 0.5,
            ..Rollup::default()
        };
        assert_eq!(cost_or_unknown(&r), "≥$0.5000");
    }

    #[test]
    fn cost_or_unknown_is_plain_when_all_priced() {
        let r = priced_rollup(2, 1.25);
        assert_eq!(cost_or_unknown(&r), "$1.25");
    }

    #[test]
    fn cost_total_marks_lower_bound_only_when_unpriced() {
        let priced = ClassifyReport {
            agent: "claude-code".into(),
            since: None,
            sessions_classified: 1,
            total_tokens: 100,
            total_cost_usd: 2.5,
            has_unpriced: false,
            by_task_type: Vec::new(),
            sessions: Vec::new(),
        };
        assert_eq!(cost_total(&priced), "$2.50");

        let unpriced = ClassifyReport {
            has_unpriced: true,
            ..priced
        };
        assert_eq!(cost_total(&unpriced), "≥$2.50");
    }

    #[test]
    fn truncate_keeps_short_strings_and_ellipsizes_long_ones() {
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("debugging; tests", 8), "debuggi…");
    }

    #[test]
    fn short_id_takes_eight_chars() {
        assert_eq!(short_id("3e9d2c41-7b5a-4f2e"), "3e9d2c41");
        assert_eq!(short_id("abc"), "abc");
    }

    /// The row builders construct cells from hand-built values without touching
    /// terminal I/O — exercising the by-hand construction of every public type.
    #[test]
    fn rows_build_from_hand_constructed_values() {
        let type_row = task_type_row(&TaskTypeRollup {
            task_type: TaskType::Debugging,
            sessions: 2,
            rollup: priced_rollup(4, 0.12),
            token_share: 0.42,
            cost_share: Some(0.42),
        });
        assert_eq!(type_row.len(), 6);

        let sess_row = session_row(&SessionClass {
            id: "deadbeef-cafe".into(),
            project: Some("tokscope".into()),
            model: Some("claude-sonnet-4-5".into()),
            task_type: TaskType::FeatureWork,
            confidence: 0.75,
            signals: vec!["add".into(), "implement".into()],
            rollup: priced_rollup(3, 0.05),
        });
        assert_eq!(sess_row.len(), 7);
    }
}
