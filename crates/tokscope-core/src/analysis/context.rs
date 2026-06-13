//! Context-bloat attribution — the v0.2 headline feature (CLAUDE.md §2.2, §11).
//!
//! Answers "*why is my context window full, and what's filling it?*" by breaking
//! a session's window down by source. Two kinds of number, kept strictly apart so
//! the user is never misled (CLAUDE.md §8):
//!
//! 1. **MEASURED** (real, agent-reported tokens, from `Usage`):
//!    - **startup overhead** — on a *cold-start* first request (`cache_read == 0`),
//!      `input + cache_creation` is the fixed prefix cached before any real work:
//!      system prompt + tool definitions + memory/CLAUDE.md + the first user turn.
//!      VERIFIED on real files: a fresh session's first request shows e.g.
//!      `input=3, cache_creation=18104, cache_read=0` → ~18 k tokens sitting in the
//!      window before you type. A warm *resume* (`cache_read > 0` on the first
//!      request) can't isolate this floor, so it's reported as unknown, not zero.
//!    - **peak / final input context** — `max`/last of
//!      `input + cache_read + cache_creation` across requests = how full the window
//!      got. The agent does not log per-tool definition sizes, so this measured
//!      bundle cannot be split further; we report the *inventory* instead (below).
//!
//! 2. **ESTIMATED** (from on-disk content sizes ÷ [`EST_CHARS_PER_TOKEN`], NEVER
//!    billed): the *relative* composition of transcript content by
//!    [`ContextSource`], per-MCP-server tool footprint, and the heaviest single
//!    contributors (the context "fat tail" — one giant `Read`/MCP result can
//!    dominate the window).
//!
//! Plus an **exact inventory** from `attachment` lines and tool calls: how many
//! MCP servers are in play, how many tools were *deferred* (available but NOT
//! loaded into the window — so they do *not* bloat it, a common misconception),
//! how many skills were listed, and how heavy the MCP-instruction blocks are.
//!
//! Inputs already captured by the model: [`Usage`] on assistant events,
//! [`Event::content_chars`]/[`Event::thinking_chars`], [`ToolCall::input_bytes`] +
//! [`ToolCall::server`], [`Event::tool_use_id`] (links a `ToolResult` back to the
//! tool that produced it), and [`Event::attachment_kind`]/[`Event::item_count`].
//!
//! [`Usage`]: crate::model::Usage
//! [`Event::content_chars`]: crate::model::Event::content_chars
//! [`Event::thinking_chars`]: crate::model::Event::thinking_chars
//! [`ToolCall::input_bytes`]: crate::model::ToolCall::input_bytes
//! [`ToolCall::server`]: crate::model::ToolCall::server
//! [`Event::tool_use_id`]: crate::model::Event::tool_use_id
//! [`EST_CHARS_PER_TOKEN`]: super::EST_CHARS_PER_TOKEN

use std::collections::{BTreeMap, BTreeSet, HashMap};

use jiff::civil::Date;
use serde::Serialize;

use crate::model::{EventKind, Session, Usage};

/// Bucket a piece of transcript content is attributed to. The split between
/// `AssistantText` and `Thinking` uses [`crate::model::Event::thinking_chars`]
/// (a subset of `content_chars`); `ToolCalls` is the tool-use input JSON.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum ContextSource {
    /// User-typed prompts.
    UserPrompts,
    /// Assistant visible text (excludes thinking and tool-call JSON).
    AssistantText,
    /// Extended-thinking text (measurable only; encrypted thinking is invisible).
    Thinking,
    /// Tool-use input JSON the assistant emitted.
    ToolCalls,
    /// Tool/command results returned into the window (usually the #1 filler).
    ToolResults,
    /// Injected non-message context: deferred-tool/skill listings, MCP
    /// instruction blocks, IDE/file context, reminders (`attachment` lines).
    Attachments,
}

/// Estimated share of transcript content from one [`ContextSource`].
#[derive(Debug, Clone, Serialize)]
pub struct SourceBreakdown {
    pub source: ContextSource,
    /// Raw content chars attributed to this source (measured from the transcript).
    pub chars: u64,
    /// Estimated tokens (`chars / EST_CHARS_PER_TOKEN`) — never a billed figure.
    pub est_tokens: u64,
    /// Fraction of total transcript content chars, `0.0..=1.0`.
    pub share: f64,
}

