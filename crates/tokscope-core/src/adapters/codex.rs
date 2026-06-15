//! Codex CLI adapter — roadmap v0.4, "new"-generation schema (Codex >= 0.44).
//!
//! Reads `~/.codex/sessions/YYYY/MM/DD/rollout-<ts>-<uuid>.jsonl` and
//! `~/.codex/archived_sessions/*.jsonl` (READ-ONLY). `$CODEX_HOME` overrides the
//! `~/.codex` root and may be a comma-separated list of roots (we scan all).
//!
//! Schema (the generation present on real installs sampled for v0.4). Every line
//! is `{"timestamp": ..., "type": ..., "payload": {...}}`:
//! - `session_meta` — `payload.{id, timestamp, cwd, originator, cli_version,
//!   model_provider, base_instructions}`. Source of session id / project / start.
//! - `turn_context` — `payload.model` (e.g. `"gpt-5.5"`) + `cwd`,
//!   `workspace_roots`. Tells us the model in effect for the following requests.
//! - `response_item` — `payload.type` ∈ {`message` (role user/assistant, with
//!   `content:[{type,text}]`), `reasoning`, `function_call`,
//!   `function_call_output`, …}. We map `message` to User/Assistant text and
//!   `function_call` to a [`ToolCall`] (MCP names `mcp__server__tool` resolve via
//!   [`ToolCall::server_from_name`]).
//! - `event_msg` — `payload.type` ∈ {`task_started`, `user_message`,
//!   `agent_message`, `token_count`, `task_complete`, `patch_apply_end`,
//!   `mcp_tool_call_end`, `context_compacted`, …}.
//!
//! THE CRITICAL CORRECTNESS RULE (Codex analog of CLAUDE.md §8.1):
//! token usage lives ONLY in `event_msg` where `payload.type == "token_count"`,
//! under `payload.info`:
//!   - `total_token_usage` is **cumulative, monotonic non-decreasing** over the
//!     whole session. Summing it across the (often hundreds of) token_count
//!     events would overcount by ~100x.
//!   - `last_token_usage` is the **per-request delta** (verified: consecutive
//!     `total` deltas equal `last`).
//!
//! So we emit ONE [`EventKind::Assistant`] event per `token_count`, carrying
//! `last_token_usage` mapped DISJOINTLY into [`Usage`] (see [`map_last_usage`]) so
//! that summing every per-request `known_total()` reconstructs the FINAL
//! cumulative `total_token_usage.total_tokens`. No `request_id` is attached —
//! these are genuinely distinct requests and downstream dedup keys each one
//! uniquely, so they are never wrongly merged.
//!
//! Reconciliation invariant: `Σ event.usage.known_total() == final
//! total_token_usage.total_tokens`. A `context_compacted` mid-session can leave a
//! tiny gap versus the final cumulative (compaction rewrites the window); that
//! gap is acceptable and surfaced, never hidden.
//!
//! Codex models are `gpt-*` and are NOT in tokscope's embedded pricing snapshot,
//! so their cost renders as "unpriced" — that is correct and honest (§8.7): we
//! never guess a price.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::adapters::Adapter;
use crate::error::CoreError;
use crate::model::{Event, EventKind, Session, SessionRef, ToolCall, Usage};

/// Codex CLI (`~/.codex`).
pub struct Codex;

/// Data roots: `$CODEX_HOME` when set (comma-separated list honored), else
/// `~/.codex` resolved via the `directories` crate — never a hardcoded `~`.
/// Tests point `CODEX_HOME` at the fixtures dir.
fn codex_roots() -> Vec<PathBuf> {
    match std::env::var("CODEX_HOME") {
        Ok(val) if !val.trim().is_empty() => val
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .collect(),
        _ => directories::BaseDirs::new()
            .map(|b| vec![b.home_dir().join(".codex")])
            .unwrap_or_default(),
    }
}

/// The two transcript directories under a Codex root.
fn session_dirs(root: &Path) -> [PathBuf; 2] {
    [root.join("sessions"), root.join("archived_sessions")]
}

