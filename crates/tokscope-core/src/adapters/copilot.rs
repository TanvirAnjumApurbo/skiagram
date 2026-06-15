//! GitHub Copilot CLI adapter — STRUCTURAL ONLY (v0.4).
//!
//! Reads `~/.copilot/session-state/` (READ-ONLY) in two layouts seen on real
//! installs:
//! - current: `~/.copilot/session-state/<session-uuid>/events.jsonl`
//! - older flat: `~/.copilot/session-state/<session-uuid>.jsonl`
//!
//! Both are discovered. `$COPILOT_HOME` overrides the `~/.copilot` root (the
//! integration test points it at fixtures; `directories::BaseDirs` does not read
//! `$HOME` on Windows, so an explicit override is required for testability —
//! mirrors Claude Code's `CLAUDE_CONFIG_DIR`).
//!
//! ## Honesty: Copilot does NOT log per-request token usage (CLAUDE.md §8.5)
//!
//! Copilot's event log records conversation STRUCTURE — sessions, model choices
//! and switches, turns, tool requests/results, reasoning presence, and content
//! sizes — but it does **not** record per-request billing token counts
//! (input/output/cache). The only token-ish fields are `session.truncation`
//! (context-window limits, not billing) and a `responseTokenLimit` in tool
//! telemetry (a cap, not a count). Neither is billable usage.
//!
//! Therefore every [`Event::usage`] this adapter emits is `None` — UNKNOWN, not
//! zero. We surface the structure and leave spend explicitly unknown rather than
//! invent or estimate it. (`summary` will show 0 priced tokens but a populated
//! `by_tool` — that is the correct, honest outcome.)
//!
//! Schema (VERIFIED on real local files, 2026-06-16 — ≥3 nested sessions plus a
//! flat one were present under `~/.copilot/session-state/`; their content is
//! read-only and contains real prompts, so it is not reproduced here. Every line
//! is `{"type", "data", "id", "timestamp", "parentId"}`):
//! - `session.start` — `data.{sessionId, copilotVersion, startTime, selectedModel,
//!   context.cwd}`. Source of session id / project (cwd basename) / start model.
//! - `session.resume` — `data.{resumeTime, eventCount, context.cwd}`.
//! - `session.model_change` — `data.{previousModel, newModel}`. Tracked toward the
//!   chosen `Session.model` (most-frequently-seen wins; see below).
//! - `user.message` — `data.content` (+ `transformedContent`, `attachments[]`).
//! - `assistant.turn_start` / `assistant.turn_end` — turn boundaries (mapped to
//!   `System` markers; kept minimal).
//! - `assistant.message` — `data.{messageId, content, toolRequests[], reasoningText,
//!   reasoningOpaque}`. Each `toolRequests[]` -> a [`ToolCall`]; reasoning presence
//!   sets `has_thinking`.
//! - `tool.execution_start` — `data.{toolCallId, toolName, arguments}`.
//! - `tool.execution_complete` — `data.{toolCallId, success, result, model}` ->
//!   `ToolResult` linked by `tool_use_id = toolCallId`.
//! - `session.truncation` — context-window fill info, NOT per-request billing.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::adapters::Adapter;
use crate::error::CoreError;
use crate::model::{Event, EventKind, Session, SessionRef, ToolCall};

/// GitHub Copilot CLI (`~/.copilot`).
pub struct Copilot;

/// Data root: `$COPILOT_HOME` when set (tests use it to point at fixtures), else
/// `~/.copilot` resolved via the `directories` crate — never a hardcoded `~`.
/// An explicit override is required because `directories::BaseDirs` ignores
/// `$HOME` on Windows (so the integration test could not otherwise relocate it).
fn copilot_dir() -> Option<PathBuf> {
    match std::env::var("COPILOT_HOME") {
        Ok(dir) if !dir.trim().is_empty() => Some(PathBuf::from(dir)),
        _ => directories::BaseDirs::new().map(|b| b.home_dir().join(".copilot")),
    }
}

