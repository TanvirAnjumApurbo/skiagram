//! Snapshot tests for the Codex CLI adapter against the synthetic fixtures.
//! Any schema-handling regression shows up as a snapshot diff.

use std::path::{Path, PathBuf};

use tokscope_core::adapters::{codex::Codex, Adapter};
use tokscope_core::model::{EventKind, SessionRef};

fn fixture_ref(rel: &str) -> SessionRef {
    let path: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/codex")
        .join(rel);
    assert!(path.is_file(), "fixture missing: {}", path.display());
    SessionRef {
        path,
        agent: "codex".into(),
        project: None,
        size_bytes: 0,
        modified: None,
    }
}

#[test]
fn parses_main_session_fixture() {
    let r = fixture_ref("sessions/2026/06/15/rollout-2026-06-15T10-00-00-1f2e3d4c-5b6a-7980-abcd-ef0123456789.jsonl");
    let session = Codex.parse(&r).expect("fixture parses");

    // Hard guarantees before the snapshot: lenient skipping + model surfaced from
    // turn_context, project from session_meta cwd.
    assert_eq!(session.skipped_lines, 2, "1 corrupt + 1 unknown-type line");
    assert_eq!(session.model.as_deref(), Some("gpt-5.5"));
    assert_eq!(session.project.as_deref(), Some("/home/dev/acme-app"));

    // THE reconciliation invariant (Codex analog of §8.1): summing the per-request
    // deltas reconstructs the FINAL cumulative `total_token_usage.total_tokens`,
    // which this fixture sets to 7000. Each token_count → one Assistant event.
    let per_request: u64 = session
        .events
        .iter()
        .filter(|e| e.kind == EventKind::Assistant)
        .filter_map(|e| e.usage)
        .map(|u| u.known_total())
        .sum();
    assert_eq!(
        per_request, 7000,
        "Σ per-request known_total == final cumulative total_tokens"
    );

    // The three token_count events each become exactly one usage-bearing event.
    let usage_events = session
        .events
        .iter()
        .filter(|e| e.kind == EventKind::Assistant && e.usage.is_some())
        .count();
    assert_eq!(usage_events, 3, "one Assistant event per token_count");

    // context_compacted → Compaction.
    assert_eq!(
        session
            .events
            .iter()
            .filter(|e| e.kind == EventKind::Compaction)
            .count(),
        1
    );

    insta::assert_json_snapshot!("main_session", session);
}

#[test]
fn parses_archived_session_fixture() {
    let r = fixture_ref(
        "archived_sessions/rollout-2026-06-14T09-00-00-2a3b4c5d-6e7f-8091-bcde-f01234567890.jsonl",
    );
    let session = Codex.parse(&r).expect("fixture parses");

    assert_eq!(session.skipped_lines, 0);
    assert_eq!(session.model.as_deref(), Some("gpt-5.5-codex"));
    let per_request: u64 = session
        .events
        .iter()
        .filter(|e| e.kind == EventKind::Assistant)
        .filter_map(|e| e.usage)
        .map(|u| u.known_total())
        .sum();
    assert_eq!(
        per_request, 900,
        "single request reconciles to total_tokens"
    );

    insta::assert_json_snapshot!("archived_session", session);
}