impl Adapter for Codex {
    fn id(&self) -> &'static str {
        "codex"
    }

    fn detect(&self) -> bool {
        codex_roots()
            .iter()
            .any(|r| session_dirs(r).iter().any(|d| d.is_dir()))
    }

    fn discover(&self) -> anyhow::Result<Vec<SessionRef>> {
        let roots = codex_roots();
        if roots.is_empty() {
            anyhow::bail!("could not determine the home directory (set CODEX_HOME)");
        }
        let mut refs = Vec::new();
        for root in &roots {
            for dir in session_dirs(root) {
                if !dir.is_dir() {
                    continue;
                }
                for entry in walkdir::WalkDir::new(&dir).follow_links(false) {
                    let entry = match entry {
                        Ok(e) => e,
                        Err(e) => {
                            tracing::debug!("skipping unreadable directory entry: {e}");
                            continue;
                        }
                    };
                    if !entry.file_type().is_file()
                        || entry.path().extension().and_then(|x| x.to_str()) != Some("jsonl")
                    {
                        continue;
                    }
                    let meta = entry.metadata().ok();
                    refs.push(SessionRef {
                        path: entry.path().to_path_buf(),
                        agent: self.id().to_string(),
                        // Project filled in from `session_meta.cwd` at parse time;
                        // the on-disk path carries only date buckets, not the cwd.
                        project: None,
                        size_bytes: meta.as_ref().map_or(0, |m| m.len()),
                        modified: meta
                            .and_then(|m| m.modified().ok())
                            .and_then(|t| jiff::Timestamp::try_from(t).ok()),
                    });
                }
            }
        }
        refs.sort_by_key(|r| std::cmp::Reverse(r.modified));
        Ok(refs)
    }

    fn parse(&self, r: &SessionRef) -> anyhow::Result<Session> {
        let file = File::open(&r.path).map_err(|source| CoreError::Io {
            path: r.path.clone(),
            source,
        })?;

        let mut session = Session {
            id: session_id_for(&r.path),
            agent: self.id().to_string(),
            project: r.project.clone(),
            model: None,
            parent_session: None,
            started_at: None,
            ended_at: None,
            events: Vec::new(),
            sub_agents: Vec::new(),
            skipped_lines: 0,
        };
        // Model in effect, learned from session_meta / turn_context as we stream;
        // each token_count event is tagged with the most recent one. BTreeMap so
        // the session-level winner is a deterministic tie-break.
        let mut current_model: Option<String> = None;
        let mut model_counts: BTreeMap<String, u64> = BTreeMap::new();

        for (idx, line) in BufReader::new(file).lines().enumerate() {
            let line = match line {
                Ok(l) => l,
                Err(e) => {
                    session.skipped_lines += 1;
                    tracing::debug!("{}:{}: unreadable line: {e}", r.path.display(), idx + 1);
                    continue;
                }
            };
            if line.trim().is_empty() {
                continue;
            }
            let raw: RawLine = match serde_json::from_str(&line) {
                Ok(v) => v,
                Err(e) => {
                    session.skipped_lines += 1;
                    tracing::debug!("{}:{}: unparseable JSON: {e}", r.path.display(), idx + 1);
                    continue;
                }
            };
            let ts = parse_ts(&raw.timestamp);
            match raw.kind.as_deref() {
                Some("session_meta") => apply_session_meta(&raw, &mut session, &mut current_model),
                Some("turn_context") => {
                    if let Some(model) = raw.payload.model.as_deref() {
                        if !model.is_empty() {
                            current_model = Some(model.to_string());
                        }
                    }
                }
                Some("response_item") => push_response_item(&raw, ts, &mut session),
                Some("event_msg") => {
                    push_event_msg(&raw, ts, &current_model, &mut session, &mut model_counts)
                }
                other => {
                    session.skipped_lines += 1;
                    tracing::debug!(
                        "{}:{}: unknown line type {:?}",
                        r.path.display(),
                        idx + 1,
                        other
                    );
                }
            }
        }

        session.started_at = session.events.iter().filter_map(|e| e.ts).min();
        session.ended_at = session.events.iter().filter_map(|e| e.ts).max();
        // Prefer the most-used model seen on token_count events; fall back to the
        // last model context if no usage was ever reported.
        session.model = model_counts
            .into_iter()
            .max_by_key(|(_, count)| *count)
            .map(|(model, _)| model)
            .or(current_model);
        Ok(session)
    }
}

