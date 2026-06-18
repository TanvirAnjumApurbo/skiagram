//! Gemini CLI adapter — roadmap v0.4, VERIFIED real schema (2026-06-17).
//!
//! Reads `~/.gemini/tmp/<project>/chats/session-<ISO8601>-<short>.jsonl`
//! (READ-ONLY). `$GEMINI_HOME` overrides the `~/.gemini` root (tests point it at
//! fixtures; `directories::BaseDirs` ignores `$HOME` on Windows, so an explicit
//! override is required for testability — mirrors Claude Code's
//! `CLAUDE_CONFIG_DIR` and Codex's `CODEX_HOME`).
//!
//! HISTORY: through 2026-06-16 this was a stub because the only `~/.gemini` data
//! on the test machine was Google **Antigravity** (an IDE), which writes
//! `antigravity*/` + `tmp/<hash>/logs.json` (UI telemetry, no token transcript).
//! Once the real Gemini CLI was installed and run (2026-06-17) it wrote genuine,
//! billable per-message token usage under `tmp/<project>/chats/` — so this is now
//! a real adapter. [`detect`] looks specifically for a `chats/session-*.jsonl`
//! file (which Antigravity never writes), so it no longer false-positives on an
//! Antigravity-only install.
//!
//! ## Schema (VERIFIED on real local files, Gemini CLI, gemini-3-flash-preview)
//!
//! One JSON object per line. Three line shapes, interleaved:
//! - **header** (first line): `{sessionId, projectHash, startTime, lastUpdated,
//!   kind:"main"}` — no `type`. Source of `started_at`.
//! - **`$set` snapshot**: `{"$set": {...}}` — a UI/state checkpoint (holds the
//!   initial `session_context` message, periodically re-set). The running
//!   transcript is NOT here; we ignore `$set` lines entirely.
//! - **message**: `{id, timestamp, type, content, ...}`:
//!   - `type:"user"` — `content:[{text}]` (a prompt) or `content:[{functionResponse}]`
//!     (a tool result, linked by `functionResponse.id`).
//!   - `type:"gemini"` (assistant) — `content` (string, the visible answer),
//!     `thoughts:[{subject,description,timestamp}]` (PLAINTEXT reasoning — unlike
//!     Claude's mostly-encrypted thinking), `model` (e.g. `gemini-3-flash-preview`),
//!     `toolCalls:[{id,name,args,...}]`, and the all-important `tokens` block.
//!   - `type:"info"` / `type:"error"` — UI notices, no usage (ignored, not skipped).
//!
//! ## THE token block & correctness rules (CLAUDE.md §8.1/§8.2)
//!
//! Each `gemini` message carries
//! `tokens: {input, output, cached, thoughts, tool, total}`. VERIFIED on real
//! data: `total == input + output + thoughts` (so `thoughts` is DISJOINT from
//! `output` — the Codex `reasoning_output_tokens` case, not Claude's
//! already-included thinking) and `cached ⊆ input` (OpenAI/Gemini convention).
//! `tool` was observed 0 and is not part of `total`; we map only the verified
//! fields. We therefore split the overlapping subsets out (see [`map_tokens`]) so
//! nothing is double-counted and `known_total() == total`.
//!
//! **Whole-message re-serialization (the Gemini dedup case).** Gemini rewrites the
//! ENTIRE message line as it streams / resolves tool calls: the same `id` appears
//! 2+ times, with content growing, `toolCalls` appended, and `tokens` finalizing.
//! This is NOT Claude's "one line per complementary content block" — it is the
//! same message superseded. So we dedup by message `id` AT PARSE TIME with
//! last-wins content / max-merge usage (a later line never *loses* tokens), rather
//! than relying on the downstream requestId char-summing dedup (which assumes
//! complementary blocks and would double-count `thoughts`). Each surviving message
//! still gets `request_id = Some(id)` for traceability; downstream dedup then sees
//! one line per request and is a no-op.
//!
//! `gemini-*` models are NOT in skiagram's embedded pricing snapshot, so their
//! cost renders as "unpriced" — correct and honest (§8.7): we never guess a price.

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::adapters::Adapter;
use crate::error::CoreError;
use crate::model::{Event, EventKind, Session, SessionRef, ToolCall, Usage};

