//! Rollups of DEDUPLICATED usage by session / model / day / tool, with cost.
//!
//! Inputs are the per-request records from [`super::dedup`] — never raw events
//! (CLAUDE.md §8.1). Sub-agent transcripts are folded into their parent
//! session's row so spawned work is attributed, not dropped (§8.3). Day
//! bucketing is UTC (see [`super::utc_date`]).

use std::collections::{BTreeMap, BTreeSet};

use jiff::civil::Date;
use jiff::Timestamp;
use serde::Serialize;

use crate::analysis::dedup::{dedup_session, DedupStats, UsageRecord};
use crate::analysis::utc_date;
use crate::model::{EventKind, Session};
use crate::pricing::PricingTable;

/// Aggregation filters.
#[derive(Debug, Default, Clone, Copy)]
pub struct Filter {
    /// Only count events on/after this UTC date.
    pub since: Option<Date>,
}

/// Token + cost rollup over a set of deduplicated requests.
#[derive(Debug, Default, Clone, Serialize)]
pub struct Rollup {
    pub requests: u64,
    pub input: u64,
    pub output: u64,
    pub cache_creation: u64,
    pub cache_read: u64,
    /// Requests with at least one unknown core usage field — the sums above are
    /// best-effort LOWER BOUNDS for those (absence ≠ zero, §8.5).
    pub incomplete_requests: u64,
    /// USD cost of the requests whose model has a known price.
    pub cost_usd: f64,
    /// Requests that could not be priced (model unknown or not in the snapshot).
    pub unpriced_requests: u64,
}

impl Rollup {
    pub(crate) fn add(&mut self, rec: &UsageRecord, pricing: &PricingTable) {
        self.requests += 1;
        let u = &rec.usage;
        self.input += u.input.unwrap_or(0);
        self.output += u.output.unwrap_or(0);
        self.cache_creation += u.cache_creation.unwrap_or(0);
        self.cache_read += u.cache_read.unwrap_or(0);
        if u.input.is_none()
            || u.output.is_none()
            || u.cache_creation.is_none()
            || u.cache_read.is_none()
        {
            self.incomplete_requests += 1;
        }
        match pricing.cost_usd(rec.model.as_deref(), u) {
            Some(cost) => self.cost_usd += cost,
            None => self.unpriced_requests += 1,
        }
    }

    pub fn total_tokens(&self) -> u64 {
        self.input + self.output + self.cache_creation + self.cache_read
    }
}

