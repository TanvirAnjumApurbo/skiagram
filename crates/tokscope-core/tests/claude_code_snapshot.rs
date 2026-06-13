//! Snapshot tests for the Claude Code adapter against the redacted fixtures.
//! Any schema-handling regression shows up as a snapshot diff.

use std::path::{Path, PathBuf};

use tokscope_core::adapters::{claude_code::ClaudeCode, Adapter};
use tokscope_core::model::SessionRef;

fn fixture_ref(rel: &str, project: &str) -> SessionRef {
    let path: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/claude-code/projects")
        .join(rel);
    assert!(path.is_file(), "fixture missing: {}", path.display());
    SessionRef {
        path,
        agent: "claude-code".into(),
        project: Some(project.into()),
        size_bytes: 0,
        modified: None,
    }
}

#[test]
fn parses_main_session_fixture() {
    let r = fixture_ref(
        "-home-dev-acme-app/3e9d2c41-7b5a-4f2e-9c1d-2f6b8a1c0e11.jsonl",
        "-home-dev-acme-app",
    );
    let session = ClaudeCode.parse(&r).expect("fixture parses");

    // Hard guarantees before the snapshot: lenient skipping + spawn mapping.
    assert_eq!(session.skipped_lines, 2, "1 corrupt + 1 unknown-type line");
    assert_eq!(session.sub_agents.len(), 1);
    assert_eq!(session.parent_session, None);

    insta::assert_json_snapshot!("main_session", session);
}

#[test]
fn parses_subagent_transcript_fixture() {
    let r = fixture_ref(
        "-home-dev-acme-app/3e9d2c41-7b5a-4f2e-9c1d-2f6b8a1c0e11/subagents/agent-fix0001.jsonl",
        "-home-dev-acme-app",
    );
    let session = ClaudeCode.parse(&r).expect("fixture parses");

    assert_eq!(
        session.parent_session.as_deref(),
        Some("3e9d2c41-7b5a-4f2e-9c1d-2f6b8a1c0e11"),
        "sub-agent spend must be attributable to the parent session (§8.3)"
    );

    insta::assert_json_snapshot!("subagent_session", session);
}
