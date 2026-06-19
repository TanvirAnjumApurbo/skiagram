//! Folded-stack data for a token-spend flamegraph (CLAUDE.md §2.4, roadmap v0.3).
//!
//! Answers "*where did the tokens (or dollars) go?*" as a hierarchy the binary
//! can hand straight to inferno: each leaf is one path with an integer weight.
//! The path's levels are chosen by [`Dim`], defaulting to `project → session →
//! model → token-type` ([`Dim::DEFAULT`]) but reorderable/droppable via the
//! binary's `--group-by`. This is the `-core` half only — it produces the
//! weighted "folded stacks"; the binary owns the SVG rendering and has the
//! inferno dependency (this crate must never gain one).
//!
//! Like every accounting pass here, weights are computed over DEDUPLICATED
//! requests, never raw JSONL lines — request-level dedup is THE accounting step
//! (CLAUDE.md §8.1). This pass reuses [`super::dedup::dedup_session`] and
//! [`crate::pricing`] exactly like [`super::aggregate`] / [`super::anomaly`], so
//! a flamegraph's widths agree with the `summary` numbers.
//!
//! The `session` frame uses the request's parent-session id when present, which
//! FOLDS sub-agent (sidechain) transcripts into their parent session's slice —
//! spawned work is attributed, never dropped (§8.3), matching `aggregate` and
//! `drilldown` — then shortens it to a readable prefix ([`short_session`]).
//!
//! Two metrics (see [`FlameMetric`]):
//!
//! - **Tokens** — deduplicated token counts. Always known, never a pricing guess
//!   (§8.7); unknown fields contribute 0 (absence ≠ zero, §8.5).
//! - **Cost** — estimated USD in MICRO-dollars (1e-6 USD) so the weight stays an
//!   integer. Priced from the embedded snapshot; a request on an unpriced model
//!   contributes nothing and is counted in [`FlameData::unpriced_requests`]
//!   rather than guessed at (§8.7). Per-token-type cost mirrors
//!   [`crate::pricing::cost_usd`] exactly so the two never drift.

use std::collections::BTreeMap;

use jiff::civil::Date;
use serde::Serialize;

use crate::analysis::aggregate::Filter;
use crate::analysis::dedup::dedup_session;
use crate::model::{Session, Usage};
use crate::pricing::{self, PricingTable};

/// Fallback `project` frame when a record has no project label.
const UNKNOWN_PROJECT: &str = "(unknown project)";
/// Fallback `model` frame when a record has no model.
const UNKNOWN_MODEL: &str = "(unknown model)";

/// What the width of a flamegraph frame measures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
pub enum FlameMetric {
    /// Deduplicated token counts. Always known; never a pricing guess (§8.7).
    #[default]
    Tokens,
    /// Estimated USD cost, in MICRO-dollars (1e-6 USD) so it stays an integer
    /// weight. Priced from the embedded snapshot; unpriced requests contribute
    /// nothing and are counted in `unpriced_requests` (§8.5/§8.7).
    Cost,
}

/// One level (frame "row") of the flamegraph hierarchy, outermost first. The
/// default is `Project → Session → Model → Type`; the binary's `--group-by`
/// lets a user reorder or drop levels — e.g. dropping [`Dim::Session`] (the
/// opaque session-id row) gives a cleaner `project → model → token-type` view.
///
/// [`Dim::Type`] is the per-request token-type split (input / output /
/// cache-read / cache-write / thinking). When it is omitted, those five are
/// SUMMED into the innermost structural frame instead of broken out — totals
/// are unchanged either way (regrouping never drops spend).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Dim {
    Project,
    Session,
    Model,
    Type,
}

impl Dim {
    /// The default hierarchy: `Project → Session → Model → Type`.
    pub const DEFAULT: [Dim; 4] = [Dim::Project, Dim::Session, Dim::Model, Dim::Type];
}

/// Human-readable session-frame label: the first 8 characters of the id — for a
/// UUID, the first hyphen-delimited group (e.g. `3e9d2c41`). Full session UUIDs
/// are unreadable in narrow frames and add nothing at a glance; 8 hex chars tell
/// sessions apart. A prefix collision would only merge two slices *visually* —
/// every leaf weight is still counted, so totals are unaffected (and a clash is
/// astronomically unlikely for realistic session counts anyway).
fn short_session(id: &str) -> String {
    id.chars().take(8).collect()
}