/// Session id = file stem (`rollout-<ts>-<uuid>`). The embedded `session_meta.id`
/// is the same uuid but the stem is unique on disk and matches discovery.
fn session_id_for(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

fn parse_ts(raw: &Option<String>) -> Option<jiff::Timestamp> {
    raw.as_deref().and_then(|t| t.parse().ok())
}

/// Display-only first non-empty line, capped at 80 chars.
fn snippet(text: &str) -> Option<String> {
    let first = text.lines().find(|l| !l.trim().is_empty())?;
    let s: String = first.trim().chars().take(80).collect();
    (!s.is_empty()).then_some(s)
}

/// Join the `content:[{type,text}]` array of a `message` payload into plain text.
fn message_text(content: &serde_json::Value) -> String {
    if let Some(text) = content.as_str() {
        return text.to_string();
    }
    let Some(blocks) = content.as_array() else {
        return String::new();
    };
    let mut out = String::new();
    for block in blocks {
        if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(text);
        }
    }
    out
}

/// `session_meta` carries the cwd (project) and start. Project is the cwd as the
/// agent recorded it, mirroring how Claude Code labels by working directory.
fn apply_session_meta(raw: &RawLine, session: &mut Session, current_model: &mut Option<String>) {
    if session.project.is_none() {
        if let Some(cwd) = raw.payload.cwd.as_deref() {
            if !cwd.is_empty() {
                session.project = Some(cwd.to_string());
            }
        }
    }
    // session_meta has no `model` field on the generation seen; turn_context does.
    // Read it defensively in case a future generation moves it here.
    if current_model.is_none() {
        if let Some(model) = raw.payload.model.as_deref() {
            if !model.is_empty() {
                *current_model = Some(model.to_string());
            }
        }
    }
}

/// Map a `response_item` to an event. `message` → User/Assistant text (content
/// chars for summaries), `function_call` → a ToolCall on an Assistant event.
/// `reasoning` / `function_call_output` and other shapes carry no billable usage
/// (usage is reported separately on token_count) — we keep them out of the spend
/// path and don't count them as skipped.
fn push_response_item(raw: &RawLine, ts: Option<jiff::Timestamp>, session: &mut Session) {
    match raw.payload.item_type.as_deref() {
        Some("message") => {
            let text = message_text(&raw.payload.content);
            let kind = match raw.payload.role.as_deref() {
                Some("assistant") => EventKind::Assistant,
                // user / tool / developer / unknown → treat as user-side input.
                _ => EventKind::User,
            };
            session.events.push(Event {
                kind,
                ts,
                request_id: None,
                model: None,
                // Usage is NEVER on response_item — only token_count carries it.
                usage: None,
                tool_calls: Vec::new(),
                sidechain: false,
                content_summary: snippet(&text),
                content_chars: text.chars().count() as u64,
                thinking_chars: 0,
                has_thinking: false,
                tool_use_id: None,
                attachment_kind: None,
                item_count: 0,
            });
        }
        Some("function_call") => {
            let name = raw
                .payload
                .name
                .clone()
                .unwrap_or_else(|| "unknown".to_string());
            let id = raw.payload.call_id.clone().unwrap_or_default();
            let input_bytes = raw
                .payload
                .arguments
                .as_ref()
                .map_or(0, |a| a.chars().count() as u64);
            session.events.push(Event {
                kind: EventKind::ToolCall,
                ts,
                request_id: None,
                model: None,
                usage: None,
                tool_calls: vec![ToolCall {
                    server: ToolCall::server_from_name(&name),
                    id,
                    name,
                    input_bytes,
                }],
                sidechain: false,
                content_summary: None,
                content_chars: input_bytes,
                thinking_chars: 0,
                has_thinking: false,
                tool_use_id: None,
                attachment_kind: None,
                item_count: 0,
            });
        }
        // `reasoning`, `function_call_output`, and any other response_item shapes
        // do not carry billable usage; ignore without counting as "skipped".
        _ => {}
    }
}

