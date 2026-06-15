//! End-to-end CLI tests for the Copilot adapter against the synthetic fixture.
//!
//! `COPILOT_HOME` points the adapter at `fixtures/copilot` (its `~/.copilot`
//! equivalent), so these run the real discover -> parse -> aggregate -> render
//! pipeline. The point of the adapter is STRUCTURE without billing: token totals
//! are unknown/zero while `by_tool` is populated — and the run must not panic on
//! the absent usage (CLAUDE.md §8.5).

use std::path::{Path, PathBuf};

use assert_cmd::Command;
use predicates::prelude::*;

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/copilot")
}

fn tokscope() -> Command {
    let mut cmd = Command::cargo_bin("tokscope").expect("binary builds");
    cmd.env("COPILOT_HOME", fixtures_dir());
    cmd
}

#[test]
fn copilot_summary_runs_with_zero_unknown_usage_and_populated_tools() {
    let output = tokscope()
        .args(["summary", "--json", "--agent", "copilot"])
        .output()
        .expect("runs");
    assert!(
        output.status.success(),
        "copilot summary must succeed, not panic on missing usage"
    );
    let v: serde_json::Value = serde_json::from_slice(&output.stdout).expect("valid JSON");

    // The fixture session parsed.
    assert_eq!(v["sessions_parsed"], 1);
    assert_eq!(v["skipped_lines"], 2, "1 unknown-type + 1 corrupt line");

    // No per-request usage anywhere => totals are zero/unknown (NOT a crash).
    assert_eq!(v["totals"]["input"], 0);
    assert_eq!(v["totals"]["output"], 0);
    assert_eq!(v["totals"]["cache_creation"], 0);
    assert_eq!(v["totals"]["cache_read"], 0);
    let cost = v["totals"]["cost_usd"].as_f64().expect("float");
    assert!(cost.abs() < 1e-12, "no usage => no priced cost, got {cost}");

    // Structure IS captured: the `bash` tool call shows up in by_tool even though
    // it carries no usage.
    assert_eq!(
        v["by_tool"]["bash"]["calls"], 1,
        "the assistant's bash toolRequest must be attributed"
    );
    assert_eq!(
        v["by_tool"]["bash"]["server"],
        serde_json::Value::Null,
        "a plain (non-MCP) tool has no server"
    );

    // The honest consequence of zero billable usage: the session contributes no
    // priced `by_session` row (the aggregator filters rows with 0 requests). We do
    // NOT fabricate spend to make it appear — structure lives in `by_tool`, the
    // parsed model lives in the adapter-level snapshot test.
    assert!(
        v["by_session"].as_array().expect("array").is_empty(),
        "no usage => no priced session row; spend stays unknown, not invented"
    );
}

#[test]
fn copilot_agent_is_no_longer_a_stub() {
    // `--agent copilot` must route to the real adapter, not bail "not yet
    // implemented".
    tokscope()
        .args(["summary", "--agent", "copilot"])
        .assert()
        .success()
        .stdout(predicate::str::contains("not yet implemented").not());
}
