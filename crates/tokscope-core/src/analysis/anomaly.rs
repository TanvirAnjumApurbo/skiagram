//! Retry-storm and fat-tail detection (CLAUDE.md §6 "Fat tail", roadmap v0.3).
//!
//! Answers "*which few requests dominated my spend, and did the agent loop?*"
//! Two findings, both computed over DEDUPLICATED requests — never raw JSONL
//! lines, since request-level dedup is THE accounting step (CLAUDE.md §8.1).
//! This pass reuses [`super::dedup::dedup_session`] exactly like
//! [`super::aggregate`] / [`super::drilldown`], so the numbers agree with the
//! `summary` and `context` commands.
//!
//! 1. **Fat tail** — the small fraction of requests that dominate token spend,
//!    surfaced two ways: a *concentration* table (the top 1 / 5 / 10 / 25 % of
//!    requests and the share of all tokens they account for) and a ranked list
//!    of the *heaviest individual requests*. Ranking and concentration are by
//!    TOKENS, which are always known and never a pricing guess (§8.7); USD cost
//!    is shown alongside where the model is priced, and totals are a lower bound
//!    when any request is unpriced (§8.5).
//!
//! 2. **Retry storms** — bursts of requests bunched together in time within one
//!    session: a candidate retry/loop episode (each retry re-sends the whole
//!    window, re-paying cache-read, often for little new output). Detected
//!    purely from timestamps as a run of at least [`STORM_MIN_REQUESTS`]
//!    requests whose every consecutive gap is at most [`STORM_MAX_GAP_SECONDS`].
//!    These thresholds are a coarse heuristic and are surfaced in the report so
//!    the output is self-describing.
//!
//! Note on scope: API-error lines (`isApiErrorMessage`) never reach this pass —
//! the adapter records them with `usage: None`, so dedup drops them (absence ≠
//! zero, §8.5). Storm detection therefore reflects only requests that actually
//! hit the API and cost money. Folding error-burst counts in is future work.

use std::collections::BTreeMap;

use jiff::civil::Date;
use jiff::Timestamp;
use serde::Serialize;

use crate::analysis::aggregate::Filter;
use crate::analysis::dedup::dedup_session;
use crate::model::Session;
use crate::pricing;

/// Minimum number of rapid requests for a run to count as a retry storm.
/// Heuristic — surfaced in [`AnomalyReport::storm_min_requests`].
pub const STORM_MIN_REQUESTS: u64 = 5;

/// Maximum allowed gap (seconds) between consecutive requests in a storm run.
/// Heuristic — surfaced in [`AnomalyReport::storm_max_gap_seconds`].
pub const STORM_MAX_GAP_SECONDS: i64 = 15;

/// How many heaviest individual requests to keep in the ranked list.
const HEAVIEST_N: usize = 15;

/// Top-of-distribution request fractions reported in the concentration table.
const CONCENTRATION_FRACTIONS: [f64; 4] = [0.01, 0.05, 0.10, 0.25];

/// One deduplicated request flagged as a heavy contributor (a fat-tail member).
#[derive(Debug, Clone, Serialize)]
pub struct HeavyRequest {
    pub session_id: String,
    pub request_id: Option<String>,
    pub model: Option<String>,
    pub ts: Option<Timestamp>,
    /// Known tokens for this request (`Usage::known_total`).
    pub total_tokens: u64,
    /// USD cost when the model is priced, else `None` (§8.7 — never guessed).
    pub cost_usd: Option<f64>,
    /// Share of total priced cost (`0.0..=1.0`), or `None` when this request is
    /// unpriced or total cost is zero.
    pub cost_share: Option<f64>,
    /// Share of total tokens (`0.0..=1.0`).
    pub token_share: f64,
    /// The request came from a sub-agent (sidechain) transcript.
    pub sidechain: bool,
    /// Thinking present but `output_tokens` looks too small to include it (§8.2).
    pub thinking_suspect: bool,
}

