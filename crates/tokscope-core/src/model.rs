//! Agent-agnostic domain model (CLAUDE.md §6).
//!
//! Everything here is what adapters normalize INTO; nothing here knows about any
//! specific agent's on-disk format.

use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Lightweight pointer to one session file on disk, produced by `Adapter::discover`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionRef {
    pub path: PathBuf,
    /// Adapter id, e.g. `"claude-code"`.
    pub agent: String,
    /// Project label as the agent records it (for Claude Code: the URL-encoded
    /// cwd directory name, e.g. `F--tokscope`).
    pub project: Option<String>,
    pub size_bytes: u64,
    pub modified: Option<Timestamp>,
}

/// One normalized agent conversation/run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub agent: String,
    pub project: Option<String>,
    /// Most frequently seen (non-synthetic) assistant model, if any.
    pub model: Option<String>,
    /// For sub-agent transcripts (e.g. Claude Code's
    /// `<project>/<parent-session-uuid>/subagents/agent-*.jsonl`): the parent
    /// session id this spend is attributed to (CLAUDE.md §8.3).
    pub parent_session: Option<String>,
    pub started_at: Option<Timestamp>,
    pub ended_at: Option<Timestamp>,
    pub events: Vec<Event>,
    /// Sub-agents spawned BY this session (`Task`/`Agent` tool calls).
    pub sub_agents: Vec<SubAgent>,
    /// Lines that failed to parse or had an unrecognized shape. Skipped leniently,
    /// surfaced so format drift is visible instead of silent.
    pub skipped_lines: u64,
}

/// What one normalized event represents.
///
/// Note: some agents (Claude Code) attach tool calls to `Assistant` events rather
/// than emitting standalone `ToolCall` events; the variant exists for agents that
/// log them separately.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EventKind {
    User,
    Assistant,
    ToolCall,
    ToolResult,
    System,
    Compaction,
    SubAgentSpawn,
    /// Non-message context injected into the window: deferred-tool listings,
    /// skill listings, MCP-server instruction blocks, IDE/file context, reminders
    /// (Claude Code `attachment` lines). Carries no token usage; used by
    /// context-bloat attribution (`analysis::context`).
    Attachment,
}

/// One line/turn of a session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub kind: EventKind,
    pub ts: Option<Timestamp>,
    /// API request id. SEVERAL events may share one request id (one line per
    /// content block) — accounting MUST dedup on it first (CLAUDE.md §8.1).
    pub request_id: Option<String>,
    pub model: Option<String>,
    pub usage: Option<Usage>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    /// True when the event belongs to a sub-agent (sidechain) transcript.
    pub sidechain: bool,
    /// Short display-only snippet of visible text.
    pub content_summary: Option<String>,
    /// Approximate chars of generated content (text + thinking + tool-call JSON).
    /// Heuristic input for thinking-token reconciliation only — never billed.
    pub content_chars: u64,
    /// Chars of extended-thinking text in this event — a SUBSET of `content_chars`
    /// (0 when none, or when the thinking block was encrypted and thus
    /// unmeasurable). Lets context attribution separate thinking from text.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub thinking_chars: u64,
    /// The event carried thinking blocks (possibly encrypted/redacted, i.e. with
    /// no measurable text).
    pub has_thinking: bool,
    /// For a `ToolResult` event: the `tool_use_id` it answers. Lets the result's
    /// bytes be attributed back to the tool / MCP server that produced them
    /// (the matching `ToolCall` carries the same id in `ToolCall::id`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_use_id: Option<String>,
    /// For an `Attachment` event: its category — e.g. `"deferred_tools_delta"`,
    /// `"skill_listing"`, `"mcp_instructions_delta"`, `"task_reminder"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attachment_kind: Option<String>,
    /// For an `Attachment` event: how many named items it added to the window
    /// (deferred tools, skills, MCP servers). 0 elsewhere.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub item_count: u64,
}

/// `serde(skip_serializing_if)` predicate: keeps zero counters out of JSON/snapshots.
fn is_zero(n: &u64) -> bool {
    *n == 0
}