/// Map an `event_msg`. The ONLY billable one is `token_count` (see module docs);
/// `context_compacted` → Compaction; tool/patch ends → tool-call events; the rest
/// are control-flow markers ignored without counting as skipped.
fn push_event_msg(
    raw: &RawLine,
    ts: Option<jiff::Timestamp>,
    current_model: &Option<String>,
    session: &mut Session,
    model_counts: &mut BTreeMap<String, u64>,
) {
    match raw.payload.item_type.as_deref() {
        Some("token_count") => {
            // Per-request delta only — NEVER `total_token_usage` (cumulative).
            let Some(info) = raw.payload.info.as_ref() else {
                session.skipped_lines += 1;
                return;
            };
            let Some(last) = info.last_token_usage.as_ref() else {
                // A token_count with no per-request delta we can attribute (e.g.
                // an initial 0-usage marker) — nothing to bill, not an error.
                return;
            };
            let usage = map_last_usage(last);
            let has_thinking = last.reasoning_output_tokens.unwrap_or(0) > 0;
            if let Some(model) = current_model {
                *model_counts.entry(model.clone()).or_default() += 1;
            }
            session.events.push(Event {
                kind: EventKind::Assistant,
                ts,
                // No request_id: these are distinct requests; downstream dedup
                // gives each a unique key so they are not merged (module docs).
                request_id: None,
                model: current_model.clone(),
                usage: Some(usage),
                tool_calls: Vec::new(),
                sidechain: false,
                content_summary: None,
                content_chars: 0,
                thinking_chars: 0,
                has_thinking,
                tool_use_id: None,
                attachment_kind: None,
                item_count: 0,
            });
        }
        Some("context_compacted") => session.events.push(Event {
            kind: EventKind::Compaction,
            ts,
            request_id: None,
            model: None,
            usage: None,
            tool_calls: Vec::new(),
            sidechain: false,
            content_summary: Some("context compacted".to_string()),
            content_chars: 0,
            thinking_chars: 0,
            has_thinking: false,
            tool_use_id: None,
            attachment_kind: None,
            item_count: 0,
        }),
        // A completed MCP / patch / generic tool call reported as an event_msg.
        // We synthesize a ToolCall event so tool + MCP-server attribution sees it
        // even when the matching `response_item` function_call was absent.
        Some("mcp_tool_call_end") => {
            if let Some(call) = mcp_tool_call(raw) {
                session.events.push(Event {
                    kind: EventKind::ToolCall,
                    ts,
                    request_id: None,
                    model: None,
                    usage: None,
                    tool_calls: vec![call],
                    sidechain: false,
                    content_summary: None,
                    content_chars: 0,
                    thinking_chars: 0,
                    has_thinking: false,
                    tool_use_id: None,
                    attachment_kind: None,
                    item_count: 0,
                });
            }
        }
        // Control-flow / progress markers with no token spend: task_started,
        // task_complete, user_message, agent_message (text already captured from
        // response_item messages), patch_apply_end, etc. Ignored without counting
        // as "skipped" so the skip stat means *unexpected*.
        _ => {}
    }
}

/// Build a `ToolCall` from an `mcp_tool_call_end` event. Prefers the explicit
/// `invocation.{server,tool}`, falling back to a flat `tool` name; reconstructs
/// the canonical `mcp__server__tool` name so [`ToolCall::server_from_name`]
/// attributes it to the right MCP server.
fn mcp_tool_call(raw: &RawLine) -> Option<ToolCall> {
    let inv = raw.payload.invocation.as_ref();
    let server = inv.and_then(|i| i.server.clone());
    let tool = inv
        .and_then(|i| i.tool.clone())
        .or_else(|| raw.payload.tool.clone());
    let name = match (&server, &tool) {
        (Some(s), Some(t)) => format!("mcp__{s}__{t}"),
        (None, Some(t)) => t.clone(),
        _ => return None,
    };
    Some(ToolCall {
        server: server.or_else(|| ToolCall::server_from_name(&name)),
        id: raw.payload.call_id.clone().unwrap_or_default(),
        name,
        input_bytes: 0,
    })
}