impl Adapter for Copilot {
    fn id(&self) -> &'static str {
        "copilot"
    }

    fn detect(&self) -> bool {
        copilot_dir().is_some_and(|d| d.join("session-state").is_dir())
    }

    fn discover(&self) -> anyhow::Result<Vec<SessionRef>> {
        let root = copilot_dir()
            .ok_or_else(|| anyhow::anyhow!("could not determine the home directory"))?
            .join("session-state");
        let mut refs = Vec::new();
        // Walk the tree so BOTH `<uuid>/events.jsonl` (current) and flat
        // `<uuid>.jsonl` (older) layouts are picked up. Any other `*.jsonl`
        // Copilot writes under here is harmless to include — parse is lenient.
        for entry in walkdir::WalkDir::new(&root).follow_links(false) {
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
                // Project lives INSIDE the file (`session.start` cwd), not in the
                // path, so it is resolved during `parse`, not here.
                project: None,
                size_bytes: meta.as_ref().map_or(0, |m| m.len()),
                modified: meta
                    .and_then(|m| m.modified().ok())
                    .and_then(|t| jiff::Timestamp::try_from(t).ok()),
            });
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
            // Copilot has no sub-agent transcripts; spend is never re-parented.
            parent_session: None,
            started_at: None,
            ended_at: None,
            events: Vec::new(),
            sub_agents: Vec::new(),
            skipped_lines: 0,
        };
        // BTreeMap so model tie-breaking is deterministic.
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
            match raw.kind.as_deref() {
                Some("session.start") => apply_session_start(&raw, &mut session, &mut model_counts),
                Some("session.resume") => apply_session_resume(&raw, &mut session),
                Some("session.model_change") => apply_model_change(&raw, &mut model_counts),
                Some("user.message") => push_user(&raw, &mut session),
                Some("assistant.message") => push_assistant(&raw, &mut session, &mut model_counts),
                Some("tool.execution_complete") => push_tool_result(&raw, &mut session),
                // Bookkeeping / boundary lines that carry no token spend and no
                // tool linkage we need. Kept explicit so the skip stat means
                // *unexpected*, not "merely uninteresting".
                Some(
                    "assistant.turn_start"
                    | "assistant.turn_end"
                    | "tool.execution_start"
                    | "session.truncation",
                ) => {}
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
        // Most-frequently-seen model wins (mirrors Claude Code). `selectedModel`
        // and every `newModel` each contribute one tally. Deterministic: iterating
        // the BTreeMap ascending, `max_by_key` keeps the lexicographically-largest
        // model id among any count-tie.
        session.model = model_counts
            .into_iter()
            .max_by_key(|(_, count)| *count)
            .map(|(model, _)| model);
        Ok(session)
    }
}

/// Session id = parent dir name for `<uuid>/events.jsonl`, else the file stem for
/// flat `<uuid>.jsonl`.
fn session_id_for(path: &Path) -> String {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned());
    if stem == "events" {
        if let Some(parent) = path.parent().and_then(|p| p.file_name()) {
            return parent.to_string_lossy().into_owned();
        }
    }
    stem
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

/// `/home/dev/acme-app` (or `C:\…\acme-app`) -> `acme-app`. Project label only.
fn cwd_basename(cwd: &str) -> Option<String> {
    let trimmed = cwd.trim_end_matches(['/', '\\']);
    let base = trimmed
        .rsplit(['/', '\\'])
        .find(|s| !s.is_empty())
        .unwrap_or(trimmed);
    (!base.is_empty()).then(|| base.to_string())
}