/// Token usage as reported by the agent.
///
/// Every field is `Option`: `None` means UNKNOWN, which is not the same as zero
/// (CLAUDE.md §8.5). Renderers must show unknowns as such.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub input: Option<u64>,
    pub output: Option<u64>,
    /// Total cache-creation (write) tokens.
    pub cache_creation: Option<u64>,
    /// 5-minute-TTL share of `cache_creation`, when the agent reports the split.
    /// Priced differently from the 1h share (see `pricing`).
    pub cache_creation_5m: Option<u64>,
    /// 1-hour-TTL share of `cache_creation` (priced 2x input vs 1.25x for 5m).
    pub cache_creation_1h: Option<u64>,
    pub cache_read: Option<u64>,
    /// Extended-thinking tokens. Claude Code does not report these separately —
    /// stays `None` there; the dedup pass flags likely undercounts instead
    /// (CLAUDE.md §8.2).
    pub thinking: Option<u64>,
}

impl Usage {
    /// True when no field carries any information.
    pub fn is_empty(&self) -> bool {
        self.input.is_none()
            && self.output.is_none()
            && self.cache_creation.is_none()
            && self.cache_read.is_none()
            && self.thinking.is_none()
    }

    /// Sum of all KNOWN token counts (the 5m/1h fields are a breakdown of
    /// `cache_creation` and are not added again).
    pub fn known_total(&self) -> u64 {
        self.input.unwrap_or(0)
            + self.output.unwrap_or(0)
            + self.cache_creation.unwrap_or(0)
            + self.cache_read.unwrap_or(0)
            + self.thinking.unwrap_or(0)
    }

    /// Field-wise maximum, treating `None` as "no information" (not zero).
    ///
    /// This is the dedup merge rule: lines of one request either repeat identical
    /// usage or grow monotonically while streaming, so MAX recovers the final
    /// per-request value in both cases.
    pub fn merge_max(self, other: Usage) -> Usage {
        fn max_opt(a: Option<u64>, b: Option<u64>) -> Option<u64> {
            match (a, b) {
                (Some(x), Some(y)) => Some(x.max(y)),
                (x, None) => x,
                (None, y) => y,
            }
        }
        Usage {
            input: max_opt(self.input, other.input),
            output: max_opt(self.output, other.output),
            cache_creation: max_opt(self.cache_creation, other.cache_creation),
            cache_creation_5m: max_opt(self.cache_creation_5m, other.cache_creation_5m),
            cache_creation_1h: max_opt(self.cache_creation_1h, other.cache_creation_1h),
            cache_read: max_opt(self.cache_read, other.cache_read),
            thinking: max_opt(self.thinking, other.thinking),
        }
    }
}

/// One tool invocation by the assistant.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    /// Serialized size of the tool input (a proxy for the output tokens the call
    /// itself cost; the *result* size lands on the matching `ToolResult` event).
    pub input_bytes: u64,
    /// MCP server the tool belongs to, parsed from `mcp__<server>__<tool>` names.
    pub server: Option<String>,
}

impl ToolCall {
    /// `mcp__github__search_issues` -> `Some("github")`; plain tools -> `None`.
    pub fn server_from_name(name: &str) -> Option<String> {
        let rest = name.strip_prefix("mcp__")?;
        rest.split("__").next().map(str::to_string)
    }
}

/// A sub-agent spawned via the `Task` (legacy) / `Agent` (current) tool.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SubAgent {
    /// The `tool_use` id of the spawning call (its `ToolResult` carries the
    /// sub-agent's report).
    pub tool_call_id: String,
    pub agent_type: Option<String>,
    pub description: Option<String>,
    pub ts: Option<Timestamp>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_max_treats_none_as_unknown() {
        let a = Usage {
            input: Some(100),
            output: None,
            ..Usage::default()
        };
        let b = Usage {
            input: Some(40),
            output: Some(7),
            ..Usage::default()
        };
        let m = a.merge_max(b);
        assert_eq!(m.input, Some(100));
        assert_eq!(m.output, Some(7));
        assert_eq!(m.cache_read, None, "unknown stays unknown, not zero");
    }

    #[test]
    fn mcp_server_is_parsed_from_tool_name() {
        assert_eq!(
            ToolCall::server_from_name("mcp__github__search_issues").as_deref(),
            Some("github")
        );
        assert_eq!(ToolCall::server_from_name("Read"), None);
    }
}