/// Per-MCP-server context footprint: the tool-use JSON the assistant emitted to
/// call the server's tools, plus the result payloads that came back (attributed
/// via [`crate::model::Event::tool_use_id`]). Plain built-in tools (Read/Edit/…)
/// are grouped under the synthetic server name [`ServerBreakdown::BUILTIN`].
#[derive(Debug, Clone, Serialize)]
pub struct ServerBreakdown {
    /// MCP server name (`mcp__<server>__…`), or [`ServerBreakdown::BUILTIN`].
    pub server: String,
    pub calls: u64,
    /// Chars of tool-use input JSON sent to this server's tools.
    pub call_chars: u64,
    /// Chars of tool-result payloads returned by this server's tools.
    pub result_chars: u64,
    /// Estimated tokens for `call_chars + result_chars`.
    pub est_tokens: u64,
}

impl ServerBreakdown {
    /// Synthetic server name for plain (non-MCP) tools and for results whose
    /// originating tool call could not be located.
    pub const BUILTIN: &'static str = "(built-in)";
}

/// One unusually large individual context contributor (the context "fat tail").
#[derive(Debug, Clone, Serialize)]
pub struct HeavyItem {
    pub source: ContextSource,
    /// Tool name / MCP server / short content summary, when known.
    pub label: Option<String>,
    /// Session the item belongs to.
    pub session_id: String,
    pub chars: u64,
    pub est_tokens: u64,
}

/// Context-bloat profile for one session (each session — parent or sub-agent —
/// is its own window, so each gets its own row; nothing is folded, nothing is
/// double-counted).
#[derive(Debug, Clone, Serialize)]
pub struct SessionContext {
    pub id: String,
    pub project: Option<String>,
    pub model: Option<String>,
    pub sidechain: bool,
    /// MEASURED real tokens cached on a cold-start first request (system prompt +
    /// tool defs + memory + first user turn). `None` for a warm resume.
    pub startup_overhead_tokens: Option<u64>,
    /// True when a cold-start request (`cache_read == 0`) anchored the overhead.
    pub cold_start: bool,
    /// MEASURED: peak `input + cache_read + cache_creation` over requests.
    pub peak_input_tokens: Option<u64>,
    /// MEASURED: the last request's `input + cache_read + cache_creation`.
    pub final_input_tokens: Option<u64>,
    /// ESTIMATED transcript-content breakdown, sorted by `chars` desc.
    pub sources: Vec<SourceBreakdown>,
    pub total_content_chars: u64,
    pub compactions: u64,
}

/// The whole context report: per-session profiles plus cross-session rollups.
#[derive(Debug, Clone, Serialize)]
pub struct ContextReport {
    pub agent: String,
    pub since: Option<Date>,
    pub sessions_profiled: u64,
    /// Per-session profiles, sorted by `peak_input_tokens` desc (then id).
    pub sessions: Vec<SessionContext>,
    /// ESTIMATED transcript-content breakdown across all sessions, sorted desc.
    pub by_source: Vec<SourceBreakdown>,
    /// MCP/tool footprint across all sessions, sorted by `est_tokens` desc.
    pub by_server: Vec<ServerBreakdown>,
    /// Largest individual contributors across all sessions, sorted desc.
    pub heaviest: Vec<HeavyItem>,
    /// Distinct MCP servers actually used (from `mcp__<server>__…` tool calls).
    pub mcp_servers: Vec<String>,
    /// Tools available but DEFERRED — not loaded into the window, so not bloat
    /// (the max set size seen across `deferred_tools_delta` attachments).
    pub deferred_tools: u64,
    /// Skills listed in the window (max across `skill_listing` attachments).
    pub skills_listed: u64,
    /// Chars of MCP-server instruction blocks injected into the window.
    pub mcp_instruction_chars: u64,
    /// Total chars across all `attachment` lines.
    pub attachment_chars: u64,
    pub compactions: u64,
    /// Largest cold-start startup overhead seen (measured), with its session id.
    pub max_startup_overhead: Option<(String, u64)>,
}

/// Cap on how many [`HeavyItem`] entries the "fat tail" view keeps.
const HEAVIEST_N: usize = 12;

/// Window predicate shared with [`super::dedup`] / [`super::aggregate`]:
/// when `since` is set, an event is in range only if it has a timestamp whose
/// UTC date is on/after `since`. Undated events are excluded once a filter is on.
fn in_window(since: Option<Date>, event: &crate::model::Event) -> bool {
    match (since, event.ts) {
        (None, _) => true,
        (Some(since), Some(ts)) => super::utc_date(ts) >= since,
        (Some(_), None) => false,
    }
}

/// The MEASURED context size of one assistant request: everything that was in
/// the window for that turn = fresh input + cache read + cache write.
fn ctx_tokens(u: &Usage) -> u64 {
    u.input.unwrap_or(0) + u.cache_read.unwrap_or(0) + u.cache_creation.unwrap_or(0)
}

/// Per-source char accumulators for one scope (a session, or the whole report).
#[derive(Default)]
struct SourceTally {
    user_prompts: u64,
    assistant_text: u64,
    thinking: u64,
    tool_calls: u64,
    tool_results: u64,
    attachments: u64,
}