/// Per-session line in the summary. Sub-agent transcript spend is folded in.
#[derive(Debug, Clone, Serialize)]
pub struct SessionSummary {
    pub id: String,
    pub project: Option<String>,
    pub model: Option<String>,
    pub started_at: Option<Timestamp>,
    pub rollup: Rollup,
    /// Sub-agents attributed to this session (spawn tool calls / folded
    /// transcripts, whichever evidence is stronger).
    pub sub_agents: u64,
    /// Known tokens contributed by folded sub-agent transcripts.
    pub sub_agent_tokens: u64,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct ToolStat {
    pub calls: u64,
    pub input_bytes: u64,
    /// MCP server, when the tool name carries one (`mcp__<server>__...`).
    pub server: Option<String>,
}

/// Everything the renderers (table / JSON / TUI) need.
#[derive(Debug, Serialize)]
pub struct Summary {
    pub agent: String,
    pub generated_at: Timestamp,
    pub since: Option<Date>,
    pub sessions_parsed: u64,
    pub sessions_failed: u64,
    /// Unrecognized/corrupt lines skipped leniently across all files.
    pub skipped_lines: u64,
    pub dedup: DedupStats,
    /// Grand totals (deduplicated, includes sub-agent spend).
    pub totals: Rollup,
    /// The sub-agent (sidechain) share of `totals`.
    pub sidechain_totals: Rollup,
    pub by_model: BTreeMap<String, Rollup>,
    /// Keyed by UTC date (`YYYY-MM-DD`).
    pub by_day: BTreeMap<String, Rollup>,
    /// Sorted by cost, then tokens, descending. Sub-agent transcripts are folded
    /// into their parent's row.
    pub by_session: Vec<SessionSummary>,
    pub by_tool: BTreeMap<String, ToolStat>,
    /// Models we refused to price (not in the embedded snapshot — §8.7 forbids
    /// guessing).
    pub unpriced_models: BTreeSet<String>,
    pub compactions: u64,
}

/// Internal accumulator for a session row (parent + folded children).
#[derive(Default)]
struct SessionAcc {
    project: Option<String>,
    model: Option<String>,
    started_at: Option<Timestamp>,
    rollup: Rollup,
    spawn_calls: u64,
    children_folded: u64,
    sub_agent_tokens: u64,
}

/// Roll deduplicated usage up into a [`Summary`].
pub fn aggregate(
    sessions: &[Session],
    filter: &Filter,
    sessions_failed: u64,
    agent: &str,
    pricing: &PricingTable,
) -> Summary {
    let mut summary = Summary {
        agent: agent.to_string(),
        generated_at: Timestamp::now(),
        since: filter.since,
        sessions_parsed: sessions.len() as u64,
        sessions_failed,
        skipped_lines: 0,
        dedup: DedupStats::default(),
        totals: Rollup::default(),
        sidechain_totals: Rollup::default(),
        by_model: BTreeMap::new(),
        by_day: BTreeMap::new(),
        by_session: Vec::new(),
        by_tool: BTreeMap::new(),
        unpriced_models: BTreeSet::new(),
        compactions: 0,
    };
    let mut rows: BTreeMap<String, SessionAcc> = BTreeMap::new();

    let in_window = |ts: Option<Timestamp>| match (filter.since, ts) {
        (None, _) => true,
        (Some(since), Some(ts)) => utc_date(ts) >= since,
        (Some(_), None) => false,
    };

    for session in sessions {
        summary.skipped_lines += session.skipped_lines;

        let (records, stats) = dedup_session(session, filter.since);
        summary.dedup.duplicate_lines_collapsed += stats.duplicate_lines_collapsed;
        summary.dedup.naive_known_tokens += stats.naive_known_tokens;
        summary.dedup.requests_with_thinking += stats.requests_with_thinking;
        summary.dedup.requests_with_encrypted_thinking += stats.requests_with_encrypted_thinking;
        summary.dedup.thinking_chars_total += stats.thinking_chars_total;

        // Tool + compaction stats come straight from events (they carry no usage).
        for event in &session.events {
            if !in_window(event.ts) {
                continue;
            }
            if event.kind == EventKind::Compaction {
                summary.compactions += 1;
            }
            for call in &event.tool_calls {
                let stat = summary.by_tool.entry(call.name.clone()).or_default();
                stat.calls += 1;
                stat.input_bytes += call.input_bytes;
                if stat.server.is_none() {
                    stat.server.clone_from(&call.server);
                }
            }
        }

        // Attribute the whole transcript to the parent session when this is a
        // sub-agent file (§8.3); otherwise to itself.
        let row_id = session
            .parent_session
            .clone()
            .unwrap_or_else(|| session.id.clone());
        let row = rows.entry(row_id).or_default();
        if session.parent_session.is_some() {
            row.children_folded += 1;
        } else {
            // Parent metadata wins over anything a child filled in earlier.
            row.project.clone_from(&session.project);
            row.model.clone_from(&session.model);
            row.started_at = session.started_at;
            row.spawn_calls += session.sub_agents.len() as u64;
        }
        if row.project.is_none() {
            row.project.clone_from(&session.project);
        }

        for rec in &records {
            summary.totals.add(rec, pricing);
            row.rollup.add(rec, pricing);
            if rec.sidechain {
                summary.sidechain_totals.add(rec, pricing);
                row.sub_agent_tokens += rec.usage.known_total();
            }
            let model_key = rec.model.clone().unwrap_or_else(|| "(unknown)".into());
            summary
                .by_model
                .entry(model_key)
                .or_default()
                .add(rec, pricing);
            if let Some(ts) = rec.ts {
                summary
                    .by_day
                    .entry(utc_date(ts).to_string())
                    .or_default()
                    .add(rec, pricing);
            }
            if let Some(model) = &rec.model {
                if pricing.lookup(model).is_none() {
                    summary.unpriced_models.insert(model.clone());
                }
            }
        }
    }

    summary.by_session = rows
        .into_iter()
        .filter(|(_, acc)| acc.rollup.requests > 0)
        .map(|(id, acc)| SessionSummary {
            id,
            project: acc.project,
            model: acc.model,
            started_at: acc.started_at,
            // A spawn call and a folded child transcript are usually the same
            // sub-agent seen from both sides — take the stronger evidence.
            sub_agents: acc.spawn_calls.max(acc.children_folded),
            sub_agent_tokens: acc.sub_agent_tokens,
            rollup: acc.rollup,
        })
        .collect();
    summary.by_session.sort_by(|a, b| {
        b.rollup
            .cost_usd
            .total_cmp(&a.rollup.cost_usd)
            .then_with(|| b.rollup.total_tokens().cmp(&a.rollup.total_tokens()))
            .then_with(|| a.id.cmp(&b.id))
    });
    summary
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Event, Usage};

    fn assistant_event(request_id: &str, ts: &str, input: u64) -> Event {
        Event {
            kind: EventKind::Assistant,
            ts: Some(ts.parse().unwrap()),
            request_id: Some(request_id.into()),
            model: Some("claude-sonnet-4-5".into()),
            usage: Some(Usage {
                input: Some(input),
                output: Some(10),
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

    fn base_session(id: &str, parent: Option<&str>, events: Vec<Event>) -> Session {
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

    /// §8.3: sub-agent transcripts fold into the parent row; nothing is dropped.
    #[test]
    fn child_transcripts_fold_into_parent_session() {
        let parent = base_session(
            "parent",
            None,
            vec![assistant_event("req_p", "2026-06-01T10:00:00Z", 1000)],
        );
        let child = base_session(
            "agent-x",
            Some("parent"),
            vec![assistant_event("req_c", "2026-06-01T10:01:00Z", 500)],
        );
        let summary = aggregate(
            &[parent, child],
            &Filter::default(),
            0,
            "claude-code",
            &PricingTable::embedded(),
        );

        assert_eq!(summary.by_session.len(), 1, "child folded, not a row");
        let row = &summary.by_session[0];
        assert_eq!(row.id, "parent");
        assert_eq!(row.rollup.input, 1500, "parent + child input");
        assert_eq!(row.sub_agents, 1);
        assert_eq!(summary.totals.input, 1500);
        assert_eq!(summary.sidechain_totals.input, 500);
    }

    #[test]
    fn since_is_inclusive_of_the_boundary_date() {
        let s = base_session(
            "s",
            None,
            vec![
                assistant_event("req_before", "2026-06-01T23:00:00Z", 111),
                assistant_event("req_on", "2026-06-02T00:00:00Z", 222),
            ],
        );
        let filter = Filter {
            since: Some("2026-06-02".parse().unwrap()),
        };
        let summary = aggregate(&[s], &filter, 0, "claude-code", &PricingTable::embedded());
        assert_eq!(summary.totals.requests, 1);
        assert_eq!(summary.totals.input, 222);
    }
}