fn apply_session_start(
    raw: &RawLine,
    session: &mut Session,
    model_counts: &mut BTreeMap<String, u64>,
) {
    let Some(data) = &raw.data else { return };
    if session.project.is_none() {
        if let Some(cwd) = data.context.as_ref().and_then(|c| c.cwd.as_deref()) {
            session.project = cwd_basename(cwd);
        }
    }
    if let Some(model) = data.selected_model.as_deref() {
        if !model.is_empty() {
            *model_counts.entry(model.to_string()).or_default() += 1;
        }
    }
    // A System marker so the session start is visible on the timeline; usage is
    // unknown (Copilot logs none).
    session.events.push(Event {
        kind: EventKind::System,
        ts: parse_ts(&raw.timestamp).or_else(|| parse_ts(&data.start_time)),
        request_id: None,
        model: data.selected_model.clone(),
        usage: None,
        tool_calls: Vec::new(),
        sidechain: false,
        content_summary: Some("session start".to_string()),
        content_chars: 0,
        has_thinking: false,
    });
}

fn apply_session_resume(raw: &RawLine, session: &mut Session) {
    let Some(data) = &raw.data else { return };
    if session.project.is_none() {
        if let Some(cwd) = data.context.as_ref().and_then(|c| c.cwd.as_deref()) {
            session.project = cwd_basename(cwd);
        }
    }
    session.events.push(Event {
        kind: EventKind::System,
        ts: parse_ts(&raw.timestamp).or_else(|| parse_ts(&data.resume_time)),
        request_id: None,
        model: None,
        usage: None,
        tool_calls: Vec::new(),
        sidechain: false,
        content_summary: Some("session resume".to_string()),
        content_chars: 0,
        has_thinking: false,
    });
}

fn apply_model_change(raw: &RawLine, model_counts: &mut BTreeMap<String, u64>) {
    if let Some(model) = raw
        .data
        .as_ref()
        .and_then(|d| d.new_model.as_deref())
        .filter(|m| !m.is_empty())
    {
        *model_counts.entry(model.to_string()).or_default() += 1;
    }
}

fn push_user(raw: &RawLine, session: &mut Session) {
    let content = raw
        .data
        .as_ref()
        .and_then(|d| d.content.as_deref())
        .unwrap_or("");
    session.events.push(Event {
        kind: EventKind::User,
        ts: parse_ts(&raw.timestamp),
        request_id: None,
        model: None,
        usage: None,
        tool_calls: Vec::new(),
        sidechain: false,
        content_summary: snippet(content),
        content_chars: content.chars().count() as u64,
        has_thinking: false,
    });
}

fn push_assistant(raw: &RawLine, session: &mut Session, model_counts: &mut BTreeMap<String, u64>) {
    let Some(data) = &raw.data else {
        session.skipped_lines += 1;
        return;
    };
    let ts = parse_ts(&raw.timestamp);

    let content = data.content.as_deref().unwrap_or("");
    let mut content_chars = content.chars().count() as u64;

    // Reasoning presence (text and/or opaque/encrypted) => has_thinking. Only the
    // plain `reasoningText` is measurable; opaque reasoning is unmeasurable
    // (mirrors Claude Code's encrypted-thinking handling).
    let reasoning_text = data.reasoning_text.as_deref().unwrap_or("");
    let has_thinking = !reasoning_text.is_empty() || data.reasoning_opaque.is_some();
    content_chars += reasoning_text.chars().count() as u64;

    let mut tool_calls = Vec::new();
    for req in &data.tool_requests {
        let name = req.name.clone().unwrap_or_else(|| "unknown".to_string());
        // Input size = serialized `arguments`, matching how Claude Code sizes a
        // tool call's input (a proxy for what it weighs in the window).
        let input_bytes = req
            .arguments
            .as_ref()
            .map_or(0, |a| a.to_string().len() as u64);
        content_chars += input_bytes;
        tool_calls.push(ToolCall {
            server: ToolCall::server_from_name(&name),
            id: req.tool_call_id.clone().unwrap_or_default(),
            name,
            input_bytes,
        });
    }

    // Copilot does not record assistant model per message in the cases sampled,
    // but if a future build adds one, fold it into the model tally too.
    let model = data.model.clone();
    if let Some(m) = &model {
        if !m.is_empty() {
            *model_counts.entry(m.clone()).or_default() += 1;
        }
    }

    session.events.push(Event {
        kind: EventKind::Assistant,
        ts,
        request_id: None,
        model,
        // HONEST: Copilot logs no per-request usage (§8.5) — unknown, not zero.
        usage: None,
        tool_calls,
        sidechain: false,
        content_summary: snippet(content),
        content_chars,
        has_thinking,
    });
}

