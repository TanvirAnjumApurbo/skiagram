//! Snapshot test for the Gemini CLI adapter against the synthetic fixture.
//! Any schema-handling regression shows up as a snapshot diff.

use std::path::{Path, PathBuf};

use tokscope_core::adapters::{gemini::Gemini, Adapter};
use tokscope_core::model::{EventKind, SessionRef};

fn fixture_ref(rel: &str) -> SessionRef {
    let path: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/gemini")
        .join(rel);
    assert!(path.is_file(), "fixture missing: {}", path.display());
    SessionRef {
        path,
        agent: "gemini".into(),
        project: None,
        size_bytes: 0,
        modified: None,
    }
}

#[test]
fn parses_main_session_fixture() {
    let r = fixture_ref("tmp/demo-project/chats/session-2026-06-17T10-00-00-abcd1234.jsonl");
    let session = Gemini.parse(&r).expect("fixture parses");

    // Lenient skipping (unknown type + malformed JSON), model + project surfaced.
    assert_eq!(session.skipped_lines, 2, "telemetry_blob + broken JSON");
    assert_eq!(session.model.as_deref(), Some("gemini-3-flash-preview"));
    assert_eq!(
        session.project.as_deref(),
        Some("demo-project"),
        "friendly project name from the tmp/<project>/chats path"
    );

    // Whole-message re-serialization collapses by id: g1 appears on two lines but
    // is ONE Assistant request; g2 is a second. Two usage-bearing requests total.
    let usage_events: Vec<_> = session
        .events
        .iter()
        .filter(|e| e.kind == EventKind::Assistant && e.usage.is_some())
        .collect();
    assert_eq!(usage_events.len(), 2, "g1 (deduped) + g2");

    // Reconciliation (§8.2 Codex analog): each request's disjoint mapping sums to
    // its `total` (input+output+thoughts); g1=1130, g2=1230 → 2360.
    let total: u64 = usage_events
        .iter()
        .filter_map(|e| e.usage)
        .map(|u| u.known_total())
        .sum();
    assert_eq!(total, 2360, "Σ per-request known_total");

    // Thinking is plaintext-measured (not encrypted) on g1 only.
    assert_eq!(session.events.iter().filter(|e| e.has_thinking).count(), 1);

    insta::assert_json_snapshot!("main_session", session);
}
