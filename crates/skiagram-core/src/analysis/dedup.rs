//! Request-level deduplication — THE critical accounting step (CLAUDE.md §8.1).
//!
//! WHY: Claude Code (and agents like it) write one JSONL line per content block,
//! and every line repeats the request's `message.usage`. A single API request
//! therefore appears as 2–10 lines sharing one `requestId` (observed on real
//! data: 83 lines -> 26 requests; 642 -> 262). Naively summing per-line usage
//! multiplies token counts by that factor.
//!
//! RULE: group assistant events by `requestId` and take the field-wise MAX of
//! each usage counter (see [`crate::model::Usage::merge_max`]). Lines of one
//! request either repeat identical numbers or grow monotonically while
//! streaming, so MAX recovers the final per-request value in both cases without
//! double counting. Events with no `requestId` are never merged with each other.
//!
//! This pass also performs thinking ATTRIBUTION (§8.2). Claude Code's
//! `output_tokens` ALREADY includes extended-thinking tokens (verified on real
//! data — thinking-only requests report output far larger than their visible
//! text), so there is no undercount to "fix". Instead we measure the visible
//! thinking chars per request (the basis for an estimated thinking-token SHARE of
//! output) and flag requests whose thinking was encrypted/redacted, where that
//! share is unmeasurable (absence ≠ zero, §8.5).

use std::collections::HashMap;

use jiff::civil::Date;
use jiff::Timestamp;
use serde::Serialize;

use crate::analysis::{utc_date, EST_CHARS_PER_TOKEN};
use crate::model::{EventKind, Session, Usage};

/// One deduplicated API request with its finalized usage.
#[derive(Debug, Clone, Serialize)]
pub struct UsageRecord {
    pub session_id: String,
    /// Parent session id when this spend came from a sub-agent transcript.
    pub parent_session: Option<String>,
    pub project: Option<String>,
    pub request_id: Option<String>,
    pub model: Option<String>,
    pub ts: Option<Timestamp>,
    pub usage: Usage,
    /// Request belongs to a sub-agent (sidechain) transcript.
    pub sidechain: bool,
    /// Visible extended-thinking chars measured across this request's lines — the
    /// basis for thinking ATTRIBUTION. These tokens are ALREADY counted inside
    /// `usage.output` (Claude Code, verified), so this answers "how much of output
    /// was thinking", never an addition to the billable total. 0 when there was no
    /// thinking, or when every thinking block was encrypted (`thinking_encrypted`).
    pub thinking_chars: u64,
    /// Thinking blocks were present but none were measurable (encrypted/redacted:
    /// `"thinking":""` + signature). The thinking share of output is unknown here —
    /// surfaced as such, never guessed (CLAUDE.md §8.5). On the sampled machine
    /// ~85% of thinking requests were encrypted, so this is the common case.
    pub thinking_encrypted: bool,
    /// Thinking blocks were present at all (measurable or encrypted).
    pub has_thinking: bool,
}

/// Proof-of-work counters from the dedup pass.
#[derive(Debug, Default, Clone, Serialize)]
pub struct DedupStats {
    /// Extra assistant lines merged away (lines − requests). If this is N > 0,
    /// a naive parser would have counted N lines' usage twice.
    pub duplicate_lines_collapsed: u64,
    /// What naive per-line summation would have reported (for the overcount
    /// stat shown to the user).
    pub naive_known_tokens: u64,
    /// Requests that carried thinking blocks (measurable or encrypted).
    pub requests_with_thinking: u64,
    /// Requests whose thinking was present but entirely encrypted/unmeasurable, so
    /// the thinking share of their output can't be measured (absence ≠ zero, §8.5).
    pub requests_with_encrypted_thinking: u64,
    /// Total measured visible thinking chars — the basis for an estimated
    /// thinking-token attribution. Already included in `output`; never added to
    /// billable totals.
    pub thinking_chars_total: u64,
}

impl DedupStats {
    /// Estimated thinking tokens from measured visible thinking chars — a LOWER
    /// BOUND: extended-thinking text tokenizes denser than the `chars/4` rule of
    /// thumb (~1–3 chars/token observed), and encrypted thinking contributes 0
    /// measurable chars though it still costs output tokens. These tokens are
    /// already inside `output` and are never billed again.
    pub fn thinking_tokens_estimate(&self) -> u64 {
        self.thinking_chars_total / EST_CHARS_PER_TOKEN
    }
}