/// Gemini CLI (`~/.gemini`).
pub struct Gemini;

/// Data root: `$GEMINI_HOME` when set (tests use it to point at fixtures), else
/// `~/.gemini` resolved via the `directories` crate — never a hardcoded `~`. An
/// explicit override is required because `directories::BaseDirs` ignores `$HOME`
/// on Windows (so the integration test could not otherwise relocate it).
fn gemini_root() -> Option<PathBuf> {
    match std::env::var("GEMINI_HOME") {
        Ok(dir) if !dir.trim().is_empty() => Some(PathBuf::from(dir)),
        _ => directories::BaseDirs::new().map(|b| b.home_dir().join(".gemini")),
    }
}

/// Is this a Gemini CLI chat transcript, i.e. `.../chats/session-*.jsonl`? This is
/// the precise signal that distinguishes the real CLI from an Antigravity-only
/// install (which never writes a `chats/` dir).
fn is_session_file(path: &Path) -> bool {
    path.extension().and_then(|x| x.to_str()) == Some("jsonl")
        && path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("session-"))
        && path
            .parent()
            .and_then(|d| d.file_name())
            .and_then(|n| n.to_str())
            == Some("chats")
}

/// Project label = the `<project>` directory under `tmp/`, i.e. the parent of the
/// `chats/` dir holding the session file (Gemini's own friendly project name,
/// e.g. `skiagram`). The on-disk file carries only an opaque `projectHash`.
fn project_from_path(path: &Path) -> Option<String> {
    path.parent() // .../chats
        .and_then(|chats| chats.parent()) // .../<project>
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().into_owned())
}

impl Adapter for Gemini {
    fn id(&self) -> &'static str {
        "gemini"
    }

    fn detect(&self) -> bool {
        let Some(tmp) = gemini_root().map(|r| r.join("tmp")) else {
            return false;
        };
        // Bounded walk (tmp/<project>/chats/session-*.jsonl is depth 3); stop at
        // the first real session file so detection stays cheap.
        walkdir::WalkDir::new(&tmp)
            .max_depth(3)
            .follow_links(false)
            .into_iter()
            .filter_map(Result::ok)
            .any(|e| e.file_type().is_file() && is_session_file(e.path()))
    }

    fn discover(&self) -> anyhow::Result<Vec<SessionRef>> {
        let root = gemini_root().ok_or_else(|| {
            anyhow::anyhow!("could not determine the home directory (set GEMINI_HOME)")
        })?;
        let tmp = root.join("tmp");
        let mut refs = Vec::new();
        if tmp.is_dir() {
            for entry in walkdir::WalkDir::new(&tmp).max_depth(3).follow_links(false) {
                let entry = match entry {
                    Ok(e) => e,
                    Err(e) => {
                        tracing::debug!("skipping unreadable directory entry: {e}");
                        continue;
                    }
                };
                if !entry.file_type().is_file() || !is_session_file(entry.path()) {
                    continue;
                }
                let meta = entry.metadata().ok();
                refs.push(SessionRef {
                    path: entry.path().to_path_buf(),
                    agent: self.id().to_string(),
                    project: project_from_path(entry.path()),
                    size_bytes: meta.as_ref().map_or(0, |m| m.len()),
                    modified: meta
                        .and_then(|m| m.modified().ok())
                        .and_then(|t| jiff::Timestamp::try_from(t).ok()),
                });
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
        let id = session_id_for(&r.path);
        let project = r.project.clone().or_else(|| project_from_path(&r.path));
        Ok(parse_reader(
            BufReader::new(file),
            id,
            project,
            self.id(),
            &r.path,
        ))
    }
}

/// Session id = file stem (`session-<ISO8601>-<short>`), unique on disk and stable
/// across discovery. The embedded `sessionId` is a uuid but the stem is what
/// discovery keys on.
fn session_id_for(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

/// Core parse: stream the JSONL, deduping `gemini` messages by `id` (last-wins
/// content, max-merge usage) so re-serialized lines collapse into one request.
/// Factored out of [`Gemini::parse`] so tests can feed an in-memory reader.
fn parse_reader<R: BufRead>(
    reader: R,
    id: String,
    project: Option<String>,
    agent: &str,
    path_for_logs: &Path,
) -> Session {
    let mut session = Session {
        id,
        agent: agent.to_string(),
        project,
        model: None,
        parent_session: None,
        started_at: None,
        ended_at: None,
        events: Vec::new(),
        sub_agents: Vec::new(),
        skipped_lines: 0,
    };
    // message id -> index of its Assistant event in `session.events`, so a
    // re-serialized line updates the existing event in place instead of adding a
    // duplicate request.
    let mut gemini_idx: HashMap<String, usize> = HashMap::new();
    let mut model_counts: HashMap<String, u64> = HashMap::new();
    let mut header_start: Option<jiff::Timestamp> = None;

    for (lineno, line) in reader.lines().enumerate() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                session.skipped_lines += 1;
                tracing::debug!(
                    "{}:{}: unreadable line: {e}",
                    path_for_logs.display(),
                    lineno + 1
                );
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
                tracing::debug!(
                    "{}:{}: unparseable JSON: {e}",
                    path_for_logs.display(),
                    lineno + 1
                );
                continue;
            }
        };
        // `$set` is a UI/state snapshot, not transcript — ignore (not "skipped").
        if raw.set.is_some() {
            continue;
        }
        let ts = parse_ts(&raw.timestamp);
        match raw.msg_type.as_deref() {
            Some("gemini") => {
                apply_gemini(&raw, ts, &mut session, &mut gemini_idx, &mut model_counts)
            }
            Some("user") => apply_user(&raw, ts, &mut session),
            // Known UI notices with no token spend — ignored, not counted as skips
            // (so `skipped_lines` keeps meaning "unexpected").
            Some("info") | Some("error") => {}
            Some(_) => {
                session.skipped_lines += 1;
                tracing::debug!(
                    "{}:{}: unknown message type {:?}",
                    path_for_logs.display(),
                    lineno + 1,
                    raw.msg_type
                );
            }
            None => {
                // The header line has no `type` but carries `sessionId`+`startTime`.
                if raw.session_id.is_some() {
                    if header_start.is_none() {
                        header_start = parse_ts(&raw.start_time);
                    }
                } else {
                    session.skipped_lines += 1;
                    tracing::debug!(
                        "{}:{}: unrecognized line shape",
                        path_for_logs.display(),
                        lineno + 1
                    );
                }
            }
        }
    }

    session.started_at = header_start.or_else(|| session.events.iter().filter_map(|e| e.ts).min());
    session.ended_at = session.events.iter().filter_map(|e| e.ts).max();
    session.model = model_counts
        .into_iter()
        .max_by_key(|(_, count)| *count)
        .map(|(model, _)| model);
    session
}