fn push_tool_result(raw: &RawLine, session: &mut Session) {
    let Some(data) = &raw.data else {
        session.skipped_lines += 1;
        return;
    };
    // Serialized result size = what this result weighs in the context window —
    // input for context-bloat analysis (mirrors Claude Code's tool-result sizing).
    let content_chars = data
        .result
        .as_ref()
        .map_or(0, |v| v.to_string().len() as u64);
    session.events.push(Event {
        kind: EventKind::ToolResult,
        ts: parse_ts(&raw.timestamp),
        request_id: None,
        model: data.model.clone(),
        usage: None,
        tool_calls: Vec::new(),
        sidechain: false,
        // The matching tool call's id, so results link back to their call.
        content_summary: data.tool_name.clone().or_else(|| data.tool_call_id.clone()),
        content_chars,
        has_thinking: false,
    });
}

// ---- raw line shapes (lenient: unknown fields ignored everywhere) ----

/// One Copilot event line: `{type, data, id, timestamp, parentId}`.
#[derive(Deserialize)]
struct RawLine {
    #[serde(rename = "type")]
    kind: Option<String>,
    timestamp: Option<String>,
    data: Option<RawData>,
}

/// The `data` object. A single flat struct covers every `type` we read; absent
/// fields simply stay `None` (lenient — one shape, many event kinds).
#[derive(Deserialize, Default)]
#[serde(default)]
struct RawData {
    // session.start
    #[serde(rename = "selectedModel")]
    selected_model: Option<String>,
    #[serde(rename = "startTime")]
    start_time: Option<String>,
    context: Option<RawContext>,
    // session.resume
    #[serde(rename = "resumeTime")]
    resume_time: Option<String>,
    // session.model_change
    #[serde(rename = "newModel")]
    new_model: Option<String>,
    // user.message / assistant.message
    content: Option<String>,
    #[serde(rename = "reasoningText")]
    reasoning_text: Option<String>,
    #[serde(rename = "reasoningOpaque")]
    reasoning_opaque: Option<serde_json::Value>,
    #[serde(rename = "toolRequests")]
    tool_requests: Vec<RawToolRequest>,
    // tool.execution_complete / tool.execution_start
    #[serde(rename = "toolCallId")]
    tool_call_id: Option<String>,
    #[serde(rename = "toolName")]
    tool_name: Option<String>,
    result: Option<serde_json::Value>,
    /// Present on some assistant / tool-completion lines.
    model: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawContext {
    cwd: Option<String>,
}

/// One entry of `assistant.message`'s `toolRequests[]`.
#[derive(Deserialize, Default)]
#[serde(default)]
struct RawToolRequest {
    #[serde(rename = "toolCallId")]
    tool_call_id: Option<String>,
    name: Option<String>,
    arguments: Option<serde_json::Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_session() -> Session {
        Session {
            id: "s".into(),
            agent: "copilot".into(),
            project: None,
            model: None,
            parent_session: None,
            started_at: None,
            ended_at: None,
            events: Vec::new(),
            sub_agents: Vec::new(),
            skipped_lines: 0,
        }
    }