#[derive(Default)]
struct Acc {
    usage: Usage,
    model: Option<String>,
    request_id: Option<String>,
    ts: Option<Timestamp>,
    lines: u64,
    thinking_chars: u64,
    has_thinking: bool,
    sidechain: bool,
}

/// Collapse a session's assistant events into per-request usage records.
///
/// `since` filters at the event level (UTC date, inclusive) so the returned
/// stats describe the same window as the records. Events without a timestamp
/// are excluded when a filter is set (they cannot be proven in-range).
pub fn dedup_session(session: &Session, since: Option<Date>) -> (Vec<UsageRecord>, DedupStats) {
    let mut stats = DedupStats::default();
    let mut keys: Vec<String> = Vec::new();
    let mut groups: HashMap<String, Acc> = HashMap::new();

    for (idx, event) in session.events.iter().enumerate() {
        if event.kind != EventKind::Assistant {
            continue;
        }
        if let Some(since) = since {
            match event.ts {
                Some(ts) if utc_date(ts) >= since => {}
                _ => continue,
            }
        }
        // Assistant lines without usage (API errors, synthetic lines) carry no
        // accountable spend. Their absence is "unknown", not zero (§8.5).
        let Some(usage) = event.usage else { continue };
        stats.naive_known_tokens += usage.known_total();

        // No requestId -> a key unique to this line, so it is never merged.
        let key = event
            .request_id
            .clone()
            .unwrap_or_else(|| format!("\u{0}line:{idx}"));
        let acc = groups.entry(key.clone()).or_insert_with(|| {
            keys.push(key);
            Acc::default()
        });
        if acc.lines > 0 {
            stats.duplicate_lines_collapsed += 1;
        }
        acc.lines += 1;
        acc.usage = acc.usage.merge_max(usage);
        if acc.model.is_none() {
            acc.model.clone_from(&event.model);
        }
        if acc.request_id.is_none() {
            acc.request_id.clone_from(&event.request_id);
        }
        acc.ts = match (acc.ts, event.ts) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        // Thinking is spread across the request's lines — sum what's measurable.
        acc.thinking_chars += event.thinking_chars;
        acc.has_thinking |= event.has_thinking;
        acc.sidechain |= event.sidechain;
    }

    let records = keys
        .into_iter()
        .filter_map(|key| {
            let acc = groups.remove(&key)?;
            let thinking_chars = acc.thinking_chars;
            // Thinking present but nothing measurable -> encrypted/redacted: the
            // thinking share of output is unknown, surfaced not guessed (§8.5).
            let thinking_encrypted = acc.has_thinking && thinking_chars == 0;
            if acc.has_thinking {
                stats.requests_with_thinking += 1;
            }
            if thinking_encrypted {
                stats.requests_with_encrypted_thinking += 1;
            }
            stats.thinking_chars_total += thinking_chars;
            Some(UsageRecord {
                session_id: session.id.clone(),
                parent_session: session.parent_session.clone(),
                project: session.project.clone(),
                request_id: acc.request_id,
                model: acc.model,
                ts: acc.ts,
                usage: acc.usage,
                sidechain: acc.sidechain || session.parent_session.is_some(),
                thinking_chars,
                thinking_encrypted,
                has_thinking: acc.has_thinking,
            })
        })
        .collect();

    (records, stats)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Event;

    fn usage(input: u64, output: u64, cc: u64, cr: u64) -> Usage {
        Usage {
            input: Some(input),
            output: Some(output),
            cache_creation: Some(cc),
            cache_read: Some(cr),
            ..Usage::default()
        }
    }

    fn assistant(request_id: Option<&str>, u: Option<Usage>) -> Event {
        Event {
            kind: EventKind::Assistant,
            ts: Some("2026-06-01T10:00:00Z".parse().unwrap()),
            request_id: request_id.map(str::to_string),
            model: Some("claude-sonnet-4-5".into()),
            usage: u,
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

    fn session(events: Vec<Event>) -> Session {
        Session {
            id: "s1".into(),
            agent: "claude-code".into(),
            project: Some("proj".into()),
            model: None,
            parent_session: None,
            started_at: None,
            ended_at: None,
            events,
            sub_agents: Vec::new(),
            skipped_lines: 0,
        }
    }

    /// The headline case: 3 lines, one request, identical repeated usage.
    /// A naive sum would report 3x the real spend.
    #[test]
    fn identical_duplicate_lines_collapse_to_one_request() {
        let u = usage(1000, 200, 300, 5000);
        let s = session(vec![
            assistant(Some("req_1"), Some(u)),
            assistant(Some("req_1"), Some(u)),
            assistant(Some("req_1"), Some(u)),
        ]);
        let (records, stats) = dedup_session(&s, None);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].usage, u, "deduped == single request's usage");
        assert_eq!(stats.duplicate_lines_collapsed, 2);
        assert_eq!(stats.naive_known_tokens, 3 * u.known_total());
    }

    /// Streaming partials grow monotonically; MAX recovers the final value.
    #[test]
    fn streaming_partials_take_field_wise_max() {
        let s = session(vec![
            assistant(Some("req_1"), Some(usage(1000, 50, 300, 5000))),
            assistant(Some("req_1"), Some(usage(1000, 120, 300, 5000))),
            assistant(Some("req_1"), Some(usage(1000, 200, 300, 5000))),
        ]);
        let (records, _) = dedup_session(&s, None);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].usage.output, Some(200));
        assert_eq!(records[0].usage.input, Some(1000), "input not multiplied");
    }

    #[test]
    fn lines_without_request_id_are_never_merged() {
        let u = usage(10, 5, 0, 0);
        let s = session(vec![assistant(None, Some(u)), assistant(None, Some(u))]);
        let (records, stats) = dedup_session(&s, None);
        assert_eq!(records.len(), 2);
        assert_eq!(stats.duplicate_lines_collapsed, 0);
    }

    #[test]
    fn missing_usage_is_unknown_not_zero() {
        let s = session(vec![assistant(Some("req_err"), None)]);
        let (records, stats) = dedup_session(&s, None);
        assert!(
            records.is_empty(),
            "no usage -> no record, not a zero record"
        );
        assert_eq!(stats.naive_known_tokens, 0);
    }

    /// §8.2: thinking is ATTRIBUTED, not "reconciled". `output_tokens` already
    /// includes thinking, so we measure visible thinking chars and flag the
    /// encrypted case (share unmeasurable) — we never invent an undercount.
    #[test]
    fn thinking_is_attributed_and_encrypted_is_flagged() {
        // Visible thinking: measurable chars, already inside the 800 output tokens.
        let mut visible = assistant(Some("req_vis"), Some(usage(1500, 800, 0, 0)));
        visible.has_thinking = true;
        visible.thinking_chars = 1600; // ~400 est. thinking tokens (a lower bound)

        // Encrypted thinking: present but nothing measurable.
        let mut encrypted = assistant(Some("req_enc"), Some(usage(1500, 500, 0, 0)));
        encrypted.has_thinking = true;
        encrypted.thinking_chars = 0;

        let (records, stats) = dedup_session(&session(vec![visible, encrypted]), None);
        assert_eq!(stats.requests_with_thinking, 2);
        assert_eq!(stats.requests_with_encrypted_thinking, 1);
        assert_eq!(stats.thinking_chars_total, 1600);
        assert_eq!(
            stats.thinking_tokens_estimate(),
            400,
            "1600 / 4, a lower bound"
        );

        let vis = records
            .iter()
            .find(|r| r.request_id.as_deref() == Some("req_vis"))
            .unwrap();
        let enc = records
            .iter()
            .find(|r| r.request_id.as_deref() == Some("req_enc"))
            .unwrap();
        assert_eq!(vis.thinking_chars, 1600);
        assert!(!vis.thinking_encrypted, "visible thinking is measurable");
        assert!(enc.has_thinking);
        assert!(enc.thinking_encrypted, "0 measurable chars -> encrypted");
        assert_eq!(enc.thinking_chars, 0);
    }

    #[test]
    fn since_filters_events_by_utc_date() {
        let mut old = assistant(Some("req_old"), Some(usage(100, 10, 0, 0)));
        old.ts = Some("2026-06-01T10:00:00Z".parse().unwrap());
        let mut new = assistant(Some("req_new"), Some(usage(200, 20, 0, 0)));
        new.ts = Some("2026-06-02T09:00:00Z".parse().unwrap());

        let since: Date = "2026-06-02".parse().unwrap();
        let (records, stats) = dedup_session(&session(vec![old, new]), Some(since));
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].request_id.as_deref(), Some("req_new"));
        assert_eq!(stats.naive_known_tokens, 220);
    }
}