/// One folded-stack line: a base→leaf frame path and its integer weight.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FoldedStack {
    /// Frames from base (root) to leaf, e.g.
    /// ["-home-dev-acme-app", "<session-id>", "claude-sonnet-4-5", "cache-read"].
    pub frames: Vec<String>,
    /// Weight in the chosen metric's unit (tokens, or micro-USD for cost).
    pub value: u64,
}

impl FoldedStack {
    /// Render as an inferno folded line: `frame;frame;… value`.
    /// (Frame labels are sanitized so `;`/whitespace can't corrupt the format.)
    pub fn to_folded_line(&self) -> String {
        let mut line = String::new();
        for (i, frame) in self.frames.iter().enumerate() {
            if i > 0 {
                line.push(';');
            }
            line.push_str(&sanitize_frame(frame));
        }
        line.push(' ');
        line.push_str(&self.value.to_string());
        line
    }
}

/// Aggregated, deduplicated flamegraph data.
#[derive(Debug, Clone, Serialize)]
pub struct FlameData {
    pub agent: String,
    pub since: Option<Date>,
    pub metric: FlameMetric,
    /// Count-name label for the unit, e.g. "tokens" or "µ$". (`&'static str`.)
    pub unit: &'static str,
    /// Folded stacks, accumulated per unique frame-path, sorted deterministically.
    pub stacks: Vec<FoldedStack>,
    /// Sum of all `stacks[].value` (matches the emitted stacks exactly).
    pub total_value: u64,
    /// Requests dropped from a Cost flamegraph because their model is unpriced.
    /// Always 0 for the Tokens metric.
    pub unpriced_requests: u64,
}

/// The five token-type leaves, paired with how each is weighted under a metric.
///
/// Tuple is `(leaf label, tokens, USD-per-million rate)`. The rate is only used
/// by the Cost metric; for Tokens the weight is the token count itself. The
/// cache-write leaf is handled separately because its rate depends on the
/// reported 5m/1h TTL split (see [`cache_write_micro_usd`]).
struct Leaf {
    label: &'static str,
    /// Weight under the Tokens metric (the raw token count, unknown → 0).
    tokens: u64,
    /// Weight under the Cost metric, in micro-USD. Pre-computed so the caller
    /// need not re-branch on the metric per leaf.
    micro_usd: f64,
}

/// Replace inferno's frame separator (`;`) and any ASCII whitespace in a label
/// so a stray character can't corrupt the `frame;frame value` line format. Real
/// labels (url-encoded project dirs, uuids, model ids, the fixed token-type
/// words) contain none of these; this is defensive per the house "lenient" style.
fn sanitize_frame(frame: &str) -> String {
    frame
        .chars()
        .map(|c| match c {
            ';' => ':',
            c if c.is_ascii_whitespace() => '_',
            c => c,
        })
        .collect()
}

/// Cache-write cost in micro-USD, mirroring [`crate::pricing::cost_usd`] exactly:
/// use the per-TTL split when the agent reports it, else price the whole total at
/// the (cheaper) 5m rate so an unsplit figure stays a lower bound.
fn cache_write_micro_usd(usage: &Usage, p: &pricing::ModelPricing) -> f64 {
    match (usage.cache_creation_5m, usage.cache_creation_1h) {
        (None, None) => usage.cache_creation.unwrap_or(0) as f64 * p.cache_write_5m,
        (m5, h1) => {
            m5.unwrap_or(0) as f64 * p.cache_write_5m + h1.unwrap_or(0) as f64 * p.cache_write_1h
        }
    }
}

