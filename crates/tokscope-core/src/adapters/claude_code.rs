//! Claude Code adapter — the MVP's fully implemented one.
//!
//! Reads `~/.claude/projects/<url-encoded-cwd>/*.jsonl` (READ-ONLY). Sub-agent
//! transcripts live in `<project>/<parent-session-uuid>/subagents/agent-<id>.jsonl`
//! and are attributed back to the parent via [`Session::parent_session`].
//!
//! Schema notes, VERIFIED against real local files (Claude Code v2.1.162, 2026-06-13):
//! - A single API request is written as 2–10 assistant lines (one per content
//!   block) that all repeat `message.usage` and share one `requestId`. Observed
//!   real ratios: 83 lines -> 26 requests; 642 -> 262. Accounting MUST dedup
//!   (CLAUDE.md §8.1) — that happens in `analysis::dedup`, not here; the adapter
//!   preserves the raw lines as events.
//! - `message.usage` keys: `input_tokens`, `output_tokens`,
//!   `cache_creation_input_tokens`, `cache_read_input_tokens`, plus a
//!   `cache_creation: { ephemeral_5m_input_tokens, ephemeral_1h_input_tokens }`
//!   breakdown (the TTLs are priced differently — see `pricing`).
//! - API-error lines have `model: "<synthetic>"` / `isApiErrorMessage: true` with
//!   zeroed usage that never hit the API; their usage is dropped.
//! - Sub-agent spawns are `tool_use` blocks named `Task` (legacy) or `Agent`
//!   (current), with `input.description` / `input.subagent_type`.
//! - Thinking blocks may be encrypted (`"thinking": ""` + `signature`), so
//!   thinking length is not always measurable from the transcript.
//! - Bookkeeping line types carrying no spend (VERIFIED on real files): `summary`,
//!   `mode`, `permission-mode`, `last-prompt`, `file-history-snapshot`,
//!   `attachment`, `ai-title`, `custom-title`, `queue-operation`, `agent-name` —
//!   ignored without counting as "skipped" so the skip stat means *unexpected*.
//! - TODO(verify): compaction markers (`subtype: "compact_boundary"` system lines,
//!   `isCompactSummary` user lines) were not present in sampled files; the mapping
//!   below is best-effort from documentation.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::adapters::Adapter;
use crate::error::CoreError;
use crate::model::{Event, EventKind, Session, SessionRef, SubAgent, ToolCall, Usage};

/// Claude Code (`~/.claude`).
pub struct ClaudeCode;

/// Data root: `$CLAUDE_CONFIG_DIR` when set (Claude Code honors the same
/// variable; tests use it to point at fixtures), else `~/.claude` resolved via
/// the `directories` crate — never a hardcoded `~`.
fn claude_dir() -> Option<PathBuf> {
    match std::env::var("CLAUDE_CONFIG_DIR") {
        Ok(dir) if !dir.trim().is_empty() => Some(PathBuf::from(dir)),
        _ => directories::BaseDirs::new().map(|b| b.home_dir().join(".claude")),
    }
}

impl Adapter for ClaudeCode {
    fn id(&self) -> &'static str {
        "claude-code"
    }

    fn detect(&self) -> bool {
        claude_dir().is_some_and(|d| d.join("projects").is_dir())
    }

    fn discover(&self) -> anyhow::Result<Vec<SessionRef>> {
        let root = claude_dir()
            .ok_or_else(|| anyhow::anyhow!("could not determine the home directory"))?
            .join("projects");
        let mut refs = Vec::new();
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
                project: project_label(entry.path(), &root),
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
            parent_session: parent_session_for(&r.path),
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
                Some("assistant") => push_assistant(raw, &mut session, &mut model_counts),
                Some("user") => push_user(raw, &mut session),
                Some("system") => push_system(raw, &mut session),
                // Bookkeeping lines that carry no token spend. VERIFIED present in
                // real v2.1.162 files: ai-title / custom-title (session titles),
                // queue-operation (message-queue ops), agent-name (sub-agent label),
                // attachment (deferred_tools / skill listings), file-history-snapshot,
                // mode, permission-mode, last-prompt. Kept `queued-message`/`progress`
                // from older builds defensively.
                Some(
                    "summary"
                    | "mode"
                    | "permission-mode"
                    | "last-prompt"
                    | "file-history-snapshot"
                    | "attachment"
                    | "ai-title"
                    | "custom-title"
                    | "queue-operation"
                    | "agent-name"
                    | "queued-message"
                    | "progress",
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
        session.model = model_counts
            .into_iter()
            .max_by_key(|(_, count)| *count)
            .map(|(model, _)| model);
        Ok(session)
    }
}

/// `<root>/<project-dir>/...` -> the project dir name.
fn project_label(path: &Path, root: &Path) -> Option<String> {
    path.strip_prefix(root)
        .ok()?
        .components()
        .next()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
}