/// Apply a `gemini` (assistant) line: create its Assistant event, or update the
/// existing one when this `id` was already seen (whole-message re-serialization).
fn apply_gemini(
    raw: &RawLine,
    ts: Option<jiff::Timestamp>,
    session: &mut Session,
    gemini_idx: &mut HashMap<String, usize>,
    model_counts: &mut HashMap<String, u64>,
) {
    let usage = raw.tokens.as_ref().map(map_tokens);
    let content = raw.content.as_str().unwrap_or_default();
    let visible_chars = content.chars().count() as u64;
    let thinking_chars = raw
        .thoughts
        .iter()
        .flatten()
        .map(RawThought::chars)
        .sum::<u64>();
    let has_thinking = raw.thoughts.as_ref().is_some_and(|t| !t.is_empty())
        || raw.tokens.as_ref().and_then(|t| t.thoughts).unwrap_or(0) > 0;
    let tool_calls: Vec<ToolCall> = raw
        .tool_calls
        .iter()
        .flatten()
        .map(RawToolCall::to_tool_call)
        .collect();
    // `content_chars` follows the model contract (visible text + thinking +
    // tool-call JSON, with `thinking_chars` a SUBSET), exactly as the Claude Code /
    // Copilot adapters build it — so `analysis::context` can recover the visible-text
    // share by `content_chars − thinking_chars − tool-call bytes`. Counting only the
    // visible answer here (as a prior version did) made that subtraction underflow to
    // 0 whenever thoughts/tool JSON outweighed the answer, dropping Gemini assistant
    // text from the context breakdown entirely.
    let content_chars =
        visible_chars + thinking_chars + tool_calls.iter().map(|c| c.input_bytes).sum::<u64>();

    let key = raw.id.clone().unwrap_or_default();
    if let Some(&idx) = gemini_idx.get(&key) {
        // Re-serialized superset of an earlier line: max-merge usage (never lose
        // tokens), last-wins content / thoughts / tool calls (they grow).
        let ev = &mut session.events[idx];
        ev.usage = match (ev.usage, usage) {
            (Some(a), Some(b)) => Some(a.merge_max(b)),
            (a, b) => a.or(b),
        };
        ev.thinking_chars = thinking_chars;
        ev.has_thinking |= has_thinking;
        if let Some(s) = snippet(content) {
            ev.content_summary = Some(s);
        }
        if !tool_calls.is_empty() {
            ev.tool_calls = tool_calls;
        }
        // Recompute against the FINAL tool set — when this line re-sent no tool calls
        // the earlier set is kept, so basing the byte share on `ev.tool_calls` keeps
        // `content_chars` consistent with the `tool_calls` that downstream subtracts.
        ev.content_chars = visible_chars
            + thinking_chars
            + ev.tool_calls.iter().map(|c| c.input_bytes).sum::<u64>();
        return;
    }

    if let Some(model) = raw.model.as_deref().filter(|m| !m.is_empty()) {
        *model_counts.entry(model.to_string()).or_default() += 1;
    }
    gemini_idx.insert(key.clone(), session.events.len());
    session.events.push(Event {
        kind: EventKind::Assistant,
        ts,
        // The message id is the dedup key (see module docs); a unique id per
        // request means downstream dedup is a no-op rather than a wrong merge.
        request_id: (!key.is_empty()).then_some(key),
        model: raw.model.clone().filter(|m| !m.is_empty()),
        usage,
        tool_calls,
        sidechain: false,
        content_summary: snippet(content),
        content_chars,
        thinking_chars,
        has_thinking,
        tool_use_id: None,
        attachment_kind: None,
        item_count: 0,
    });
}