/// The five token-type leaves for one request's usage under both metrics.
///
/// Cost weights are in micro-USD; because the snapshot rates are USD per million,
/// `tokens * rate` already yields micro-USD. Thinking bills at the output rate,
/// mirroring [`crate::pricing::cost_usd`]. `cache-write` uses the request total
/// (the 5m/1h fields are a breakdown of it, not added again — §8.4).
fn leaves(usage: &Usage, pricing: Option<&pricing::ModelPricing>) -> [Leaf; 5] {
    let input = usage.input.unwrap_or(0);
    let output = usage.output.unwrap_or(0);
    let cache_read = usage.cache_read.unwrap_or(0);
    let cache_write = usage.cache_creation.unwrap_or(0);
    let thinking = usage.thinking.unwrap_or(0);

    // Cost weights are 0 unless the model is priced; callers skip whole records
    // for unpriced models, so these are only consulted for priced ones.
    let (c_in, c_out, c_read, c_write, c_think) = match pricing {
        Some(p) => (
            input as f64 * p.input,
            output as f64 * p.output,
            cache_read as f64 * p.cache_read,
            cache_write_micro_usd(usage, p),
            thinking as f64 * p.output,
        ),
        None => (0.0, 0.0, 0.0, 0.0, 0.0),
    };

    [
        Leaf {
            label: "input",
            tokens: input,
            micro_usd: c_in,
        },
        Leaf {
            label: "output",
            tokens: output,
            micro_usd: c_out,
        },
        Leaf {
            label: "cache-read",
            tokens: cache_read,
            micro_usd: c_read,
        },
        Leaf {
            label: "cache-write",
            tokens: cache_write,
            micro_usd: c_write,
        },
        Leaf {
            label: "thinking",
            tokens: thinking,
            micro_usd: c_think,
        },
    ]
}