impl SourceTally {
    fn total(&self) -> u64 {
        self.user_prompts
            + self.assistant_text
            + self.thinking
            + self.tool_calls
            + self.tool_results
            + self.attachments
    }

    /// Emit `SourceBreakdown` rows for the non-empty sources, shares taken over
    /// `total` (0.0 when nothing accumulated), sorted by chars desc then source.
    fn breakdown(&self) -> Vec<SourceBreakdown> {
        let total = self.total();
        let mut rows: Vec<SourceBreakdown> = [
            (ContextSource::UserPrompts, self.user_prompts),
            (ContextSource::AssistantText, self.assistant_text),
            (ContextSource::Thinking, self.thinking),
            (ContextSource::ToolCalls, self.tool_calls),
            (ContextSource::ToolResults, self.tool_results),
            (ContextSource::Attachments, self.attachments),
        ]
        .into_iter()
        .filter(|(_, chars)| *chars > 0)
        .map(|(source, chars)| SourceBreakdown {
            source,
            chars,
            est_tokens: super::est_tokens(chars),
            share: if total == 0 {
                0.0
            } else {
                chars as f64 / total as f64
            },
        })
        .collect();
        rows.sort_by(|a, b| {
            b.chars
                .cmp(&a.chars)
                .then_with(|| source_rank(a.source).cmp(&source_rank(b.source)))
        });
        rows
    }
}

/// Stable tiebreak ordering for sources that have equal chars.
fn source_rank(s: ContextSource) -> u8 {
    match s {
        ContextSource::UserPrompts => 0,
        ContextSource::AssistantText => 1,
        ContextSource::Thinking => 2,
        ContextSource::ToolCalls => 3,
        ContextSource::ToolResults => 4,
        ContextSource::Attachments => 5,
    }
}

/// Σ of `input_bytes` over an event's tool calls.
fn tool_call_bytes(event: &crate::model::Event) -> u64 {
    event.tool_calls.iter().map(|c| c.input_bytes).sum()
}