/// Apply a `user` line: plain text → a User event; a `functionResponse` block → a
/// ToolResult event linked by `tool_use_id`. Neither carries billable usage.
fn apply_user(raw: &RawLine, ts: Option<jiff::Timestamp>, session: &mut Session) {
    let blocks = match raw.content.as_array() {
        Some(b) => b,
        None => return,
    };
    // A tool result (`functionResponse`) — link it back to the call by id.
    if let Some(fr) = blocks.iter().find_map(|b| b.get("functionResponse")) {
        let tool_use_id = fr
            .get("id")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .filter(|s| !s.is_empty());
        let output = fr
            .get("response")
            .and_then(|r| r.get("output"))
            .map(value_text)
            .unwrap_or_default();
        session.events.push(Event {
            kind: EventKind::ToolResult,
            ts,
            request_id: None,
            model: None,
            usage: None,
            tool_calls: Vec::new(),
            sidechain: false,
            content_summary: snippet(&output),
            content_chars: output.chars().count() as u64,
            thinking_chars: 0,
            has_thinking: false,
            tool_use_id,
            attachment_kind: None,
            item_count: 0,
        });
        return;
    }
    // Otherwise a normal user prompt.
    let text = blocks
        .iter()
        .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
        .collect::<Vec<_>>()
        .join("\n");
    session.events.push(Event {
        kind: EventKind::User,
        ts,
        request_id: None,
        model: None,
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

/// Map a Gemini `tokens` block DISJOINTLY into [`Usage`] so `known_total()` equals
/// its `total` (see module docs): `cached ⊆ input`, `thoughts` disjoint from
/// `output`, `total == input + output + thoughts`.
///   - `cache_read = cached`
///   - `input      = input − cached`     (saturating)
///   - `output     = output`             (already visible-only)
///   - `thinking   = thoughts`
///
/// `saturating_sub` guards a schema-violating `cached > input` so we never panic
/// on bad data (CLAUDE.md §9). `tool` is observed 0 and not part of `total`, so it
/// is intentionally unmapped.
fn map_tokens(t: &RawTokens) -> Usage {
    let cached = t.cached.unwrap_or(0);
    Usage {
        input: t.input.map(|i| i.saturating_sub(cached)),
        output: t.output,
        cache_creation: None,
        cache_creation_5m: None,
        cache_creation_1h: None,
        cache_read: t.cached,
        thinking: t.thoughts,
    }
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

/// Flatten a JSON value to plain text for size measurement (string as-is, else its
/// compact serialization).
fn value_text(v: &serde_json::Value) -> String {
    match v.as_str() {
        Some(s) => s.to_string(),
        None => v.to_string(),
    }
}

// ---- raw line shapes (lenient: unknown fields ignored everywhere) ----

#[derive(Deserialize, Default)]
struct RawLine {
    /// Presence marks a `$set` UI/state snapshot line (ignored).
    #[serde(rename = "$set")]
    set: Option<serde::de::IgnoredAny>,
    /// Header lines carry `sessionId` (+ `startTime`) and no `type`.
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
    #[serde(rename = "startTime")]
    start_time: Option<String>,
    /// Message discriminator: `user` / `gemini` / `info` / `error`.
    #[serde(rename = "type")]
    msg_type: Option<String>,
    /// Message id — the dedup key for `gemini` messages.
    id: Option<String>,
    timestamp: Option<String>,
    /// `gemini` → string; `user` → `[{text}]` or `[{functionResponse}]`.
    #[serde(default)]
    content: serde_json::Value,
    /// `gemini.thoughts` — plaintext reasoning blocks.
    thoughts: Option<Vec<RawThought>>,
    /// `gemini.tokens` — the only place usage lives.
    tokens: Option<RawTokens>,
    /// `gemini.model` (e.g. `gemini-3-flash-preview`).
    model: Option<String>,
    /// `gemini.toolCalls`.
    #[serde(rename = "toolCalls")]
    tool_calls: Option<Vec<RawToolCall>>,
}

/// A Gemini per-message token block. `total == input + output + thoughts`;
/// `cached ⊆ input`; `tool` observed 0 (see [`map_tokens`]).
#[derive(Deserialize, Clone, Copy)]
struct RawTokens {
    input: Option<u64>,
    output: Option<u64>,
    cached: Option<u64>,
    thoughts: Option<u64>,
    #[allow(dead_code)]
    tool: Option<u64>,
    #[allow(dead_code)]
    total: Option<u64>,
}

#[derive(Deserialize)]
struct RawThought {
    subject: Option<String>,
    description: Option<String>,
}

impl RawThought {
    /// Measurable thinking chars = subject + description (Gemini thoughts are
    /// plaintext, so unlike Claude this is a real measurement, not a lower bound).
    fn chars(&self) -> u64 {
        let s = self.subject.as_deref().map_or(0, |x| x.chars().count());
        let d = self.description.as_deref().map_or(0, |x| x.chars().count());
        (s + d) as u64
    }
}

#[derive(Deserialize)]
struct RawToolCall {
    id: Option<String>,
    name: Option<String>,
    args: Option<serde_json::Value>,
}

impl RawToolCall {
    fn to_tool_call(&self) -> ToolCall {
        let name = self.name.clone().unwrap_or_else(|| "unknown".to_string());
        let input_bytes = self
            .args
            .as_ref()
            .map_or(0, |a| value_text(a).chars().count() as u64);
        ToolCall {
            server: ToolCall::server_from_name(&name),
            id: self.id.clone().unwrap_or_default(),
            name,
            input_bytes,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// The reconciliation core: a `tokens` block maps DISJOINTLY so its
    /// `known_total()` equals `total` (no double-count of cached / thoughts).
    #[test]
    fn tokens_map_disjointly_to_total() {
        let t = RawTokens {
            input: Some(1000),
            output: Some(50),
            cached: Some(200),
            thoughts: Some(80),
            tool: Some(0),
            total: Some(1130),
        };
        let u = map_tokens(&t);
        assert_eq!(u.cache_read, Some(200));
        assert_eq!(u.input, Some(800), "input minus cached subset");
        assert_eq!(u.output, Some(50), "output is already visible-only");
        assert_eq!(u.thinking, Some(80), "thoughts disjoint from output");
        assert_eq!(u.cache_creation, None);
        assert_eq!(
            u.known_total(),
            1130,
            "Σ disjoint fields == total (input+output+thoughts)"
        );
    }

    /// `cached > input` (schema-violating) must saturate, never panic.
    #[test]
    fn underflow_saturates_instead_of_panicking() {
        let t = RawTokens {
            input: Some(100),
            output: Some(40),
            cached: Some(250),
            thoughts: Some(10),
            tool: None,
            total: Some(150),
        };
        let u = map_tokens(&t);
        assert_eq!(u.input, Some(0));
        assert_eq!(u.cache_read, Some(250));
    }

    /// Whole-message re-serialization: the SAME `id` on two lines collapses to one
    /// Assistant event (max usage, last-wins tool calls), not two requests.
    #[test]
    fn repeated_gemini_id_collapses_last_wins() {
        let lines = [
            r#"{"sessionId":"s1","startTime":"2026-06-17T10:00:00.000Z","kind":"main"}"#,
            r#"{"id":"g1","timestamp":"2026-06-17T10:00:02.000Z","type":"gemini","content":"","thoughts":[{"subject":"Plan","description":"do it"}],"tokens":{"input":1000,"output":50,"cached":200,"thoughts":80,"tool":0,"total":1130},"model":"gemini-3-flash-preview"}"#,
            r#"{"id":"g1","timestamp":"2026-06-17T10:00:02.500Z","type":"gemini","content":"done","thoughts":[{"subject":"Plan","description":"do it"}],"tokens":{"input":1000,"output":50,"cached":200,"thoughts":80,"tool":0,"total":1130},"model":"gemini-3-flash-preview","toolCalls":[{"id":"tc1","name":"mcp__acme-db__query","args":{"sql":"select 1"}}]}"#,
        ]
        .join("\n");
        let s = parse_reader(
            Cursor::new(lines),
            "s1".into(),
            Some("demo".into()),
            "gemini",
            Path::new("mem://test"),
        );
        let assistants: Vec<_> = s
            .events
            .iter()
            .filter(|e| e.kind == EventKind::Assistant)
            .collect();
        assert_eq!(assistants.len(), 1, "two lines, one request");
        let a = assistants[0];
        assert_eq!(a.usage.unwrap().known_total(), 1130);
        // content_chars follows the model contract: visible "done" (4) + thoughts
        // "Plan"+"do it" (9) + tool-call JSON {"sql":"select 1"} (18) = 31, so
        // analysis::context recovers the 4 visible chars by subtraction.
        assert_eq!(
            a.thinking_chars, 9,
            "thoughts are a subset of content_chars"
        );
        assert_eq!(
            a.content_chars, 31,
            "visible(4) + thinking(9) + tool JSON(18)"
        );
        assert_eq!(a.tool_calls.len(), 1, "tool call from the later line");
        assert_eq!(a.tool_calls[0].server.as_deref(), Some("acme-db"));
        assert_eq!(s.model.as_deref(), Some("gemini-3-flash-preview"));
        assert_eq!(
            s.started_at.map(|t| t.to_string()).as_deref(),
            Some("2026-06-17T10:00:00Z"),
            "started_at from the header line"
        );
    }

    /// `$set` snapshots are ignored; malformed JSON and unknown types are skipped.
    #[test]
    fn set_ignored_unknown_and_malformed_skipped() {
        let lines = [
            r#"{"$set":{"messages":[{"id":"u0","type":"user","content":[{"text":"ctx"}]}]}}"#,
            r#"{"id":"u1","type":"user","content":[{"text":"hi"}]}"#,
            r#"{"id":"i1","type":"info","content":[{"text":"model set"}]}"#,
            r#"{"id":"z1","type":"telemetry_blob","data":1}"#,
            r#"{"id":"broken","type":"gemini","#,
        ]
        .join("\n");
        let s = parse_reader(
            Cursor::new(lines),
            "s2".into(),
            None,
            "gemini",
            Path::new("mem://test"),
        );
        assert_eq!(s.skipped_lines, 2, "unknown type + malformed JSON");
        // Only the real user prompt became an event ($set + info contributed none).
        assert_eq!(
            s.events
                .iter()
                .filter(|e| e.kind == EventKind::User)
                .count(),
            1
        );
    }

    /// `$GEMINI_HOME` overrides the root.
    #[test]
    fn gemini_home_overrides_root() {
        std::env::set_var("GEMINI_HOME", "/tmp/fake-gemini");
        let root = gemini_root();
        std::env::remove_var("GEMINI_HOME");
        assert_eq!(root, Some(PathBuf::from("/tmp/fake-gemini")));
    }
}