/// Cumulative concentration: the top `request_fraction` of requests (by tokens)
/// account for `token_share` of all tokens — the classic "fat tail" statement.
#[derive(Debug, Clone, Serialize)]
pub struct ConcentrationBucket {
    /// Top fraction of requests by count (e.g. `0.05` = the heaviest 5 %).
    pub request_fraction: f64,
    /// Requests in this top slice: `ceil(request_fraction * n)`, clamped to
    /// `1..=n`.
    pub requests: u64,
    /// Their share of total tokens (`0.0..=1.0`).
    pub token_share: f64,
    /// Their share of total priced cost, or `None` when total cost is zero.
    pub cost_share: Option<f64>,
}

/// A temporal burst of requests within one session — a candidate retry/loop
/// episode. See the module docs for the detection rule and its caveats.
#[derive(Debug, Clone, Serialize)]
pub struct RetryStorm {
    pub session_id: String,
    pub project: Option<String>,
    pub started_at: Timestamp,
    pub ended_at: Timestamp,
    /// Requests in the burst (post-dedup).
    pub requests: u64,
    pub total_tokens: u64,
    /// Summed cost of the burst's priced requests; `None` when none are priced.
    /// A lower bound when the burst mixes priced and unpriced requests (§8.5).
    pub cost_usd: Option<f64>,
    /// Wall-clock span of the burst, in seconds (`ended_at - started_at`).
    pub span_seconds: i64,
    /// Requests in the burst flagged as thinking-undercount-suspect (§8.2).
    pub thinking_suspect_requests: u64,
}

/// Retry-storm + fat-tail findings over a set of deduplicated requests.
#[derive(Debug, Clone, Serialize)]
pub struct AnomalyReport {
    pub agent: String,
    pub since: Option<Date>,
    /// Deduplicated requests considered (the denominator for every share).
    pub requests_analyzed: u64,
    pub total_tokens: u64,
    /// Total priced cost across analyzed requests; a lower bound when
    /// `has_unpriced` is true.
    pub total_cost_usd: f64,
    /// At least one analyzed request had no price (model unknown / not in the
    /// snapshot) — cost figures are lower bounds (§8.5/§8.7).
    pub has_unpriced: bool,
    /// Top-fraction concentration, ascending by `request_fraction`.
    pub concentration: Vec<ConcentrationBucket>,
    /// Heaviest individual requests, by tokens desc (then cost, ts, ids).
    pub heaviest: Vec<HeavyRequest>,
    /// Detected retry storms, by total tokens desc (then start, session).
    pub retry_storms: Vec<RetryStorm>,
    /// Heuristic params, echoed so the output is self-describing.
    pub storm_min_requests: u64,
    pub storm_max_gap_seconds: i64,
}

/// A deduplicated request reduced to just what anomaly detection needs.
struct Req {
    session_id: String,
    project: Option<String>,
    request_id: Option<String>,
    model: Option<String>,
    ts: Option<Timestamp>,
    tokens: u64,
    cost: Option<f64>,
    sidechain: bool,
    thinking_suspect: bool,
}

/// `part / whole` as a fraction, with `whole == 0` yielding `0.0` (never NaN).
fn share(part: u64, whole: u64) -> f64 {
    if whole == 0 {
        0.0
    } else {
        part as f64 / whole as f64
    }
}