/// Session id = file stem (`<uuid>.jsonl`, or `agent-<id>.jsonl` for sub-agents).
fn session_id_for(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

/// `.../<parent-session-uuid>/subagents/<file>.jsonl` -> parent session uuid.
fn parent_session_for(path: &Path) -> Option<String> {
    let comps: Vec<String> = path
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    let pos = comps.iter().rposition(|c| c == "subagents")?;
    (pos > 0).then(|| comps[pos - 1].clone())
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

fn push_assistant(raw: RawLine, session: &mut Session, model_counts: &mut BTreeMap<String, u64>) {
    let Some(msg) = raw.message else {
        session.skipped_lines += 1;
        return;
    };
    let ts = parse_ts(&raw.timestamp);
    let model = msg.model.clone();
    let synthetic = raw.is_api_error || model.as_deref() == Some("<synthetic>");
    if let Some(m) = &model {
        if !synthetic {
            *model_counts.entry(m.clone()).or_default() += 1;
        }
    }

    let mut tool_calls = Vec::new();
    let mut spawns = Vec::new();
    let mut content_chars = 0u64;
    let mut has_thinking = false;
    let mut summary = None;

    if let Some(blocks) = msg.content.as_array() {
        for block in blocks {
            match block.get("type").and_then(|t| t.as_str()) {
                Some("text") => {
                    let text = block.get("text").and_then(|t| t.as_str()).unwrap_or("");
                    content_chars += text.chars().count() as u64;
                    if summary.is_none() {
                        summary = snippet(text);
                    }
                }
                Some("thinking") => {
                    has_thinking = true;
                    let text = block.get("thinking").and_then(|t| t.as_str()).unwrap_or("");
                    content_chars += text.chars().count() as u64;
                }
                Some("redacted_thinking") => has_thinking = true,
                Some("tool_use") => {
                    let id = block
                        .get("id")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string();
                    let name = block
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown")
                        .to_string();
                    let input_bytes = block.get("input").map_or(0, |i| i.to_string().len() as u64);
                    content_chars += input_bytes;
                    // `Task` (legacy) / `Agent` (current) tool = sub-agent spawn.
                    if name == "Task" || name == "Agent" {
                        spawns.push(SubAgent {
                            tool_call_id: id.clone(),
                            agent_type: block
                                .pointer("/input/subagent_type")
                                .and_then(|v| v.as_str())
                                .map(str::to_string),
                            description: block
                                .pointer("/input/description")
                                .and_then(|v| v.as_str())
                                .map(str::to_string),
                            ts,
                        });
                    }
                    tool_calls.push(ToolCall {
                        server: ToolCall::server_from_name(&name),
                        id,
                        name,
                        input_bytes,
                    });
                }
                _ => {}
            }
        }
    } else if let Some(text) = msg.content.as_str() {
        content_chars += text.chars().count() as u64;
        summary = snippet(text);
    }

    // API-error lines report zeroed usage for requests that never completed —
    // dropping it keeps "absence ≠ zero" honest (the spend is unknown, not 0).
    let usage = if synthetic {
        None
    } else {
        msg.usage.map(RawUsage::into_usage)
    };

    session.events.push(Event {
        kind: EventKind::Assistant,
        ts,
        request_id: raw.request_id,
        model,
        usage,
        tool_calls,
        sidechain: raw.is_sidechain,
        content_summary: summary,
        content_chars,
        has_thinking,
    });
    for spawn in spawns {
        session.events.push(Event {
            kind: EventKind::SubAgentSpawn,
            ts,
            request_id: None,
            model: None,
            usage: None,
            tool_calls: Vec::new(),
            sidechain: raw.is_sidechain,
            content_summary: spawn.description.clone(),
            content_chars: 0,
            has_thinking: false,
        });
        session.sub_agents.push(spawn);
    }
}

fn push_user(raw: RawLine, session: &mut Session) {
    let ts = parse_ts(&raw.timestamp);
    let mut kind = if raw.is_compact_summary {
        EventKind::Compaction // TODO(verify): see module docs
    } else {
        EventKind::User
    };
    let mut summary = None;
    let mut content_chars = 0u64;

    match raw.message.as_ref().map(|m| &m.content) {
        Some(serde_json::Value::String(text)) => {
            content_chars = text.chars().count() as u64;
            summary = snippet(text);
        }
        Some(value @ serde_json::Value::Array(blocks)) => {
            let is_tool_result = blocks
                .iter()
                .any(|b| b.get("type").and_then(|t| t.as_str()) == Some("tool_result"));
            if is_tool_result {
                kind = EventKind::ToolResult;
                // Serialized result size = what this result weighs in the context
                // window (incl. base64 images) — input for context-bloat analysis.
                content_chars = value.to_string().len() as u64;
            } else {
                for block in blocks {
                    if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                        content_chars += text.chars().count() as u64;
                        if summary.is_none() {
                            summary = snippet(text);
                        }
                    }
                }
            }
        }
        _ => {}
    }

    session.events.push(Event {
        kind,
        ts,
        request_id: None,
        model: None,
        usage: None,
        tool_calls: Vec::new(),
        sidechain: raw.is_sidechain,
        content_summary: summary,
        content_chars,
        has_thinking: false,
    });
}

fn push_system(raw: RawLine, session: &mut Session) {
    let kind = if raw.subtype.as_deref() == Some("compact_boundary") {
        EventKind::Compaction // TODO(verify): see module docs
    } else {
        EventKind::System
    };
    session.events.push(Event {
        kind,
        ts: parse_ts(&raw.timestamp),
        request_id: None,
        model: None,
        usage: None,
        tool_calls: Vec::new(),
        sidechain: raw.is_sidechain,
        content_summary: raw.content.as_deref().and_then(snippet),
        content_chars: raw
            .content
            .as_deref()
            .map_or(0, |c| c.chars().count() as u64),
        has_thinking: false,
    });
}

// ---- raw line shapes (lenient: unknown fields ignored everywhere) ----

#[derive(Deserialize)]
struct RawLine {
    #[serde(rename = "type")]
    kind: Option<String>,
    timestamp: Option<String>,
    #[serde(rename = "requestId")]
    request_id: Option<String>,
    #[serde(rename = "isSidechain", default)]
    is_sidechain: bool,
    #[serde(rename = "isCompactSummary", default)]
    is_compact_summary: bool,
    #[serde(rename = "isApiErrorMessage", default)]
    is_api_error: bool,
    subtype: Option<String>,
    /// Top-level content on `system` lines.
    content: Option<String>,
    message: Option<RawMessage>,
}

#[derive(Deserialize)]
struct RawMessage {
    model: Option<String>,
    #[serde(default)]
    content: serde_json::Value,
    usage: Option<RawUsage>,
}

#[derive(Deserialize)]
struct RawUsage {
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    cache_creation_input_tokens: Option<u64>,
    cache_read_input_tokens: Option<u64>,
    cache_creation: Option<RawCacheCreation>,
}

#[derive(Deserialize)]
struct RawCacheCreation {
    ephemeral_5m_input_tokens: Option<u64>,
    ephemeral_1h_input_tokens: Option<u64>,
}

impl RawUsage {
    fn into_usage(self) -> Usage {
        Usage {
            input: self.input_tokens,
            output: self.output_tokens,
            cache_creation: self.cache_creation_input_tokens,
            cache_creation_5m: self
                .cache_creation
                .as_ref()
                .and_then(|c| c.ephemeral_5m_input_tokens),
            cache_creation_1h: self
                .cache_creation
                .as_ref()
                .and_then(|c| c.ephemeral_1h_input_tokens),
            cache_read: self.cache_read_input_tokens,
            // Claude Code never reports thinking tokens separately (§8.2);
            // dedup flags suspected undercounts instead.
            thinking: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parent_session_is_derived_from_subagents_path() {
        let p = Path::new("projects/F--demo/1111-2222/subagents/agent-abc.jsonl");
        assert_eq!(parent_session_for(p).as_deref(), Some("1111-2222"));
        let normal = Path::new("projects/F--demo/1111-2222.jsonl");
        assert_eq!(parent_session_for(normal), None);
    }

    #[test]
    fn legacy_task_tool_counts_as_spawn() {
        // Same branch handles "Task" and "Agent"; cover the legacy name here since
        // fixtures use the current one.
        let line = r#"{"type":"assistant","timestamp":"2026-06-01T10:00:00Z","requestId":"req_t","message":{"role":"assistant","model":"claude-sonnet-4-5","content":[{"type":"tool_use","id":"t1","name":"Task","input":{"description":"d","prompt":"p","subagent_type":"general-purpose"}}],"usage":{"input_tokens":10,"output_tokens":5}}}"#;
        let raw: RawLine = serde_json::from_str(line).unwrap();
        let mut session = Session {
            id: "s".into(),
            agent: "claude-code".into(),
            project: None,
            model: None,
            parent_session: None,
            started_at: None,
            ended_at: None,
            events: Vec::new(),
            sub_agents: Vec::new(),
            skipped_lines: 0,
        };
        push_assistant(raw, &mut session, &mut BTreeMap::new());
        assert_eq!(session.sub_agents.len(), 1);
        assert_eq!(
            session.sub_agents[0].agent_type.as_deref(),
            Some("general-purpose")
        );
        assert_eq!(session.events.len(), 2, "assistant event + spawn event");
        assert_eq!(session.events[1].kind, EventKind::SubAgentSpawn);
    }
}