/// Map a Codex `last_token_usage` (a per-request delta) DISJOINTLY into [`Usage`]
/// so `known_total()` equals its `total_tokens` and summing per-request totals
/// reconstructs the session's cumulative `total_tokens`.
///
/// OpenAI convention: `cached_input_tokens ⊆ input_tokens` and
/// `reasoning_output_tokens ⊆ output_tokens`, with
/// `total_tokens == input_tokens + output_tokens`. We therefore split the
/// overlapping subsets out so nothing is double counted:
///   - `cache_read = cached_input_tokens`
///   - `input      = input_tokens − cached_input_tokens`   (saturating)
///   - `thinking   = reasoning_output_tokens`
///   - `output     = output_tokens − reasoning_output_tokens` (saturating)
///
/// `saturating_sub` guards the (schema-violating but possible) underflow so we
/// never panic on bad data — CLAUDE.md §9.
fn map_last_usage(last: &RawTokenUsage) -> Usage {
    let cached = last.cached_input_tokens.unwrap_or(0);
    let reasoning = last.reasoning_output_tokens.unwrap_or(0);
    Usage {
        input: last.input_tokens.map(|i| i.saturating_sub(cached)),
        output: last.output_tokens.map(|o| o.saturating_sub(reasoning)),
        cache_creation: None,
        cache_creation_5m: None,
        cache_creation_1h: None,
        cache_read: last.cached_input_tokens,
        thinking: last.reasoning_output_tokens,
    }
}

// ---- raw line shapes (lenient: unknown fields ignored everywhere) ----

#[derive(Deserialize)]
struct RawLine {
    #[serde(rename = "type")]
    kind: Option<String>,
    timestamp: Option<String>,
    #[serde(default)]
    payload: RawPayload,
}

/// One `payload` object. Codex overloads this across every `type`, so all fields
/// are optional and we read only the ones relevant to the line's `type`.
#[derive(Deserialize, Default)]
struct RawPayload {
    /// Inner discriminator on `response_item` / `event_msg` lines.
    #[serde(rename = "type")]
    item_type: Option<String>,
    /// `session_meta.id`.
    #[allow(dead_code)]
    id: Option<String>,
    /// `session_meta.cwd` / `turn_context.cwd` — the project working directory.
    cwd: Option<String>,
    /// `turn_context.model` (and defensively any future `session_meta.model`).
    model: Option<String>,
    /// `message.role` (user / assistant / …).
    role: Option<String>,
    /// `message.content` — string or `[{type,text}]`.
    #[serde(default)]
    content: serde_json::Value,
    /// `function_call.name` (e.g. `shell`, `mcp__server__tool`).
    name: Option<String>,
    /// `function_call.arguments` (serialized JSON string).
    arguments: Option<String>,
    /// `function_call.call_id` / `*_tool_call_end.call_id`.
    call_id: Option<String>,
    /// `token_count.info`.
    info: Option<RawTokenInfo>,
    /// `mcp_tool_call_end.invocation`.
    invocation: Option<RawInvocation>,
    /// Flat `tool` name fallback on some tool-end events.
    tool: Option<String>,
}

/// `token_count.info` — the only place usage lives.
#[derive(Deserialize)]
struct RawTokenInfo {
    /// Cumulative, monotonic — NOT summed (would overcount ~100x). Kept for
    /// potential cross-checks; the per-request delta is what we bill.
    #[allow(dead_code)]
    total_token_usage: Option<RawTokenUsage>,
    /// Per-request delta — the value we map into [`Usage`].
    last_token_usage: Option<RawTokenUsage>,
}

/// A Codex token-usage block (OpenAI convention; see [`map_last_usage`]).
#[derive(Deserialize)]
struct RawTokenUsage {
    input_tokens: Option<u64>,
    cached_input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    reasoning_output_tokens: Option<u64>,
    #[allow(dead_code)]
    total_tokens: Option<u64>,
}

/// `mcp_tool_call_end.invocation` — server + tool name of a completed MCP call.
#[derive(Deserialize)]
struct RawInvocation {
    server: Option<String>,
    tool: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The reconciliation core: a per-request delta maps DISJOINTLY so its
    /// `known_total()` equals `total_tokens` (no double counting of the cached /
    /// reasoning subsets).
    #[test]
    fn last_usage_maps_disjointly_to_total_tokens() {
        let last = RawTokenUsage {
            input_tokens: Some(2000),
            cached_input_tokens: Some(500),
            output_tokens: Some(300),
            reasoning_output_tokens: Some(100),
            total_tokens: Some(2300),
        };
        let u = map_last_usage(&last);
        assert_eq!(u.cache_read, Some(500));
        assert_eq!(u.input, Some(1500), "input minus cached subset");
        assert_eq!(u.thinking, Some(100));
        assert_eq!(u.output, Some(200), "output minus reasoning subset");
        assert_eq!(u.cache_creation, None, "Codex has no cache-creation split");
        assert_eq!(
            u.known_total(),
            2300,
            "Σ disjoint fields == total_tokens for the request"
        );
    }