/// Fold sessions into flamegraph data. See module docs for the rules.
///
/// Each session is reduced to deduplicated requests via [`dedup_session`] (THE
/// accounting step, §8.1 — `filter.since` filtering happens inside it). Every
/// request contributes up to five leaves; `dims` chooses the frame path each
/// leaf lands on (outermost first), defaulting to
/// `project → session → model → token-type` ([`Dim::DEFAULT`]). The `Session`
/// frame folds sub-agent transcripts into their parent (§8.3) and is shortened
/// to a readable prefix ([`short_session`]); omitting [`Dim::Type`] sums the
/// token-types into the innermost structural frame. Under the Cost metric, a
/// request whose model is unpriced is skipped entirely and counted in
/// `unpriced_requests` — prices are never guessed (§8.7).
pub fn fold(
    sessions: &[Session],
    filter: &Filter,
    agent: &str,
    metric: FlameMetric,
    dims: &[Dim],
    pricing_table: &PricingTable,
) -> FlameData {
    // Accumulate weights per unique 4-frame path. A BTreeMap keeps the emitted
    // stacks in deterministic sorted order for free. f64 keeps cross-request
    // cost accumulation exact (we round only on emit); for tokens the values are
    // integers well below 2^53, so f64 is exact there too.
    let mut paths: BTreeMap<Vec<String>, f64> = BTreeMap::new();
    let mut unpriced_requests = 0u64;

    for session in sessions {
        let (records, _stats) = dedup_session(session, filter.since);
        for rec in records {
            // Cost: refuse to price an unknown model — count it and skip (§8.7).
            let pricing = match metric {
                FlameMetric::Tokens => None,
                FlameMetric::Cost => {
                    match rec.model.as_deref().and_then(|m| pricing_table.lookup(m)) {
                        Some(p) => Some(p),
                        None => {
                            unpriced_requests += 1;
                            continue;
                        }
                    }
                }
            };

            // Structural frame values for this request. `session` folds sub-agent
            // transcripts into their parent (§8.3) and is shortened to a readable
            // prefix; project/model fall back to explicit "unknown" labels.
            let project = rec
                .project
                .clone()
                .unwrap_or_else(|| UNKNOWN_PROJECT.into());
            let session_label =
                short_session(rec.parent_session.as_deref().unwrap_or(&rec.session_id));
            let model = rec.model.clone().unwrap_or_else(|| UNKNOWN_MODEL.into());

            for leaf in leaves(&rec.usage, pricing) {
                let weight = match metric {
                    FlameMetric::Tokens => leaf.tokens as f64,
                    FlameMetric::Cost => leaf.micro_usd,
                };
                // Only non-empty leaves matter; zeros are dropped on emit anyway.
                if weight <= 0.0 {
                    continue;
                }
                // Build the frame path from the selected dimensions, in order.
                // Omitting `Type` makes every leaf of a request share one path,
                // so the five token-types sum into the innermost structural frame.
                let frames: Vec<String> = dims
                    .iter()
                    .map(|dim| match dim {
                        Dim::Project => project.clone(),
                        Dim::Session => session_label.clone(),
                        Dim::Model => model.clone(),
                        Dim::Type => leaf.label.to_string(),
                    })
                    .collect();
                *paths.entry(frames).or_insert(0.0) += weight;
            }
        }
    }

    // Emit in sorted (ascending frame-path) order; round each weight to an
    // integer here so cross-request accumulation never compounds rounding error.
    // total_value sums the EMITTED values, so it equals the stacks exactly.
    let mut total_value = 0u64;
    let mut stacks = Vec::with_capacity(paths.len());
    for (frames, weight) in paths {
        let value = weight.round() as u64;
        if value == 0 {
            continue;
        }
        total_value += value;
        stacks.push(FoldedStack { frames, value });
    }

    FlameData {
        agent: agent.to_string(),
        since: filter.since,
        metric,
        unit: match metric {
            FlameMetric::Tokens => "tokens",
            FlameMetric::Cost => "µ$",
        },
        stacks,
        total_value,
        unpriced_requests,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Event, EventKind, Usage};

    /// The default `project → session → model → token-type` hierarchy, as the CLI
    /// passes it when `--group-by` is omitted.
    const DIMS: &[Dim] = &Dim::DEFAULT;

    /// Assistant event with one request id and the given full usage breakdown.
    /// Model is the priced `claude-sonnet-4-5` unless a test overrides it.
    fn turn(rid: &str, usage: Usage) -> Event {
        Event {
            kind: EventKind::Assistant,
            ts: Some("2026-06-02T10:00:00Z".parse().unwrap()),
            request_id: Some(rid.into()),
            model: Some("claude-sonnet-4-5".into()),
            usage: Some(usage),
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

    fn usage(input: u64, output: u64, cache_read: u64, cache_write: u64) -> Usage {
        Usage {
            input: Some(input),
            output: Some(output),
            cache_read: Some(cache_read),
            cache_creation: Some(cache_write),
            ..Usage::default()
        }
    }

    /// Look up the weight of one full frame-path in the emitted stacks.
    fn leaf_value(
        d: &FlameData,
        project: &str,
        session: &str,
        model: &str,
        ty: &str,
    ) -> Option<u64> {
        let want = [project, session, model, ty];
        d.stacks.iter().find(|s| s.frames == want).map(|s| s.value)
    }

    /// Basic tokens fold: one request's four non-zero leaves appear under
    /// `proj;s;model;type` with the right values, zero leaves are omitted, and
    /// `total_value` sums the emitted stacks.
    #[test]
    fn basic_tokens_fold_produces_per_type_leaves() {
        let s = session("s", None, vec![turn("r", usage(1_000, 200, 5_000, 300))]);
        let d = fold(
            &[s],
            &Filter::default(),
            "claude-code",
            FlameMetric::Tokens,
            DIMS,
            &PricingTable::embedded(),
        );

        assert_eq!(d.unit, "tokens");
        assert_eq!(d.unpriced_requests, 0);
        let m = "claude-sonnet-4-5";
        assert_eq!(leaf_value(&d, "proj", "s", m, "input"), Some(1_000));
        assert_eq!(leaf_value(&d, "proj", "s", m, "output"), Some(200));
        assert_eq!(leaf_value(&d, "proj", "s", m, "cache-read"), Some(5_000));
        assert_eq!(leaf_value(&d, "proj", "s", m, "cache-write"), Some(300));
        // No thinking tokens -> that leaf is dropped (only four stacks).
        assert_eq!(d.stacks.len(), 4);
        assert!(leaf_value(&d, "proj", "s", m, "thinking").is_none());
        assert_eq!(d.total_value, 1_000 + 200 + 5_000 + 300);
        let stack_sum: u64 = d.stacks.iter().map(|s| s.value).sum();
        assert_eq!(d.total_value, stack_sum);
    }

    /// Going through `dedup_session`: three lines sharing one requestId collapse
    /// to one request, so the leaf values are NOT multiplied by three.
    #[test]
    fn dedup_collapses_repeated_lines() {
        let u = usage(1_000, 200, 0, 0);
        let s = session(
            "s",
            None,
            vec![turn("req", u), turn("req", u), turn("req", u)],
        );
        let d = fold(
            &[s],
            &Filter::default(),
            "claude-code",
            FlameMetric::Tokens,
            DIMS,
            &PricingTable::embedded(),
        );
        let m = "claude-sonnet-4-5";
        assert_eq!(leaf_value(&d, "proj", "s", m, "input"), Some(1_000));
        assert_eq!(leaf_value(&d, "proj", "s", m, "output"), Some(200));
        assert_eq!(d.total_value, 1_200, "3 lines -> 1 request, not 3x");
    }

    /// §8.3: a sub-agent (sidechain) transcript folds into its PARENT's session
    /// frame, so the parent and child contributions sum under the same leaf.
    #[test]
    fn subagent_folds_into_parent_session_frame() {
        let parent = session("parent", None, vec![turn("p", usage(1_000, 0, 0, 0))]);
        let child = session(
            "agent-x",
            Some("parent"),
            vec![turn("c", usage(500, 0, 0, 0))],
        );
        let d = fold(
            &[parent, child],
            &Filter::default(),
            "claude-code",
            FlameMetric::Tokens,
            DIMS,
            &PricingTable::embedded(),
        );
        let m = "claude-sonnet-4-5";
        // Both land on session frame "parent"; the child's own id never appears.
        assert_eq!(
            leaf_value(&d, "proj", "parent", m, "input"),
            Some(1_500),
            "parent + child fold into one leaf"
        );
        assert!(leaf_value(&d, "proj", "agent-x", m, "input").is_none());
        assert_eq!(d.total_value, 1_500);
    }

    /// Cost metric on a priced model: each leaf is micro-USD = tokens * rate, and
    /// the SUM of the request's cost leaves matches `pricing::cost_usd * 1e6`
    /// (guards against drift from the pricing module).
    #[test]
    fn cost_metric_prices_each_leaf() {
        let u = usage(1_000, 100, 2_000, 400);
        let s = session("s", None, vec![turn("r", u)]);
        let d = fold(
            &[s],
            &Filter::default(),
            "claude-code",
            FlameMetric::Cost,
            DIMS,
            &PricingTable::embedded(),
        );

        assert_eq!(d.unit, "µ$");
        assert_eq!(d.unpriced_requests, 0);
        let m = "claude-sonnet-4-5";
        // sonnet-4-5: input 3.0, output 15.0, cache_read 0.30, cache_write_5m 3.75
        // (USD per million == micro-USD per token).
        assert_eq!(leaf_value(&d, "proj", "s", m, "input"), Some(3_000)); // 1000 * 3.0
        assert_eq!(leaf_value(&d, "proj", "s", m, "output"), Some(1_500)); // 100 * 15.0
        assert_eq!(leaf_value(&d, "proj", "s", m, "cache-read"), Some(600)); // 2000 * 0.30
        assert_eq!(leaf_value(&d, "proj", "s", m, "cache-write"), Some(1_500)); // 400 * 3.75

        // Cross-check the per-request total against the authoritative pricer.
        let expected = pricing::cost_usd(Some("claude-sonnet-4-5"), &u).unwrap() * 1e6;
        let diff = (d.total_value as f64 - expected).abs();
        assert!(
            diff <= 3.0,
            "flame cost {} vs pricer {expected}",
            d.total_value
        );
    }

    /// Thinking tokens bill at the OUTPUT rate under the Cost metric (mirrors
    /// `pricing::cost_usd`), and the leaf is labeled `thinking`.
    #[test]
    fn cost_metric_thinking_bills_at_output_rate() {
        let u = Usage {
            thinking: Some(1_000),
            ..Usage::default()
        };
        let s = session("s", None, vec![turn("r", u)]);
        let d = fold(
            &[s],
            &Filter::default(),
            "claude-code",
            FlameMetric::Cost,
            DIMS,
            &PricingTable::embedded(),
        );
        // 1000 thinking tokens * output rate 15.0 = 15_000 micro-USD.
        assert_eq!(
            leaf_value(&d, "proj", "s", "claude-sonnet-4-5", "thinking"),
            Some(15_000)
        );
    }

    /// §8.7: a request on an unpriced model produces NO stacks under the Cost
    /// metric and is counted; a mixed session keeps only the priced request.
    #[test]
    fn cost_metric_skips_and_counts_unpriced() {
        // Pure unpriced session: no stacks, one unpriced request.
        let mut unp = turn("u", usage(5_000, 100, 0, 0));
        unp.model = Some("claude-opus-5-0".into()); // post-snapshot, unpriced
        let only_unpriced = session("u", None, vec![unp.clone()]);
        let d = fold(
            &[only_unpriced],
            &Filter::default(),
            "claude-code",
            FlameMetric::Cost,
            DIMS,
            &PricingTable::embedded(),
        );
        assert!(d.stacks.is_empty());
        assert_eq!(d.total_value, 0);
        assert_eq!(d.unpriced_requests, 1);

        // Mixed: the priced request appears, the unpriced one is only counted.
        let priced = turn("p", usage(1_000, 0, 0, 0)); // sonnet-4-5
        let mixed = session("mix", None, vec![unp, priced]);
        let d = fold(
            &[mixed],
            &Filter::default(),
            "claude-code",
            FlameMetric::Cost,
            DIMS,
            &PricingTable::embedded(),
        );
        assert_eq!(d.unpriced_requests, 1);
        assert_eq!(d.stacks.len(), 1, "only the priced request contributes");
        assert_eq!(
            leaf_value(&d, "proj", "mix", "claude-sonnet-4-5", "input"),
            Some(3_000)
        );
    }

    /// Unpriced never affects the Tokens metric: `unpriced_requests` stays 0 and
    /// the tokens are still counted (§8.7 — tokens are never a pricing guess).
    #[test]
    fn tokens_metric_ignores_pricing() {
        let mut unp = turn("u", usage(5_000, 100, 0, 0));
        unp.model = Some("claude-opus-5-0".into());
        let s = session("s", None, vec![unp]);
        let d = fold(
            &[s],
            &Filter::default(),
            "claude-code",
            FlameMetric::Tokens,
            DIMS,
            &PricingTable::embedded(),
        );
        assert_eq!(d.unpriced_requests, 0);
        assert_eq!(
            leaf_value(&d, "proj", "s", "claude-opus-5-0", "input"),
            Some(5_000)
        );
    }

    /// `since` flows through to `dedup_session`: out-of-window requests drop.
    #[test]
    fn since_filter_excludes_out_of_window_requests() {
        let mut old = turn("old", usage(9_999, 0, 0, 0));
        old.ts = Some("2026-06-01T10:00:00Z".parse().unwrap());
        let mut new = turn("new", usage(100, 0, 0, 0));
        new.ts = Some("2026-06-02T10:00:00Z".parse().unwrap());
        let s = session("s", None, vec![old, new]);
        let filter = Filter {
            since: Some("2026-06-02".parse().unwrap()),
        };
        let d = fold(
            &[s],
            &filter,
            "claude-code",
            FlameMetric::Tokens,
            DIMS,
            &PricingTable::embedded(),
        );
        assert_eq!(d.since, filter.since);
        assert_eq!(d.total_value, 100, "only the in-window request remains");
        assert_eq!(
            leaf_value(&d, "proj", "s", "claude-sonnet-4-5", "input"),
            Some(100)
        );
    }

    /// Empty input -> an empty but well-formed result.
    #[test]
    fn empty_input_is_an_empty_result() {
        let d = fold(
            &[],
            &Filter::default(),
            "claude-code",
            FlameMetric::Tokens,
            DIMS,
            &PricingTable::embedded(),
        );
        assert!(d.stacks.is_empty());
        assert_eq!(d.total_value, 0);
        assert_eq!(d.unpriced_requests, 0);
    }

    /// `to_folded_line` yields `a;b;c;d value`, and sanitizes any `;` (→ `:`) or
    /// ASCII whitespace (→ `_`) so it cannot corrupt inferno's line format.
    #[test]
    fn to_folded_line_formats_and_sanitizes() {
        let clean = FoldedStack {
            frames: vec!["a".into(), "b".into(), "c".into(), "input".into()],
            value: 1_234,
        };
        assert_eq!(clean.to_folded_line(), "a;b;c;input 1234");

        let dirty = FoldedStack {
            frames: vec!["a;b".into(), "c d".into()],
            value: 7,
        };
        assert_eq!(dirty.to_folded_line(), "a:b;c_d 7");
    }

    /// The emitted stacks come out in ascending frame-path order (BTreeMap),
    /// giving deterministic output across runs.
    #[test]
    fn stacks_are_emitted_in_sorted_order() {
        // Two projects, two sessions, multiple leaves -> several stacks.
        let mut a = session("sa", None, vec![turn("ra", usage(10, 20, 30, 40))]);
        a.project = Some("zeta".into());
        let mut b = session("sb", None, vec![turn("rb", usage(1, 2, 3, 4))]);
        b.project = Some("alpha".into());
        let d = fold(
            &[a, b],
            &Filter::default(),
            "claude-code",
            FlameMetric::Tokens,
            DIMS,
            &PricingTable::embedded(),
        );

        assert!(d.stacks.len() >= 2);
        for w in d.stacks.windows(2) {
            assert!(w[0].frames <= w[1].frames, "stacks must be sorted");
        }
        // "alpha" sorts before "zeta" at the project frame.
        assert_eq!(d.stacks[0].frames[0], "alpha");
    }

    /// The session frame is shortened to a readable 8-char prefix; the full UUID
    /// never appears as a frame, and the total is unchanged.
    #[test]
    fn session_frame_is_shortened_to_prefix() {
        let id = "3e9d2c41-7b5a-4f2e-9c1d-2f6b8a1c0e11";
        let s = session(id, None, vec![turn("r", usage(1_000, 0, 0, 0))]);
        let d = fold(
            &[s],
            &Filter::default(),
            "claude-code",
            FlameMetric::Tokens,
            DIMS,
            &PricingTable::embedded(),
        );
        let m = "claude-sonnet-4-5";
        assert_eq!(leaf_value(&d, "proj", "3e9d2c41", m, "input"), Some(1_000));
        assert!(
            !d.stacks.iter().any(|s| s.frames.iter().any(|f| f == id)),
            "the full UUID must never appear as a frame"
        );
        assert_eq!(d.total_value, 1_000);
    }

    /// `--group-by project,model,type` drops the session level: every path is a
    /// 3-frame `project;model;type`, no session prefix appears, and the grand
    /// total is unchanged (regrouping never drops spend).
    #[test]
    fn dims_drop_session_level() {
        let id = "3e9d2c41-7b5a-4f2e-9c1d-2f6b8a1c0e11";
        let s = session(id, None, vec![turn("r", usage(1_000, 200, 0, 0))]);
        let dims = &[Dim::Project, Dim::Model, Dim::Type];
        let d = fold(
            &[s],
            &Filter::default(),
            "claude-code",
            FlameMetric::Tokens,
            dims,
            &PricingTable::embedded(),
        );
        assert!(d.stacks.iter().all(|s| s.frames.len() == 3));
        assert!(
            !d.stacks
                .iter()
                .any(|s| s.frames.iter().any(|f| f == "3e9d2c41")),
            "session level dropped"
        );
        let m = "claude-sonnet-4-5";
        let input = d.stacks.iter().find(|s| s.frames == ["proj", m, "input"]);
        assert_eq!(input.map(|s| s.value), Some(1_000));
        assert_eq!(d.total_value, 1_200);
    }

    /// Omitting `Type` sums the token-types into the innermost structural frame:
    /// a single `project;model` stack whose weight is every type added together.
    #[test]
    fn omitting_type_sums_token_types() {
        let s = session("s", None, vec![turn("r", usage(1_000, 200, 50, 0))]);
        let dims = &[Dim::Project, Dim::Model];
        let d = fold(
            &[s],
            &Filter::default(),
            "claude-code",
            FlameMetric::Tokens,
            dims,
            &PricingTable::embedded(),
        );
        let m = "claude-sonnet-4-5";
        assert_eq!(d.stacks.len(), 1);
        assert_eq!(d.stacks[0].frames, ["proj", m]);
        assert_eq!(d.stacks[0].value, 1_250, "1000 + 200 + 50 summed");
        assert_eq!(d.total_value, 1_250);
    }
}