/// Detect fat-tail concentration and retry storms across the given sessions.
///
/// Sub-agent transcripts are deduplicated like any other file; each request
/// keeps its own session id (storms are per-transcript timelines), while the
/// fat-tail rollups span every request in the window. `filter.since` is applied
/// inside [`dedup_session`], so the report covers the same window as `summary`.
pub fn detect(sessions: &[Session], filter: &Filter, agent: &str) -> AnomalyReport {
    // 1. Flatten every session to deduplicated requests (§8.1).
    let mut reqs: Vec<Req> = Vec::new();
    for session in sessions {
        let (records, _stats) = dedup_session(session, filter.since);
        for rec in records {
            let tokens = rec.usage.known_total();
            let cost = pricing::cost_usd(rec.model.as_deref(), &rec.usage);
            reqs.push(Req {
                session_id: rec.session_id,
                project: rec.project,
                request_id: rec.request_id,
                model: rec.model,
                ts: rec.ts,
                tokens,
                cost,
                sidechain: rec.sidechain,
                thinking_suspect: rec.thinking_suspect,
            });
        }
    }

    let n = reqs.len();
    let total_tokens: u64 = reqs.iter().map(|r| r.tokens).sum();
    let total_cost_usd: f64 = reqs.iter().filter_map(|r| r.cost).sum();
    let has_unpriced = reqs.iter().any(|r| r.cost.is_none());

    // Empty window: an honest empty report (still echo the heuristic params).
    if n == 0 {
        return AnomalyReport {
            agent: agent.to_string(),
            since: filter.since,
            requests_analyzed: 0,
            total_tokens: 0,
            total_cost_usd: 0.0,
            has_unpriced: false,
            concentration: Vec::new(),
            heaviest: Vec::new(),
            retry_storms: Vec::new(),
            storm_min_requests: STORM_MIN_REQUESTS,
            storm_max_gap_seconds: STORM_MAX_GAP_SECONDS,
        };
    }

    // 2. Rank requests by tokens desc for the fat-tail views. The order is a
    //    strict total order so the output is deterministic across runs.
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&a, &b| {
        let (ra, rb) = (&reqs[a], &reqs[b]);
        rb.tokens
            .cmp(&ra.tokens)
            .then_with(|| rb.cost.unwrap_or(0.0).total_cmp(&ra.cost.unwrap_or(0.0)))
            .then_with(|| ra.session_id.cmp(&rb.session_id))
            .then_with(|| ra.request_id.cmp(&rb.request_id))
    });

    // 3. Concentration: top ceil(frac * n) requests' share of tokens / cost.
    let concentration = CONCENTRATION_FRACTIONS
        .iter()
        .map(|&frac| {
            let k = ((frac * n as f64).ceil() as usize).clamp(1, n);
            let slice = &order[..k];
            let tok: u64 = slice.iter().map(|&i| reqs[i].tokens).sum();
            let cost: f64 = slice.iter().filter_map(|&i| reqs[i].cost).sum();
            ConcentrationBucket {
                request_fraction: frac,
                requests: k as u64,
                token_share: share(tok, total_tokens),
                cost_share: (total_cost_usd > 0.0).then_some(cost / total_cost_usd),
            }
        })
        .collect();

    // 4. Heaviest individual requests (the named fat-tail members).
    let heaviest = order
        .iter()
        .take(HEAVIEST_N)
        .map(|&i| {
            let r = &reqs[i];
            HeavyRequest {
                session_id: r.session_id.clone(),
                request_id: r.request_id.clone(),
                model: r.model.clone(),
                ts: r.ts,
                total_tokens: r.tokens,
                cost_usd: r.cost,
                cost_share: match (r.cost, total_cost_usd > 0.0) {
                    (Some(c), true) => Some(c / total_cost_usd),
                    _ => None,
                },
                token_share: share(r.tokens, total_tokens),
                sidechain: r.sidechain,
                thinking_suspect: r.thinking_suspect,
            }
        })
        .collect();

    // 5. Retry storms: per-session temporal bursts over dated requests.
    let retry_storms = detect_storms(&reqs);

    AnomalyReport {
        agent: agent.to_string(),
        since: filter.since,
        requests_analyzed: n as u64,
        total_tokens,
        total_cost_usd,
        has_unpriced,
        concentration,
        heaviest,
        retry_storms,
        storm_min_requests: STORM_MIN_REQUESTS,
        storm_max_gap_seconds: STORM_MAX_GAP_SECONDS,
    }
}