    /// Schema-violating underflow (cached > input) must saturate, never panic.
    #[test]
    fn underflow_saturates_instead_of_panicking() {
        let last = RawTokenUsage {
            input_tokens: Some(100),
            cached_input_tokens: Some(250),
            output_tokens: Some(40),
            reasoning_output_tokens: Some(90),
            total_tokens: Some(140),
        };
        let u = map_last_usage(&last);
        assert_eq!(u.input, Some(0));
        assert_eq!(u.output, Some(0));
        assert_eq!(u.cache_read, Some(250));
        assert_eq!(u.thinking, Some(90));
    }

    /// `CODEX_HOME` may be a comma-separated list of roots; each contributes its
    /// `sessions` + `archived_sessions` dirs. Empties and whitespace are dropped.
    #[test]
    fn codex_home_parses_comma_separated_roots() {
        std::env::set_var("CODEX_HOME", " /a/one , /b/two ,, /c/three ");
        let roots = codex_roots();
        std::env::remove_var("CODEX_HOME");
        assert_eq!(
            roots,
            vec![
                PathBuf::from("/a/one"),
                PathBuf::from("/b/two"),
                PathBuf::from("/c/three"),
            ],
            "trimmed, empties dropped"
        );
    }

    /// A token_count event becomes exactly one Assistant event tagged with the
    /// current model, and a context_compacted becomes a Compaction.
    #[test]
    fn token_count_and_compaction_map_to_events() {
        let mut session = Session {
            id: "s".into(),
            agent: "codex".into(),
            project: None,
            model: None,
            parent_session: None,
            started_at: None,
            ended_at: None,
            events: Vec::new(),
            sub_agents: Vec::new(),
            skipped_lines: 0,
        };
        let mut counts: BTreeMap<String, u64> = BTreeMap::new();
        let model = Some("gpt-5.5".to_string());

        let tc: RawLine = serde_json::from_str(
            r#"{"timestamp":"2026-06-15T10:00:00Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":1000,"cached_input_tokens":0,"output_tokens":200,"reasoning_output_tokens":50,"total_tokens":1200},"last_token_usage":{"input_tokens":1000,"cached_input_tokens":0,"output_tokens":200,"reasoning_output_tokens":50,"total_tokens":1200}}}}"#,
        )
        .unwrap();
        push_event_msg(
            &tc,
            parse_ts(&tc.timestamp),
            &model,
            &mut session,
            &mut counts,
        );

        let cc: RawLine = serde_json::from_str(
            r#"{"timestamp":"2026-06-15T10:00:01Z","type":"event_msg","payload":{"type":"context_compacted"}}"#,
        )
        .unwrap();
        push_event_msg(
            &cc,
            parse_ts(&cc.timestamp),
            &model,
            &mut session,
            &mut counts,
        );

        assert_eq!(session.events.len(), 2);
        assert_eq!(session.events[0].kind, EventKind::Assistant);
        assert_eq!(session.events[0].model.as_deref(), Some("gpt-5.5"));
        assert!(session.events[0].has_thinking, "reasoning_output_tokens>0");
        assert_eq!(session.events[0].usage.unwrap().known_total(), 1200);
        assert_eq!(session.events[1].kind, EventKind::Compaction);
        assert_eq!(counts.get("gpt-5.5"), Some(&1));
    }

    /// `mcp_tool_call_end` synthesizes a server-attributed ToolCall.
    #[test]
    fn mcp_tool_call_end_attributes_server() {
        let raw: RawLine = serde_json::from_str(
            r#"{"timestamp":"2026-06-15T10:00:00Z","type":"event_msg","payload":{"type":"mcp_tool_call_end","invocation":{"server":"acme-db","tool":"query"},"call_id":"c1"}}"#,
        )
        .unwrap();
        let call = mcp_tool_call(&raw).expect("a tool call");
        assert_eq!(call.name, "mcp__acme-db__query");
        assert_eq!(call.server.as_deref(), Some("acme-db"));
    }
}
