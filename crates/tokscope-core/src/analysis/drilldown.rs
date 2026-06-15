//! Per-session drill-down view-models for the TUI (session → turns → context).
//!
//! This module re-derives nothing: it composes the authoritative passes so the
//! interactive view shows the same numbers as the `summary`/`context` commands.
//!
//! - per-turn usage comes from [`super::dedup::dedup_session`] — request-level
//!   dedup is THE accounting step (CLAUDE.md §8.1);
//! - per-turn cost from [`crate::pricing::cost_usd`] (every figure traces to a
//!   unit price, §8.7);
//! - the per-session context-bloat breakdown from [`super::context::profile_refs`]
//!   (§2.2), scoped to just this session's files;
//! - sub-agent transcripts fold into their parent session exactly as
//!   [`super::aggregate`] does — same row id, nothing dropped (§8.3).
//!
//! The display-only fields on a [`Turn`] (a short content snippet, the tool names
//! touched in the request) are joined on by `request_id` and never influence
//! accounting — if the join misses, the number is still right, just unlabeled.

use std::collections::BTreeMap;

use jiff::Timestamp;
use serde::Serialize;

use crate::analysis::aggregate::{Filter, Rollup};
use crate::analysis::context::{profile_refs, ContextReport};
use crate::analysis::dedup::dedup_session;
use crate::model::{Event, EventKind, Session, Usage};
use crate::pricing;

/// One deduplicated API request in a session, enriched for display.
///
/// `usage` is the finalized per-request usage (post-dedup); `cost_usd` is `None`
/// when the model is unpriced (§8.7 — never guessed). `summary`/`tools` are
/// display-only and may be empty without affecting the numbers.
#[derive(Debug, Clone, Serialize)]
pub struct Turn {
    pub request_id: Option<String>,
    pub ts: Option<Timestamp>,
    pub model: Option<String>,
    pub usage: Usage,
    /// USD cost of this request, or `None` when the model is unpriced.
    pub cost_usd: Option<f64>,
    /// The request came from a sub-agent (sidechain) transcript folded into this
    /// session (§8.3).
    pub sidechain: bool,
    /// Short visible-text snippet for display (first non-empty across the
    /// request's lines), when available.
    pub summary: Option<String>,
    /// Tool names invoked in this request, in first-seen order (deduplicated).
    pub tools: Vec<String>,
    /// Thinking blocks were present (possibly encrypted/unmeasurable).
    pub has_thinking: bool,
    /// Thinking present but `output_tokens` looks too small to include it — the
    /// usage is a likely undercount, surfaced not hidden (§8.2).
    pub thinking_suspect: bool,
}

/// Everything the drill-down needs about one session row: the same folded,
/// sub-agent-inclusive accounting as a `summary` row, plus its turn list and its
/// context-bloat breakdown.
#[derive(Debug, Clone, Serialize)]
pub struct SessionDetail {
    pub id: String,
    pub project: Option<String>,
    pub model: Option<String>,
    pub started_at: Option<Timestamp>,
    pub ended_at: Option<Timestamp>,
    /// Token + cost rollup over all turns (parent + folded sub-agents), identical
    /// to this session's row in the `summary`.
    pub rollup: Rollup,
    /// Sub-agents attributed to this session (stronger of spawn calls / folded
    /// child transcripts), as in [`super::aggregate`].
    pub sub_agents: u64,
    /// Known tokens contributed by folded sub-agent transcripts.
    pub sub_agent_tokens: u64,
    /// Deduplicated requests, oldest first (undated last), sub-agent turns marked.
    pub turns: Vec<Turn>,
    /// Context-bloat breakdown scoped to this session's files (parent + children),
    /// reusing [`super::context::profile_refs`].
    pub context: ContextReport,
}

impl SessionDetail {
    /// Total deduplicated tokens for the row (sort key shared with `summary`).
    pub fn total_tokens(&self) -> u64 {
        self.rollup.total_tokens()
    }
}

/// Window predicate matching [`super::dedup`]/[`super::context`]: with `since`
/// set, an event is in range only if dated on/after it; undated events drop.
fn in_window(filter: &Filter, event: &Event) -> bool {
    match (filter.since, event.ts) {
        (None, _) => true,
        (Some(since), Some(ts)) => super::utc_date(ts) >= since,
        (Some(_), None) => false,
    }
}

/// Map `request_id -> (first content snippet, tool names)` over a session's
/// in-window assistant events, for display-only enrichment of dedup records.
fn enrichment(
    session: &Session,
    filter: &Filter,
) -> BTreeMap<String, (Option<String>, Vec<String>)> {
    let mut map: BTreeMap<String, (Option<String>, Vec<String>)> = BTreeMap::new();
    for event in &session.events {
        if event.kind != EventKind::Assistant || !in_window(filter, event) {
            continue;
        }
        let Some(rid) = &event.request_id else {
            continue;
        };
        let entry = map.entry(rid.clone()).or_default();
        if entry.0.is_none() {
            if let Some(s) = &event.content_summary {
                if !s.is_empty() {
                    entry.0 = Some(s.clone());
                }
            }
        }
        for call in &event.tool_calls {
            if !entry.1.contains(&call.name) {
                entry.1.push(call.name.clone());
            }
        }
    }
    map
}