/// Build a [`ContextReport`] from parsed sessions.
///
/// `since` filters events by UTC date (inclusive), consistent with
/// [`super::dedup`] / [`super::aggregate`]: when set, events without a timestamp
/// are excluded (they cannot be proven in range).
///
/// Computation (see module docs for the why):
/// - **startup_overhead** / **cold_start**: from the first in-window assistant
///   event that carries `Usage`; cold when its `cache_read` is `Some(0)`, in
///   which case overhead = `input + cache_creation` (unknown fields count as 0).
/// - **peak/final input**: over assistant events with usage,
///   `input + cache_read + cache_creation`.
/// - **by source** (estimated): `UserPrompts` = `User` event chars;
///   `ToolResults` = `ToolResult` event chars; `Attachments` = `Attachment` event
///   chars; `ToolCalls` = Σ `ToolCall::input_bytes`; `Thinking` = Σ
///   `thinking_chars`; `AssistantText` = `content_chars − thinking_chars −
///   tool-call bytes` (saturating). `share` is over the session's total.
/// - **by server**: first map every `ToolCall::id → server`; attribute each
///   tool-use's `input_bytes` to its server and each `ToolResult`'s chars to the
///   server of its `tool_use_id` (unmatched → [`ServerBreakdown::BUILTIN`]).
/// - **heaviest**: every tool result, assistant text chunk, thinking block, and
///   tool call as a candidate; keep the top N by chars.
/// - **inventory**: `deferred_tools`/`skills_listed` = max `item_count` over the
///   respective attachment kinds; `mcp_instruction_chars` = Σ chars of
///   `mcp_instructions_delta` attachments; `mcp_servers` = distinct tool-call
///   servers; `compactions` = `Compaction` events.
pub fn profile(sessions: &[Session], agent: &str, since: Option<Date>) -> ContextReport {
    let mut session_rows: Vec<SessionContext> = Vec::new();

    // Cross-session accumulators.
    let mut global_sources = SourceTally::default();
    let mut heaviest: Vec<HeavyItem> = Vec::new();

    // Per-server footprint, two passes over in-window events.
    // Pass 1 builds id -> server / id -> tool-name from the tool calls; pass 2
    // attributes ToolResult bytes via tool_use_id.
    struct ServerAcc {
        calls: u64,
        call_chars: u64,
        result_chars: u64,
    }
    let mut servers: BTreeMap<String, ServerAcc> = BTreeMap::new();
    let mut id_to_server: HashMap<String, String> = HashMap::new();
    let mut id_to_tool: HashMap<String, String> = HashMap::new();

    // Inventory accumulators.
    let mut mcp_servers: BTreeSet<String> = BTreeSet::new();
    let mut deferred_tools: u64 = 0;
    let mut skills_listed: u64 = 0;
    let mut mcp_instruction_chars: u64 = 0;
    let mut attachment_chars: u64 = 0;
    let mut total_compactions: u64 = 0;
    let mut max_startup_overhead: Option<(String, u64)> = None;

    // Pass 1: per-session profile + global source tally + the id->server map +
    // inventory. (Tool-call servers/maps are global because results in any
    // session reference ids; in practice ids are session-local but the map is
    // keyed by id so this is safe.)
    for session in sessions {
        let mut tally = SourceTally::default();
        let mut compactions: u64 = 0;

        // MEASURED state, threaded over assistant-with-usage events in order.
        let mut first_usage: Option<Usage> = None;
        let mut peak: Option<u64> = None;
        let mut final_ctx: Option<u64> = None;

        for event in &session.events {
            if !in_window(since, event) {
                continue;
            }

            // --- inventory + per-server pass 1 (over all in-window events) ---
            for call in &event.tool_calls {
                if let Some(server) = &call.server {
                    mcp_servers.insert(server.clone());
                }
                let key = call
                    .server
                    .clone()
                    .unwrap_or_else(|| ServerBreakdown::BUILTIN.to_string());
                let acc = servers.entry(key.clone()).or_insert(ServerAcc {
                    calls: 0,
                    call_chars: 0,
                    result_chars: 0,
                });
                acc.calls += 1;
                acc.call_chars += call.input_bytes;
                id_to_server.insert(call.id.clone(), key);
                id_to_tool.insert(call.id.clone(), call.name.clone());
            }

            // --- MEASURED (assistant events with usage) ---
            if event.kind == EventKind::Assistant {
                if let Some(u) = event.usage {
                    if first_usage.is_none() {
                        first_usage = Some(u);
                    }
                    let c = ctx_tokens(&u);
                    peak = Some(peak.map_or(c, |p| p.max(c)));
                    final_ctx = Some(c);
                }
            }

            // --- ESTIMATED source attribution + heaviest candidates ---
            match event.kind {
                EventKind::User => {
                    tally.user_prompts += event.content_chars;
                    global_sources.user_prompts += event.content_chars;
                    if event.content_chars > 0 {
                        heaviest.push(HeavyItem {
                            source: ContextSource::UserPrompts,
                            label: event.content_summary.clone(),
                            session_id: session.id.clone(),
                            chars: event.content_chars,
                            est_tokens: 0,
                        });
                    }
                }
                EventKind::ToolResult => {
                    tally.tool_results += event.content_chars;
                    global_sources.tool_results += event.content_chars;
                    if event.content_chars > 0 {
                        let label = event
                            .tool_use_id
                            .as_ref()
                            .and_then(|id| id_to_tool.get(id).cloned());
                        heaviest.push(HeavyItem {
                            source: ContextSource::ToolResults,
                            label,
                            session_id: session.id.clone(),
                            chars: event.content_chars,
                            est_tokens: 0,
                        });
                    }
                }
                EventKind::Attachment => {
                    tally.attachments += event.content_chars;
                    global_sources.attachments += event.content_chars;
                    attachment_chars += event.content_chars;
                    match event.attachment_kind.as_deref() {
                        Some("deferred_tools_delta") => {
                            deferred_tools = deferred_tools.max(event.item_count);
                        }
                        Some("skill_listing") => {
                            skills_listed = skills_listed.max(event.item_count);
                        }
                        Some("mcp_instructions_delta") => {
                            mcp_instruction_chars += event.content_chars;
                        }
                        _ => {}
                    }
                }
                EventKind::Assistant | EventKind::ToolCall => {
                    let tc = tool_call_bytes(event);
                    let text = event
                        .content_chars
                        .saturating_sub(event.thinking_chars + tc);
                    tally.tool_calls += tc;
                    tally.thinking += event.thinking_chars;
                    tally.assistant_text += text;
                    global_sources.tool_calls += tc;
                    global_sources.thinking += event.thinking_chars;
                    global_sources.assistant_text += text;

                    if text > 0 {
                        heaviest.push(HeavyItem {
                            source: ContextSource::AssistantText,
                            label: event.content_summary.clone(),
                            session_id: session.id.clone(),
                            chars: text,
                            est_tokens: 0,
                        });
                    }
                    if event.thinking_chars > 0 {
                        heaviest.push(HeavyItem {
                            source: ContextSource::Thinking,
                            label: None,
                            session_id: session.id.clone(),
                            chars: event.thinking_chars,
                            est_tokens: 0,
                        });
                    }
                    for call in &event.tool_calls {
                        if call.input_bytes > 0 {
                            heaviest.push(HeavyItem {
                                source: ContextSource::ToolCalls,
                                label: Some(call.name.clone()),
                                session_id: session.id.clone(),
                                chars: call.input_bytes,
                                est_tokens: 0,
                            });
                        }
                    }
                }
                EventKind::Compaction => {
                    compactions += 1;
                }
                EventKind::System | EventKind::SubAgentSpawn => {}
            }
        }

        total_compactions += compactions;

        let cold_start = first_usage.is_some_and(|u| u.cache_read == Some(0));
        let startup_overhead_tokens = if cold_start {
            first_usage.map(|u| u.input.unwrap_or(0) + u.cache_creation.unwrap_or(0))
        } else {
            None
        };
        if let Some(v) = startup_overhead_tokens {
            match &max_startup_overhead {
                Some((_, best)) if *best >= v => {}
                _ => max_startup_overhead = Some((session.id.clone(), v)),
            }
        }

        let total_content_chars = tally.total();
        // No usage and no content => nothing to profile; skip the empty row.
        if first_usage.is_none() && total_content_chars == 0 {
            continue;
        }

        session_rows.push(SessionContext {
            id: session.id.clone(),
            project: session.project.clone(),
            model: session.model.clone(),
            sidechain: session.parent_session.is_some(),
            startup_overhead_tokens,
            cold_start,
            peak_input_tokens: peak,
            final_input_tokens: final_ctx,
            sources: tally.breakdown(),
            total_content_chars,
            compactions,
        });
    }

    // --- per-server pass 2: attribute ToolResult chars by tool_use_id ---
    for session in sessions {
        for event in &session.events {
            if !in_window(since, event) {
                continue;
            }
            if event.kind != EventKind::ToolResult {
                continue;
            }
            let key = event
                .tool_use_id
                .as_ref()
                .and_then(|id| id_to_server.get(id).cloned())
                .unwrap_or_else(|| ServerBreakdown::BUILTIN.to_string());
            servers
                .entry(key)
                .or_insert(ServerAcc {
                    calls: 0,
                    call_chars: 0,
                    result_chars: 0,
                })
                .result_chars += event.content_chars;
        }
    }

    let mut by_server: Vec<ServerBreakdown> = servers
        .into_iter()
        .map(|(server, acc)| ServerBreakdown {
            server,
            calls: acc.calls,
            call_chars: acc.call_chars,
            result_chars: acc.result_chars,
            est_tokens: super::est_tokens(acc.call_chars + acc.result_chars),
        })
        .collect();
    by_server.sort_by(|a, b| {
        b.est_tokens
            .cmp(&a.est_tokens)
            .then_with(|| a.server.cmp(&b.server))
    });

    // Heaviest: rank by chars desc, keep top N, then fill est_tokens.
    heaviest.sort_by(|a, b| {
        b.chars
            .cmp(&a.chars)
            .then_with(|| a.session_id.cmp(&b.session_id))
    });
    heaviest.truncate(HEAVIEST_N);
    for item in &mut heaviest {
        item.est_tokens = super::est_tokens(item.chars);
    }

    // Per-session rows sorted by peak desc (None last) then id.
    session_rows.sort_by(|a, b| match (b.peak_input_tokens, a.peak_input_tokens) {
        (Some(x), Some(y)) => x.cmp(&y).then_with(|| a.id.cmp(&b.id)),
        (Some(_), None) => std::cmp::Ordering::Greater,
        (None, Some(_)) => std::cmp::Ordering::Less,
        (None, None) => a.id.cmp(&b.id),
    });

    ContextReport {
        agent: agent.to_string(),
        since,
        sessions_profiled: session_rows.len() as u64,
        sessions: session_rows,
        by_source: global_sources.breakdown(),
        by_server,
        heaviest,
        mcp_servers: mcp_servers.into_iter().collect(),
        deferred_tools,
        skills_listed,
        mcp_instruction_chars,
        attachment_chars,
        compactions: total_compactions,
        max_startup_overhead,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Event, ToolCall};
    use jiff::Timestamp;

    const TS: &str = "2026-06-02T10:00:00Z";

    /// A bare event of a given kind, timestamped, everything else empty.
    fn ev(kind: EventKind) -> Event {
        Event {
            kind,
            ts: Some(TS.parse().unwrap()),
            request_id: None,
            model: None,
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

    fn usage(input: u64, cc: u64, cr: Option<u64>) -> Usage {
        Usage {
            input: Some(input),
            output: Some(0),
            cache_creation: Some(cc),
            cache_read: cr,
            ..Usage::default()
        }
    }

    /// Assistant event carrying only usage (no content).
    fn asst_usage(u: Usage) -> Event {
        let mut e = ev(EventKind::Assistant);
        e.usage = Some(u);
        e
    }

    fn call(id: &str, name: &str, bytes: u64) -> ToolCall {
        ToolCall {
            id: id.into(),
            name: name.into(),
            input_bytes: bytes,
            server: ToolCall::server_from_name(name),
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

    fn src(b: &[SourceBreakdown], s: ContextSource) -> Option<&SourceBreakdown> {
        b.iter().find(|x| x.source == s)
    }

    fn server<'a>(b: &'a [ServerBreakdown], name: &str) -> Option<&'a ServerBreakdown> {
        b.iter().find(|x| x.server == name)
    }

    // --- MEASURED: cold start vs warm resume vs unknown ---

    #[test]
    fn cold_start_isolates_startup_overhead() {
        // First request: cache_read == Some(0) => cold. overhead = 3 + 18104.
        let s = session("s1", None, vec![asst_usage(usage(3, 18104, Some(0)))]);
        let r = profile(&[s], "claude-code", None);
        let sc = &r.sessions[0];
        assert!(sc.cold_start);
        assert_eq!(sc.startup_overhead_tokens, Some(18107));
        // ctx = 3 + 0 (cr) + 18104 = 18107 for peak & final.
        assert_eq!(sc.peak_input_tokens, Some(18107));
        assert_eq!(sc.final_input_tokens, Some(18107));
        assert_eq!(r.max_startup_overhead, Some(("s1".to_string(), 18107)));
    }

    #[test]
    fn warm_resume_reports_overhead_as_unknown() {
        // First request already reads cache (Some(>0)) => not cold, overhead None.
        let s = session("s1", None, vec![asst_usage(usage(10, 0, Some(500)))]);
        let r = profile(&[s], "claude-code", None);
        let sc = &r.sessions[0];
        assert!(!sc.cold_start);
        assert_eq!(sc.startup_overhead_tokens, None);
        // ctx = 10 + 500 + 0 = 510.
        assert_eq!(sc.peak_input_tokens, Some(510));
        assert_eq!(r.max_startup_overhead, None);
    }

    #[test]
    fn unknown_cache_read_is_not_cold_start() {
        // cache_read None => unknown, must NOT be treated as cold (Some(0)).
        let s = session("s1", None, vec![asst_usage(usage(10, 0, None))]);
        let r = profile(&[s], "claude-code", None);
        let sc = &r.sessions[0];
        assert!(!sc.cold_start);
        assert_eq!(sc.startup_overhead_tokens, None);
        // ctx = 10 + 0 (cr None -> 0) + 0 = 10.
        assert_eq!(sc.final_input_tokens, Some(10));
    }

    #[test]
    fn peak_is_max_and_final_is_last_request() {
        // ctx values: 100, 5000, 800 -> peak 5000, final 800.
        let s = session(
            "s1",
            None,
            vec![
                asst_usage(usage(100, 0, Some(0))),
                asst_usage(usage(0, 0, Some(5000))),
                asst_usage(usage(300, 0, Some(500))),
            ],
        );
        let r = profile(&[s], "claude-code", None);
        let sc = &r.sessions[0];
        assert_eq!(sc.peak_input_tokens, Some(5000));
        assert_eq!(sc.final_input_tokens, Some(800));
        // First request cache_read Some(0) -> cold.
        assert!(sc.cold_start);
        assert_eq!(sc.startup_overhead_tokens, Some(100));
    }

    // --- ESTIMATED: source split within one assistant event ---

    #[test]
    fn assistant_event_splits_text_thinking_and_tool_calls() {
        // content_chars=100, thinking=30, one tool call=20 bytes.
        // AssistantText = 100 - (30 + 20) = 50; Thinking=30; ToolCalls=20.
        let mut a = ev(EventKind::Assistant);
        a.content_chars = 100;
        a.thinking_chars = 30;
        a.tool_calls = vec![call("t1", "Read", 20)];
        let s = session("s1", None, vec![a]);
        let r = profile(&[s], "claude-code", None);
        let sc = &r.sessions[0];

        assert_eq!(sc.total_content_chars, 100, "50 text + 30 think + 20 call");
        assert_eq!(
            src(&sc.sources, ContextSource::AssistantText)
                .unwrap()
                .chars,
            50
        );
        assert_eq!(src(&sc.sources, ContextSource::Thinking).unwrap().chars, 30);
        assert_eq!(
            src(&sc.sources, ContextSource::ToolCalls).unwrap().chars,
            20
        );
        // shares: 50/100, 30/100, 20/100.
        let at = src(&sc.sources, ContextSource::AssistantText).unwrap();
        assert!((at.share - 0.5).abs() < 1e-9);
        assert_eq!(at.est_tokens, 12, "50 / 4 = 12");
        // sorted by chars desc: AssistantText(50), Thinking(30), ToolCalls(20).
        assert_eq!(sc.sources[0].source, ContextSource::AssistantText);
        assert_eq!(sc.sources[1].source, ContextSource::Thinking);
        assert_eq!(sc.sources[2].source, ContextSource::ToolCalls);
    }

    #[test]
    fn source_split_across_event_kinds() {
        let mut user = ev(EventKind::User);
        user.content_chars = 40;
        let mut result = ev(EventKind::ToolResult);
        result.content_chars = 1000;
        result.tool_use_id = Some("t1".into());
        let mut attach = ev(EventKind::Attachment);
        attach.content_chars = 8;
        let mut a = ev(EventKind::Assistant);
        a.content_chars = 12; // pure text, no thinking, no calls
        let s = session("s1", None, vec![user, a, result, attach]);
        let r = profile(&[s], "claude-code", None);
        let sc = &r.sessions[0];

        assert_eq!(sc.total_content_chars, 40 + 12 + 1000 + 8);
        assert_eq!(
            src(&sc.sources, ContextSource::UserPrompts).unwrap().chars,
            40
        );
        assert_eq!(
            src(&sc.sources, ContextSource::ToolResults).unwrap().chars,
            1000
        );
        assert_eq!(
            src(&sc.sources, ContextSource::Attachments).unwrap().chars,
            8
        );
        assert_eq!(
            src(&sc.sources, ContextSource::AssistantText)
                .unwrap()
                .chars,
            12
        );
        // Largest is the tool result.
        assert_eq!(sc.sources[0].source, ContextSource::ToolResults);
        // Cross-session by_source mirrors single session here.
        assert_eq!(
            src(&r.by_source, ContextSource::ToolResults).unwrap().chars,
            1000
        );
    }

    // --- by server, including unmatched result -> BUILTIN ---

    #[test]
    fn server_attribution_groups_calls_and_results() {
        // One MCP call (github) with a result, one builtin Read call with a
        // result, and one result whose tool_use_id matches no call -> BUILTIN.
        let mut a = ev(EventKind::Assistant);
        a.tool_calls = vec![
            call("g1", "mcp__github__search", 100),
            call("r1", "Read", 30),
        ];
        let mut res_github = ev(EventKind::ToolResult);
        res_github.tool_use_id = Some("g1".into());
        res_github.content_chars = 4000;
        let mut res_read = ev(EventKind::ToolResult);
        res_read.tool_use_id = Some("r1".into());
        res_read.content_chars = 200;
        let mut res_orphan = ev(EventKind::ToolResult);
        res_orphan.tool_use_id = Some("missing".into());
        res_orphan.content_chars = 7;
        let s = session("s1", None, vec![a, res_github, res_read, res_orphan]);
        let r = profile(&[s], "claude-code", None);

        let gh = server(&r.by_server, "github").unwrap();
        assert_eq!(gh.calls, 1);
        assert_eq!(gh.call_chars, 100);
        assert_eq!(gh.result_chars, 4000);
        assert_eq!(gh.est_tokens, (100 + 4000) / 4);

        let bi = server(&r.by_server, ServerBreakdown::BUILTIN).unwrap();
        assert_eq!(bi.calls, 1, "the Read call");
        assert_eq!(bi.call_chars, 30);
        // 200 (matched Read result) + 7 (orphan result) both land on BUILTIN.
        assert_eq!(bi.result_chars, 207);

        // Sorted by est_tokens desc -> github (1025) before builtin (59).
        assert_eq!(r.by_server[0].server, "github");
        // Inventory: github is the only mcp server.
        assert_eq!(r.mcp_servers, vec!["github".to_string()]);
    }

    // --- heaviest ordering ---

    #[test]
    fn heaviest_ranks_biggest_contributor_first() {
        let mut a = ev(EventKind::Assistant);
        a.content_chars = 60; // pure text 60
        a.content_summary = Some("hello".into());
        let mut big = ev(EventKind::ToolResult);
        big.tool_use_id = Some("t1".into());
        big.content_chars = 9000;
        let mut small = ev(EventKind::ToolResult);
        small.content_chars = 100;
        let mut a2 = ev(EventKind::Assistant);
        a2.tool_calls = vec![call("t1", "Grep", 25)];
        let s = session("s1", None, vec![a, a2, big, small]);
        let r = profile(&[s], "claude-code", None);

        assert_eq!(r.heaviest[0].chars, 9000);
        assert_eq!(r.heaviest[0].source, ContextSource::ToolResults);
        // Label resolved via tool_use_id -> the Grep call name.
        assert_eq!(r.heaviest[0].label.as_deref(), Some("Grep"));
        assert_eq!(r.heaviest[0].est_tokens, 2250);
        // The set: 9000 (result), 100 (result), 60 (asst text), 25 (tool call).
        let chars: Vec<u64> = r.heaviest.iter().map(|h| h.chars).collect();
        assert_eq!(chars, vec![9000, 100, 60, 25]);
    }

    // --- inventory counts ---

    #[test]
    fn inventory_counts_deferred_skills_servers_and_compactions() {
        let mut deferred1 = ev(EventKind::Attachment);
        deferred1.attachment_kind = Some("deferred_tools_delta".into());
        deferred1.item_count = 30;
        deferred1.content_chars = 5;
        let mut deferred2 = ev(EventKind::Attachment);
        deferred2.attachment_kind = Some("deferred_tools_delta".into());
        deferred2.item_count = 42; // max wins
        deferred2.content_chars = 5;
        let mut skills = ev(EventKind::Attachment);
        skills.attachment_kind = Some("skill_listing".into());
        skills.item_count = 9;
        skills.content_chars = 5;
        let mut mcp_instr = ev(EventKind::Attachment);
        mcp_instr.attachment_kind = Some("mcp_instructions_delta".into());
        mcp_instr.content_chars = 800;
        let mut a = ev(EventKind::Assistant);
        a.tool_calls = vec![
            call("g1", "mcp__github__x", 1),
            call("c1", "mcp__canva__y", 1),
        ];
        let comp = ev(EventKind::Compaction);
        let s = session(
            "s1",
            None,
            vec![deferred1, deferred2, skills, mcp_instr, a, comp],
        );
        let r = profile(&[s], "claude-code", None);

        assert_eq!(r.deferred_tools, 42, "max item_count across deltas");
        assert_eq!(r.skills_listed, 9);
        assert_eq!(r.mcp_instruction_chars, 800);
        // attachment_chars = 5 + 5 + 5 + 800.
        assert_eq!(r.attachment_chars, 815);
        assert_eq!(r.compactions, 1);
        assert_eq!(r.sessions[0].compactions, 1);
        assert_eq!(
            r.mcp_servers,
            vec!["canva".to_string(), "github".to_string()]
        );
    }

    // --- sidechain flag + empty-row skipping ---

    #[test]
    fn sidechain_flag_and_empty_sessions_skipped() {
        let child = session(
            "agent-x",
            Some("parent"),
            vec![asst_usage(usage(5, 0, Some(0)))],
        );
        // A session with neither usage nor content => no row.
        let empty = session("empty", None, vec![ev(EventKind::System)]);
        let r = profile(&[child, empty], "claude-code", None);
        assert_eq!(r.sessions_profiled, 1, "empty session produced no row");
        assert_eq!(r.sessions[0].id, "agent-x");
        assert!(r.sessions[0].sidechain);
    }

    // --- since filter ---

    #[test]
    fn since_filter_excludes_older_and_undated_events() {
        let mut old = asst_usage(usage(1000, 0, Some(0)));
        old.ts = Some("2026-06-01T10:00:00Z".parse::<Timestamp>().unwrap());
        let mut new = asst_usage(usage(50, 0, Some(200)));
        new.ts = Some("2026-06-02T09:00:00Z".parse::<Timestamp>().unwrap());
        // Undated event must be excluded when a filter is set.
        let mut undated = ev(EventKind::ToolResult);
        undated.ts = None;
        undated.content_chars = 99999;

        let since: Date = "2026-06-02".parse().unwrap();
        let s = session("s1", None, vec![old, new, undated]);
        let r = profile(&[s], "claude-code", Some(since));
        let sc = &r.sessions[0];

        // Only the 2026-06-02 assistant event survives.
        assert_eq!(
            sc.peak_input_tokens,
            Some(250),
            "50 + 200, old 1000 excluded"
        );
        assert_eq!(sc.final_input_tokens, Some(250));
        // Its cache_read is Some(200) -> warm, not cold.
        assert!(!sc.cold_start);
        // Undated ToolResult's 99999 chars excluded.
        assert_eq!(sc.total_content_chars, 0);
        assert!(r.heaviest.is_empty(), "undated heavy result filtered out");
        assert_eq!(r.since, Some(since));
    }

    #[test]
    fn sessions_sorted_by_peak_desc_then_id() {
        let a = session("aaa", None, vec![asst_usage(usage(100, 0, Some(0)))]);
        let b = session("bbb", None, vec![asst_usage(usage(0, 0, Some(9000)))]);
        // No-usage-but-has-content session: peak None -> sorts last.
        let mut txt = ev(EventKind::User);
        txt.content_chars = 10;
        let c = session("ccc", None, vec![txt]);
        let r = profile(&[a, b, c], "claude-code", None);
        let ids: Vec<&str> = r.sessions.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, vec!["bbb", "aaa", "ccc"], "peak 9000, 100, then None");
        assert_eq!(r.sessions[2].peak_input_tokens, None);
    }
}
