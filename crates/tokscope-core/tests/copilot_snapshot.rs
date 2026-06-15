//! Snapshot test for the Copilot CLI adapter against the synthetic fixture.
//! Any schema-handling regression shows up as a snapshot diff.
//!
//! The load-bearing honesty guarantee is checked explicitly before the snapshot:
//! Copilot logs no per-request token usage, so EVERY event's `usage` is `None`
//! (UNKNOWN, not zero — CLAUDE.md §8.5).

use std::path::{Path, PathBuf};

use tokscope_core::adapters::{copilot::Copilot, Adapter};
use tokscope_core::model::{EventKind, SessionRef};

fn fixture_ref(rel: &str) -> SessionRef {
    let path: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/copilot/session-state")
        .join(rel);
    assert!(path.is_file(), "fixture missing: {}", path.display());
    SessionRef {
        path,
        agent: "copilot".into(),
        project: None,
        size_bytes: 0,
        modified: None,
    }
}

#[test]
fn parses_events_fixture() {
    let r = fixture_ref("11111111-2222-4333-8444-555555555555/events.jsonl");
    let session = Copilot.parse(&r).expect("fixture parses");

    // Lenient skipping: 1 unknown-type line + 1 corrupt line.
    assert_eq!(session.skipped_lines, 2, "1 unknown-type + 1 corrupt line");

    // Session id is the parent dir uuid (nested `events.jsonl` layout).
    assert_eq!(session.id, "11111111-2222-4333-8444-555555555555");
    // Project = cwd basename from `session.start`.
    assert_eq!(session.project.as_deref(), Some("synthetic-app"));
    // Most-frequent model: `gpt-5` is tallied twice (selectedModel + the
    // switch-back model_change) vs `gpt-5-mini` once, so it wins cleanly.
    assert_eq!(session.model.as_deref(), Some("gpt-5"));
    // Copilot has no sub-agent transcripts.
    assert!(session.sub_agents.is_empty());
    assert_eq!(session.parent_session, None);

    // THE honesty guarantee: usage is unknown everywhere (§8.5).
    assert!(
        session.events.iter().all(|e| e.usage.is_none()),
        "Copilot records no per-request usage; every event's usage must be None"
    );

    // Tool call/result linkage by tool_call_id: the assistant requests `call_0001`
    // and a later ToolResult carries the same id in its summary.
    let has_request = session
        .events
        .iter()
        .filter(|e| e.kind == EventKind::Assistant)
        .any(|e| e.tool_calls.iter().any(|t| t.id == "call_0001"));
    assert!(has_request, "assistant must emit the tool call");
    let result = session
        .events
        .iter()
        .find(|e| e.kind == EventKind::ToolResult)
        .expect("a tool result event");
    assert_eq!(
        result.content_summary.as_deref(),
        Some("bash"),
        "result links back to its call (toolName/toolCallId)"
    );

    // Reasoning presence surfaced on the first assistant message.
    let first_assistant = session
        .events
        .iter()
        .find(|e| e.kind == EventKind::Assistant)
        .expect("an assistant event");
    assert!(
        first_assistant.has_thinking,
        "reasoningText sets has_thinking"
    );

    insta::assert_json_snapshot!("events_session", session);
}