/// Turn rows for a single session file, dedup-correct and enriched for display.
fn session_turns(session: &Session, filter: &Filter) -> Vec<Turn> {
    let enrich = enrichment(session, filter);
    let (records, _stats) = dedup_session(session, filter.since);
    records
        .into_iter()
        .map(|rec| {
            let (summary, tools) = rec
                .request_id
                .as_ref()
                .and_then(|rid| enrich.get(rid))
                .cloned()
                .unwrap_or_default();
            Turn {
                cost_usd: pricing::cost_usd(rec.model.as_deref(), &rec.usage),
                request_id: rec.request_id,
                ts: rec.ts,
                model: rec.model,
                usage: rec.usage,
                sidechain: rec.sidechain,
                summary,
                tools,
                has_thinking: rec.has_thinking,
                thinking_suspect: rec.thinking_suspect,
            }
        })
        .collect()
}

/// Accumulator for one session row (parent + folded sub-agent files).
struct Acc<'a> {
    files: Vec<&'a Session>,
    parent: Option<&'a Session>,
    spawn_calls: u64,
    children_folded: u64,
}

/// Build one [`SessionDetail`] per session row, folding sub-agent transcripts into
/// their parent (§8.3) and sorting the same way `summary` sorts its sessions:
/// cost desc, then total tokens desc, then id. Rows with no requests are dropped
/// (mirrors [`super::aggregate`]), so the list matches the `summary`.
pub fn build_details(sessions: &[Session], filter: &Filter, agent: &str) -> Vec<SessionDetail> {
    // Group by row id = parent_session.unwrap_or(id), exactly like `aggregate`.
    let mut rows: BTreeMap<String, Acc> = BTreeMap::new();
    for session in sessions {
        let row_id = session
            .parent_session
            .clone()
            .unwrap_or_else(|| session.id.clone());
        let acc = rows.entry(row_id).or_insert_with(|| Acc {
            files: Vec::new(),
            parent: None,
            spawn_calls: 0,
            children_folded: 0,
        });
        acc.files.push(session);
        if session.parent_session.is_some() {
            acc.children_folded += 1;
        } else {
            acc.parent = Some(session);
            acc.spawn_calls += session.sub_agents.len() as u64;
        }
    }

    let mut details: Vec<SessionDetail> = rows
        .into_iter()
        .filter_map(|(id, acc)| {
            // Metadata from the parent file when present, else the first file seen.
            let meta = acc.parent.or_else(|| acc.files.first().copied());

            let mut rollup = Rollup::default();
            let mut sub_agent_tokens = 0u64;
            let mut turns: Vec<Turn> = Vec::new();
            for file in &acc.files {
                let (records, _) = dedup_session(file, filter.since);
                for rec in &records {
                    rollup.add(rec);
                    if rec.sidechain {
                        sub_agent_tokens += rec.usage.known_total();
                    }
                }
                turns.extend(session_turns(file, filter));
            }

            // Drop empty rows so the list matches `summary` (which keeps requests>0).
            if rollup.requests == 0 {
                return None;
            }

            // Oldest first; undated turns sort last, stably.
            turns.sort_by(|a, b| match (a.ts, b.ts) {
                (Some(x), Some(y)) => x.cmp(&y),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => std::cmp::Ordering::Equal,
            });

            let context = profile_refs(&acc.files, agent, filter.since);

            Some(SessionDetail {
                id,
                project: meta.and_then(|s| s.project.clone()),
                model: meta.and_then(|s| s.model.clone()),
                started_at: meta.and_then(|s| s.started_at),
                ended_at: meta.and_then(|s| s.ended_at),
                // A spawn call and a folded child are usually the same sub-agent
                // seen from both sides — take the stronger evidence (as aggregate).
                sub_agents: acc.spawn_calls.max(acc.children_folded),
                sub_agent_tokens,
                turns,
                rollup,
                context,
            })
        })
        .collect();

    details.sort_by(|a, b| {
        b.rollup
            .cost_usd
            .total_cmp(&a.rollup.cost_usd)
            .then_with(|| b.total_tokens().cmp(&a.total_tokens()))
            .then_with(|| a.id.cmp(&b.id))
    });
    details
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ToolCall, Usage};

    fn ev(kind: EventKind) -> Event {
        Event {
            kind,
            ts: Some("2026-06-02T10:00:00Z".parse().unwrap()),
            request_id: None,
            model: Some("claude-sonnet-4-5".into()),
            usage: None,
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

    fn usage(input: u64, output: u64) -> Usage {
        Usage {
            input: Some(input),
            output: Some(output),
            cache_creation: Some(0),
            cache_read: Some(0),
            ..Usage::default()
        }
    }

    /// Assistant turn at a given timestamp with a request id and usage.
    fn turn(rid: &str, ts: &str, u: Usage) -> Event {
        let mut e = ev(EventKind::Assistant);
        e.request_id = Some(rid.into());
        e.ts = Some(ts.parse().unwrap());
        e.usage = Some(u);
        e
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

    /// §8.3: a sub-agent transcript folds into the parent row; turns from both
    /// appear, sub-agent turns marked, tokens attributed not dropped.
    #[test]
    fn child_folds_into_parent_with_marked_turns() {
        let mut p = turn("req_p", "2026-06-01T10:00:00Z", usage(1000, 200));
        p.content_summary = Some("parent does a thing".into());
        p.tool_calls = vec![ToolCall {
            id: "t1".into(),
            name: "Read".into(),
            input_bytes: 20,
            server: None,
        }];
        let parent = session("parent", None, vec![p]);
        let child = session(
            "agent-x",
            Some("parent"),
            vec![turn("req_c", "2026-06-01T10:05:00Z", usage(500, 50))],
        );

        let details = build_details(&[parent, child], &Filter::default(), "claude-code");
        assert_eq!(details.len(), 1, "child folded, not its own row");
        let d = &details[0];
        assert_eq!(d.id, "parent");
        assert_eq!(d.rollup.input, 1500, "parent 1000 + child 500");
        assert_eq!(d.sub_agents, 1);
        assert_eq!(d.sub_agent_tokens, 550, "child known_total = 500+50");

        assert_eq!(d.turns.len(), 2);
        // Oldest first: parent turn, then child turn.
        assert_eq!(d.turns[0].request_id.as_deref(), Some("req_p"));
        assert!(!d.turns[0].sidechain);
        assert_eq!(d.turns[0].summary.as_deref(), Some("parent does a thing"));
        assert_eq!(d.turns[0].tools, vec!["Read".to_string()]);
        assert_eq!(d.turns[1].request_id.as_deref(), Some("req_c"));
        assert!(d.turns[1].sidechain, "child turn marked as sub-agent");
    }

    /// Per-turn cost traces to the unit price; unpriced models surface as None.
    #[test]
    fn turn_cost_priced_and_unpriced() {
        let priced = session(
            "a",
            None,
            vec![turn("r", "2026-06-02T10:00:00Z", usage(1000, 0))],
        );
        let mut unp = turn("r2", "2026-06-02T10:00:00Z", usage(1000, 0));
        unp.model = Some("claude-opus-4-8".into()); // post-snapshot, unpriced
        let unpriced = session("b", None, vec![unp]);

        let details = build_details(&[priced, unpriced], &Filter::default(), "claude-code");
        let a = details.iter().find(|d| d.id == "a").unwrap();
        let b = details.iter().find(|d| d.id == "b").unwrap();
        // 1000 input * $3/M = $0.003.
        assert!((a.turns[0].cost_usd.unwrap() - 0.003).abs() < 1e-9);
        assert_eq!(b.turns[0].cost_usd, None, "unpriced model -> no guess");
    }

    /// Rows are sorted like `summary`: by cost desc.
    #[test]
    fn details_sorted_by_cost_desc() {
        let small = session(
            "small",
            None,
            vec![turn("r1", "2026-06-02T10:00:00Z", usage(100, 0))],
        );
        let big = session(
            "big",
            None,
            vec![turn("r2", "2026-06-02T10:00:00Z", usage(100_000, 0))],
        );
        let details = build_details(&[small, big], &Filter::default(), "claude-code");
        let ids: Vec<&str> = details.iter().map(|d| d.id.as_str()).collect();
        assert_eq!(ids, vec!["big", "small"]);
    }

    /// A session with no accountable usage produces no row (mirrors aggregate).
    #[test]
    fn empty_session_is_dropped() {
        let empty = session("empty", None, vec![ev(EventKind::System)]);
        let details = build_details(&[empty], &Filter::default(), "claude-code");
        assert!(details.is_empty());
    }

    /// Duplicate lines of one request collapse to a single turn (§8.1).
    #[test]
    fn duplicate_request_lines_collapse_to_one_turn() {
        let u = usage(1000, 200);
        let s = session(
            "s",
            None,
            vec![
                turn("req_1", "2026-06-02T10:00:00Z", u),
                turn("req_1", "2026-06-02T10:00:00Z", u),
                turn("req_1", "2026-06-02T10:00:00Z", u),
            ],
        );
        let details = build_details(&[s], &Filter::default(), "claude-code");
        assert_eq!(details[0].turns.len(), 1, "3 lines, 1 request -> 1 turn");
        assert_eq!(details[0].rollup.input, 1000, "not multiplied by 3");
    }
}