/// Scan each session's dated requests in time order and emit a [`RetryStorm`]
/// for every run of `>= STORM_MIN_REQUESTS` requests whose consecutive gaps are
/// all `<= STORM_MAX_GAP_SECONDS`. Undated requests cannot be sequenced and are
/// excluded from storms (they still count toward the fat-tail totals).
fn detect_storms(reqs: &[Req]) -> Vec<RetryStorm> {
    let mut by_session: BTreeMap<&str, Vec<(Timestamp, usize)>> = BTreeMap::new();
    for (i, r) in reqs.iter().enumerate() {
        if let Some(ts) = r.ts {
            by_session
                .entry(r.session_id.as_str())
                .or_default()
                .push((ts, i));
        }
    }

    let mut storms: Vec<RetryStorm> = Vec::new();
    for (_session, mut timeline) in by_session {
        timeline.sort();
        let mut run: Vec<(Timestamp, usize)> = Vec::new();
        let mut last: Option<Timestamp> = None;
        for &(ts, i) in &timeline {
            let continues = matches!(
                last,
                Some(prev) if ts.as_second() - prev.as_second() <= STORM_MAX_GAP_SECONDS
            );
            if continues {
                run.push((ts, i));
            } else {
                push_storm(&mut storms, reqs, &run);
                run = vec![(ts, i)];
            }
            last = Some(ts);
        }
        push_storm(&mut storms, reqs, &run);
    }

    // Heaviest bursts first; stable, deterministic tiebreak.
    storms.sort_by(|a, b| {
        b.total_tokens
            .cmp(&a.total_tokens)
            .then_with(|| a.started_at.cmp(&b.started_at))
            .then_with(|| a.session_id.cmp(&b.session_id))
    });
    storms
}

