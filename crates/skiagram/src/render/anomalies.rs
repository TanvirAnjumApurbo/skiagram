//! Human-readable rendering of the retry-storm / fat-tail [`AnomalyReport`]
//! (CLAUDE.md §6 "Fat tail", roadmap v0.3).
//!
//! Every figure here is MEASURED: deduplicated token counts and prices traced to
//! the embedded snapshot (§8.7). Unlike `context`, there are no estimated
//! (chars/4) numbers in this view.
//!
//! The section-header strings (`CONCENTRATION`, `HEAVIEST REQUESTS`, `RETRY
//! STORMS`) are a contract with the integration tests — keep them verbatim.

use comfy_table::{presets::UTF8_FULL_CONDENSED, Cell, CellAlignment, ContentArrangement, Table};
use owo_colors::{OwoColorize, Stream};
use skiagram_core::analysis::anomaly::{
    AnomalyReport, ConcentrationBucket, HeavyRequest, RetryStorm,
};

use super::{fmt_cost, fmt_count};

/// Print an [`AnomalyReport`] as plain tables.
pub fn print(report: &AnomalyReport) {
    let since = report
        .since
        .map(|d| format!(" · since {d} (UTC)"))
        .unwrap_or_default();
    println!(
        "{} anomalies — {} · {} request(s) analyzed{}",
        "skiagram".if_supports_color(Stream::Stdout, |t| t.bold()),
        report.agent,
        fmt_count(report.requests_analyzed),
        since
    );
    println!("deduplicated (per-request) token counts · cost from the embedded pricing snapshot\n");

    if report.requests_analyzed == 0 {
        println!("no usage data found — nothing to report.");
        return;
    }

    println!(
        "totals: {} tokens · {} across {} request(s)",
        fmt_count(report.total_tokens),
        cost_total(report),
        fmt_count(report.requests_analyzed),
    );

    if let Some(headline) = headline_insight(&report.concentration) {
        println!(
            "{}",
            headline.if_supports_color(Stream::Stdout, |t| t.yellow())
        );
    }

    concentration(report);
    heaviest(report);
    retry_storms(report);
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

/// Short request id — 12 chars (request ids are longer/more similar than
/// session ids, so they need more room to disambiguate at a glance).
fn short_request_id(id: &str) -> String {
    id.chars().take(12).collect()
}

fn pct(share: f64) -> String {
    format!("{:.1}%", share * 100.0)
}

/// Optional share -> "12.3%" or "?" (unpriced -> no share, absence ≠ zero).
fn opt_pct(share: Option<f64>) -> String {
    share.map(pct).unwrap_or_else(|| "?".into())
}

/// Optional per-request cost -> "$0.0123" or "?" (§8.5/§8.7 — never $0 for
/// unknown).
fn opt_cost(cost: Option<f64>) -> String {
    cost.map(fmt_cost).unwrap_or_else(|| "?".into())
}

/// Grand-total cost, marked as a lower bound when any request was unpriced.
fn cost_total(report: &AnomalyReport) -> String {
    if report.has_unpriced {
        format!("≥{}", fmt_cost(report.total_cost_usd))
    } else {
        fmt_cost(report.total_cost_usd)
    }
}

/// `HH:MM:SS` from a `Timestamp`'s RFC-3339 display (UTC).
fn hms(ts: jiff::Timestamp) -> String {
    let s = format!("{ts}");
    s.chars().skip(11).take(8).collect()
}

/// Flag markers for one HEAVIEST REQUESTS row. `heaviest_rank1` marks the #1
/// (single heaviest) row with `★`, drawing the eye to the top contributor —
/// plain text, so it can't desync comfy-table's column-width math the way
/// embedded ANSI color codes would (this crate doesn't enable comfy-table's
/// `custom_styling` feature).
fn flags(sidechain: bool, has_thinking: bool, heaviest_rank1: bool) -> String {
    let mut f = Vec::new();
    if heaviest_rank1 {
        f.push("★");
    }
    if sidechain {
        f.push("↳sub");
    }
    if has_thinking {
        f.push("think");
    }
    f.join(" ")
}

/// A single honest sentence summarizing the fat tail, derived only from the
/// concentration buckets (ascending by `request_fraction`).
///
/// Prefers the smallest top-fraction bucket whose `token_share >= 0.5` — "the
/// heaviest N requests (top X%) already account for >=50% of all tokens".
/// When no bucket clears 50%, falls back to a neutral statement using the
/// top-10% bucket (present whenever the report is non-empty), so the line
/// never overstates concentration that isn't there.
fn headline_insight(buckets: &[ConcentrationBucket]) -> Option<String> {
    if let Some(b) = buckets.iter().find(|b| b.token_share >= 0.5) {
        return Some(format!(
            "⚠ the heaviest {} request(s) (top {}) account for {} of all tokens — that's your fat tail.",
            fmt_count(b.requests),
            pct(b.request_fraction),
            pct(b.token_share),
        ));
    }

    // Neutral fallback: report the top-10% bucket as-is, no alarm framing.
    let top10 = buckets
        .iter()
        .find(|b| (b.request_fraction - 0.10).abs() < 1e-9)?;
    Some(format!(
        "the heaviest {} request(s) (top {}) account for {} of all tokens.",
        fmt_count(top10.requests),
        pct(top10.request_fraction),
        pct(top10.token_share),
    ))
}

fn concentration(report: &AnomalyReport) {
    section("CONCENTRATION (fat tail — few requests, most of the tokens)");
    if report.concentration.is_empty() {
        println!("(not enough requests)");
        return;
    }
    let mut t = base_table(&["Top requests", "Count", "Token share", "Cost share"]);
    for b in &report.concentration {
        t.add_row(vec![
            Cell::new(format!("top {}", pct(b.request_fraction))),
            num(fmt_count(b.requests)),
            num(pct(b.token_share)),
            num(opt_pct(b.cost_share)),
        ]);
    }
    println!("{t}");
}

fn heaviest(report: &AnomalyReport) {
    section("HEAVIEST REQUESTS");
    if report.heaviest.is_empty() {
        println!("(none)");
        return;
    }
    let mut t = base_table(&[
        "Session",
        "Request",
        "Model",
        "Tokens",
        "Token %",
        "Cost %",
        "Est. cost",
        "Flags",
    ]);
    for (i, h) in report.heaviest.iter().take(10).enumerate() {
        t.add_row(heavy_row(h, i == 0));
    }
    println!("{t}");
}

/// One row of the HEAVIEST REQUESTS table. `is_top` marks the single heaviest
/// request (rank #1 in the tokens-desc ranking) with a `★` flag, so it stands
/// out as the headline contributor at a glance.
fn heavy_row(h: &HeavyRequest, is_top: bool) -> Vec<Cell> {
    vec![
        Cell::new(short_id(&h.session_id)),
        Cell::new(
            h.request_id
                .as_deref()
                .map(short_request_id)
                .unwrap_or_else(|| "?".into()),
        ),
        Cell::new(h.model.as_deref().unwrap_or("?")),
        num(fmt_count(h.total_tokens)),
        num(pct(h.token_share)),
        num(opt_pct(h.cost_share)),
        num(opt_cost(h.cost_usd)),
        Cell::new(flags(h.sidechain, h.has_thinking, is_top)),
    ]
}

fn retry_storms(report: &AnomalyReport) {
    section("RETRY STORMS (rapid request bursts — candidate retry/loop episodes)");
    println!(
        "heuristic: ≥ {} requests with every gap ≤ {}s",
        report.storm_min_requests, report.storm_max_gap_seconds
    );
    if report.retry_storms.is_empty() {
        println!("none detected.");
        return;
    }
    let mut t = base_table(&[
        "Session",
        "Project",
        "Start (UTC)",
        "Requests",
        "Span (s)",
        "Tokens",
        "Est. cost",
    ]);
    for s in &report.retry_storms {
        t.add_row(storm_row(s));
    }
    println!("{t}");
}

fn storm_row(s: &RetryStorm) -> Vec<Cell> {
    vec![
        Cell::new(short_id(&s.session_id)),
        Cell::new(s.project.as_deref().unwrap_or("?")),
        Cell::new(hms(s.started_at)),
        num(fmt_count(s.requests)),
        num(fmt_count(s.span_seconds.max(0) as u64)),
        num(fmt_count(s.total_tokens)),
        num(opt_cost(s.cost_usd)),
    ]
}

fn notes(report: &AnomalyReport) {
    let mut notes = Vec::new();
    if report.has_unpriced {
        notes.push(
            "some requests use an unpriced model (not in the embedded snapshot); \
             cost figures are lower bounds"
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
    use skiagram_core::analysis::anomaly::ConcentrationBucket;

    fn bucket(request_fraction: f64, requests: u64, token_share: f64) -> ConcentrationBucket {
        ConcentrationBucket {
            request_fraction,
            requests,
            token_share,
            cost_share: Some(token_share),
        }
    }

    #[test]
    fn headline_picks_smallest_bucket_at_or_above_half() {
        let buckets = vec![
            bucket(0.01, 1, 0.20),
            bucket(0.05, 3, 0.55), // first to cross 50%
            bucket(0.10, 5, 0.70),
            bucket(0.25, 12, 0.90),
        ];
        let h = headline_insight(&buckets).unwrap();
        assert!(h.contains("3 request(s)"), "{h}");
        assert!(h.contains("top 5.0%"), "{h}");
        assert!(h.contains("55.0%"), "{h}");
        assert!(h.starts_with("⚠"), "{h}");
    }

    #[test]
    fn headline_falls_back_to_top_10_when_nothing_reaches_half() {
        let buckets = vec![
            bucket(0.01, 1, 0.05),
            bucket(0.05, 2, 0.15),
            bucket(0.10, 4, 0.28),
            bucket(0.25, 9, 0.45),
        ];
        let h = headline_insight(&buckets).unwrap();
        // Neutral framing, no warning glyph.
        assert!(!h.starts_with("⚠"), "{h}");
        assert!(h.contains("4 request(s)"), "{h}");
        assert!(h.contains("top 10.0%"), "{h}");
        assert!(h.contains("28.0%"), "{h}");
    }

    #[test]
    fn headline_is_none_for_empty_concentration() {
        assert!(headline_insight(&[]).is_none());
    }

    #[test]
    fn short_request_id_takes_twelve_chars() {
        assert_eq!(short_request_id("req_01abcdef23456789"), "req_01abcdef");
        assert_eq!(short_request_id("short"), "short");
    }

    #[test]
    fn flags_combine_star_sidechain_and_thinking() {
        assert_eq!(flags(false, false, false), "");
        assert_eq!(flags(true, false, false), "↳sub");
        assert_eq!(flags(false, true, false), "think");
        assert_eq!(flags(true, true, false), "↳sub think");
        assert_eq!(flags(false, false, true), "★");
        assert_eq!(flags(true, true, true), "★ ↳sub think");
    }
}