    #[test]
    fn tool_request_maps_to_tool_call_with_server_and_no_usage() {
        let line = r#"{"type":"assistant.message","timestamp":"2026-06-16T10:00:00Z","data":{"messageId":"m1","content":"on it","toolRequests":[{"toolCallId":"call_1","name":"mcp__github__search_issues","arguments":{"query":"x"},"type":"function"}]}}"#;
        let raw: RawLine = serde_json::from_str(line).unwrap();
        let mut session = empty_session();
        push_assistant(&raw, &mut session, &mut BTreeMap::new());

        assert_eq!(session.events.len(), 1);
        let ev = &session.events[0];
        assert_eq!(ev.kind, EventKind::Assistant);
        assert!(
            ev.usage.is_none(),
            "Copilot logs no per-request usage (§8.5)"
        );
        assert_eq!(ev.tool_calls.len(), 1);
        assert_eq!(ev.tool_calls[0].id, "call_1");
        assert_eq!(ev.tool_calls[0].name, "mcp__github__search_issues");
        assert_eq!(ev.tool_calls[0].server.as_deref(), Some("github"));
        assert!(ev.tool_calls[0].input_bytes > 0);
    }

    #[test]
    fn reasoning_sets_has_thinking_but_not_usage() {
        let line = r#"{"type":"assistant.message","timestamp":"2026-06-16T10:00:01Z","data":{"messageId":"m2","content":"answer","reasoningText":"let me think about this"}}"#;
        let raw: RawLine = serde_json::from_str(line).unwrap();
        let mut session = empty_session();
        push_assistant(&raw, &mut session, &mut BTreeMap::new());

        let ev = &session.events[0];
        assert!(ev.has_thinking, "reasoningText must set has_thinking");
        assert!(ev.usage.is_none());
        // content_chars includes the visible answer + reasoning text length.
        assert_eq!(
            ev.content_chars,
            "answer".len() as u64 + "let me think about this".len() as u64
        );
    }

    #[test]
    fn opaque_reasoning_alone_sets_has_thinking() {
        let line = r#"{"type":"assistant.message","timestamp":"2026-06-16T10:00:02Z","data":{"messageId":"m3","content":"hi","reasoningOpaque":"BASE64=="}}"#;
        let raw: RawLine = serde_json::from_str(line).unwrap();
        let mut session = empty_session();
        push_assistant(&raw, &mut session, &mut BTreeMap::new());
        assert!(session.events[0].has_thinking);
    }

    #[test]
    fn tool_result_links_by_tool_call_id_and_sizes_result() {
        let line = r#"{"type":"tool.execution_complete","timestamp":"2026-06-16T10:00:03Z","data":{"toolCallId":"call_1","toolName":"bash","success":true,"result":{"content":"ok"}}}"#;
        let raw: RawLine = serde_json::from_str(line).unwrap();
        let mut session = empty_session();
        push_tool_result(&raw, &mut session);

        let ev = &session.events[0];
        assert_eq!(ev.kind, EventKind::ToolResult);
        assert!(ev.usage.is_none());
        assert!(ev.content_chars > 0, "serialized result has size");
    }

    #[test]
    fn session_id_prefers_parent_dir_for_nested_events_file() {
        let nested = Path::new("session-state/abc-123/events.jsonl");
        assert_eq!(session_id_for(nested), "abc-123");
        let flat = Path::new("session-state/def-456.jsonl");
        assert_eq!(session_id_for(flat), "def-456");
    }

    #[test]
    fn cwd_basename_handles_both_separators() {
        assert_eq!(
            cwd_basename("/home/dev/acme-app").as_deref(),
            Some("acme-app")
        );
        assert_eq!(
            cwd_basename("C:\\Users\\dev\\acme-app\\").as_deref(),
            Some("acme-app")
        );
    }

    #[test]
    fn model_change_tallies_new_model() {
        let line = r#"{"type":"session.model_change","timestamp":"2026-06-16T10:00:04Z","data":{"previousModel":"gpt-a","newModel":"gpt-b"}}"#;
        let raw: RawLine = serde_json::from_str(line).unwrap();
        let mut counts = BTreeMap::new();
        apply_model_change(&raw, &mut counts);
        assert_eq!(counts.get("gpt-b"), Some(&1));
        assert!(
            !counts.contains_key("gpt-a"),
            "only the new model is tallied"
        );
    }
}