/// Emit a [`RetryStorm`] for `run` when it is long enough; otherwise a no-op.
fn push_storm(out: &mut Vec<RetryStorm>, reqs: &[Req], run: &[(Timestamp, usize)]) {
    if (run.len() as u64) < STORM_MIN_REQUESTS {
        return;
    }
    let (started_at, first_idx) = run[0];
    let ended_at = run[run.len() - 1].0;

    let mut total_tokens = 0u64;
    let mut cost_sum = 0.0f64;
    let mut any_priced = false;
    let mut thinking_suspect_requests = 0u64;
    for &(_, i) in run {
        total_tokens += reqs[i].tokens;
        if let Some(c) = reqs[i].cost {
            cost_sum += c;
            any_priced = true;
        }
        if reqs[i].thinking_suspect {
            thinking_suspect_requests += 1;
        }
    }

    out.push(RetryStorm {
        session_id: reqs[first_idx].session_id.clone(),
        project: reqs[first_idx].project.clone(),
        started_at,
        ended_at,
        requests: run.len() as u64,
        total_tokens,
        cost_usd: any_priced.then_some(cost_sum),
        span_seconds: ended_at.as_second() - started_at.as_second(),
        thinking_suspect_requests,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Event, EventKind, Usage};

    /// Assistant event at `ts` with one request id and the given token counts.
    fn turn(rid: &str, ts: &str, input: u64, output: u64) -> Event {
        Event {
            kind: EventKind::Assistant,
            ts: Some(ts.parse().unwrap()),
            request_id: Some(rid.into()),
            model: Some("claude-sonnet-4-5".into()),
            usage: Some(Usage {
                input: Some(input),
                output: Some(output),
                cache_creation: Some(0),
                cache_read: Some(0),
                ..Usage::default()
            }),
            tool_calls: Vec::new(),
            sidechain: false,
            content_summary: None,
            content_chars: 0,
            thinking_chars: 0,
            has_thinking: false,
            tool_use_id: None,
            attachment_kind: None,
            item_count: 0,
        }
    }

    fn session(id: &str, parent: Option<&str>, events: Vec<Event>) -> Session {
        Session {
            id: id.into(),
            agent: "claude-code".into(),
            project: Some("proj".into()),
            model: Some("claude-sonnet-4-5".into()),
            parent_session: parent.map(str::to_string),
            started_at: None,
            ended_at: None,
            events,
            sub_agents: Vec::new(),
            skipped_lines: 0,
        }
    }

    fn bucket(r: &AnomalyReport, frac: f64) -> &ConcentrationBucket {
        r.concentration
            .iter()
            .find(|b| (b.request_fraction - frac).abs() < 1e-9)
            .expect("bucket present")
    }

    /// One giant request among nine small ones: the heaviest 10 % (1 request)
    /// holds the lion's share of tokens — the fat-tail headline.
    #[test]
    fn concentration_reports_token_share_of_top_requests() {
        let mut events = vec![turn("big", "2026-06-02T10:00:00Z", 100_000, 0)];
        // Nine small requests, each 1,000 input, spread far apart in time so they
        // form no storm.
        for i in 0..9 {
            events.push(turn(
                &format!("small{i}"),
                &format!("2026-06-02T1{i}:00:00Z"),
                1_000,
                0,
            ));
        }
        let s = session("s", None, events);
        let r = detect(&[s], &Filter::default(), "claude-code");

        assert_eq!(r.requests_analyzed, 10);
        assert_eq!(r.total_tokens, 109_000);

        // top 10 % of 10 = 1 request = the 100k one.
        let b10 = bucket(&r, 0.10);
        assert_eq!(b10.requests, 1);
        assert!((b10.token_share - 100_000.0 / 109_000.0).abs() < 1e-9);
        // top 25 % of 10 = ceil(2.5) = 3 requests = 100k + 1k + 1k.
        let b25 = bucket(&r, 0.25);
        assert_eq!(b25.requests, 3);
        assert!((b25.token_share - 102_000.0 / 109_000.0).abs() < 1e-9);

        // Heaviest list ranks the giant first with the right share.
        assert_eq!(r.heaviest[0].request_id.as_deref(), Some("big"));
        assert_eq!(r.heaviest[0].total_tokens, 100_000);
        assert!((r.heaviest[0].token_share - 100_000.0 / 109_000.0).abs() < 1e-9);
        // sonnet-4-5 is priced, so cost + cost_share are present.
        assert!(r.heaviest[0].cost_usd.is_some());
        assert!(r.heaviest[0].cost_share.is_some());
        assert!(!r.has_unpriced);
    }

    /// Five requests within 15 s gaps form one storm; a far-later sixth does not
    /// extend it (gap too large) and is too short on its own.
    #[test]
    fn retry_storm_detects_a_temporal_burst() {
        let s = session(
            "s",
            None,
            vec![
                turn("r0", "2026-06-02T10:00:00Z", 1_000, 10),
                turn("r1", "2026-06-02T10:00:10Z", 1_000, 10),
                turn("r2", "2026-06-02T10:00:20Z", 1_000, 10),
                turn("r3", "2026-06-02T10:00:30Z", 1_000, 10),
                turn("r4", "2026-06-02T10:00:40Z", 1_000, 10),
                // One hour later — breaks the run.
                turn("late", "2026-06-02T11:00:00Z", 1_000, 10),
            ],
        );
        let r = detect(&[s], &Filter::default(), "claude-code");
        assert_eq!(r.retry_storms.len(), 1, "one burst of five");
        let storm = &r.retry_storms[0];
        assert_eq!(storm.requests, 5);
        assert_eq!(storm.total_tokens, 5 * 1_010);
        assert_eq!(storm.span_seconds, 40);
        assert_eq!(storm.session_id, "s");
        assert!(storm.cost_usd.is_some());
    }

    /// Four rapid requests are below the storm floor; one gap over the limit
    /// splits an otherwise-rapid sequence so neither half qualifies.
    #[test]
    fn no_storm_below_min_or_across_a_large_gap() {
        // Only four rapid requests.
        let four = session(
            "four",
            None,
            vec![
                turn("a", "2026-06-02T10:00:00Z", 1, 1),
                turn("b", "2026-06-02T10:00:05Z", 1, 1),
                turn("c", "2026-06-02T10:00:10Z", 1, 1),
                turn("d", "2026-06-02T10:00:15Z", 1, 1),
            ],
        );
        // Six requests but a 60 s gap in the middle splits them 3 + 3.
        let split = session(
            "split",
            None,
            vec![
                turn("a", "2026-06-02T10:00:00Z", 1, 1),
                turn("b", "2026-06-02T10:00:05Z", 1, 1),
                turn("c", "2026-06-02T10:00:10Z", 1, 1),
                turn("d", "2026-06-02T10:01:10Z", 1, 1),
                turn("e", "2026-06-02T10:01:15Z", 1, 1),
                turn("f", "2026-06-02T10:01:20Z", 1, 1),
            ],
        );
        let r = detect(&[four, split], &Filter::default(), "claude-code");
        assert!(r.retry_storms.is_empty());
    }

    /// Dedup runs first: three lines of one request collapse to a single
    /// analyzed request, so neither tokens nor the storm count are inflated.
    #[test]
    fn dedup_is_applied_before_counting() {
        let s = session(
            "s",
            None,
            vec![
                turn("req", "2026-06-02T10:00:00Z", 1_000, 200),
                turn("req", "2026-06-02T10:00:00Z", 1_000, 200),
                turn("req", "2026-06-02T10:00:00Z", 1_000, 200),
            ],
        );
        let r = detect(&[s], &Filter::default(), "claude-code");
        assert_eq!(r.requests_analyzed, 1, "3 lines -> 1 request");
        assert_eq!(r.total_tokens, 1_200);
        assert!(r.retry_storms.is_empty(), "one request is not a storm");
    }

    /// Unpriced models surface as such: cost is a lower bound, per-request cost
    /// is `None`, but tokens (and the fat-tail ranking) are unaffected (§8.7).
    #[test]
    fn unpriced_requests_are_flagged_not_guessed() {
        let mut unp = turn("u", "2026-06-02T10:00:00Z", 5_000, 0);
        unp.model = Some("claude-opus-4-8".into()); // post-snapshot, unpriced
        let priced = turn("p", "2026-06-02T11:00:00Z", 1_000, 0);
        let s = session("s", None, vec![unp, priced]);
        let r = detect(&[s], &Filter::default(), "claude-code");

        assert!(r.has_unpriced);
        // Heaviest is the 5k unpriced request — ranking is by tokens, not cost.
        assert_eq!(r.heaviest[0].request_id.as_deref(), Some("u"));
        assert_eq!(r.heaviest[0].cost_usd, None);
        assert_eq!(r.heaviest[0].cost_share, None, "no cost -> no share");
        // total cost reflects only the priced request (a lower bound).
        assert!(r.total_cost_usd > 0.0);
    }

    /// `since` flows through to dedup: out-of-window requests drop entirely.
    #[test]
    fn since_filter_restricts_the_window() {
        let s = session(
            "s",
            None,
            vec![
                turn("old", "2026-06-01T10:00:00Z", 9_999, 0),
                turn("new", "2026-06-02T10:00:00Z", 100, 0),
            ],
        );
        let filter = Filter {
            since: Some("2026-06-02".parse().unwrap()),
        };
        let r = detect(&[s], &filter, "claude-code");
        assert_eq!(r.requests_analyzed, 1);
        assert_eq!(r.total_tokens, 100);
        assert_eq!(r.heaviest[0].request_id.as_deref(), Some("new"));
        assert_eq!(r.since, filter.since);
    }

    /// No requests in the window -> an empty but well-formed report.
    #[test]
    fn empty_window_is_an_empty_report() {
        let r = detect(&[], &Filter::default(), "claude-code");
        assert_eq!(r.requests_analyzed, 0);
        assert!(r.concentration.is_empty());
        assert!(r.heaviest.is_empty());
        assert!(r.retry_storms.is_empty());
        assert_eq!(r.storm_min_requests, STORM_MIN_REQUESTS);
    }

    /// A storm inside a sub-agent transcript is detected on its own timeline and
    /// the burst is marked with that transcript's session id.
    #[test]
    fn storm_within_a_subagent_transcript() {
        let child = session(
            "agent-x",
            Some("parent"),
            vec![
                turn("c0", "2026-06-02T10:00:00Z", 100, 1),
                turn("c1", "2026-06-02T10:00:05Z", 100, 1),
                turn("c2", "2026-06-02T10:00:10Z", 100, 1),
                turn("c3", "2026-06-02T10:00:14Z", 100, 1),
                turn("c4", "2026-06-02T10:00:18Z", 100, 1),
            ],
        );
        let r = detect(&[child], &Filter::default(), "claude-code");
        assert_eq!(r.retry_storms.len(), 1);
        assert_eq!(r.retry_storms[0].session_id, "agent-x");
        assert!(r.heaviest.iter().all(|h| h.sidechain));
    }
}
