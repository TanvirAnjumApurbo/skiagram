//! End-to-end CLI tests against the redacted fixtures in `fixtures/claude-code`.
//!
//! `CLAUDE_CONFIG_DIR` points the adapter at the fixtures, so these run the real
//! discover -> parse -> dedup -> aggregate -> render pipeline.

use std::path::{Path, PathBuf};

use assert_cmd::Command;
use predicates::prelude::*;

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/claude-code")
}

fn tokscope() -> Command {
    let mut cmd = Command::cargo_bin("tokscope").expect("binary builds");
    cmd.env("CLAUDE_CONFIG_DIR", fixtures_dir());
    cmd
}

#[test]
fn summary_table_shows_deduplicated_totals() {
    tokscope()
        .args(["summary", "--agent", "claude-code"])
        .assert()
        .success()
        // Deduplicated grand total — NOT the naive 30,850.
        .stdout(predicate::str::contains("18,080"))
        // ...and the dedup pass proves what naive summing would have said.
        .stdout(predicate::str::contains("30,850"))
        .stdout(predicate::str::contains("requestId dedup"))
        .stdout(predicate::str::contains("claude-sonnet-4-5-20250929"))
        .stdout(predicate::str::contains("claude-haiku-4-5-20251001"))
        .stdout(predicate::str::contains("sub-agent share"));
}

#[test]
fn summary_json_has_exact_deduplicated_numbers() {
    let output = tokscope()
        .args(["summary", "--json", "--agent", "claude-code"])
        .output()
        .expect("runs");
    assert!(output.status.success());
    let v: serde_json::Value = serde_json::from_slice(&output.stdout).expect("valid JSON");

    // Grand totals (see fixtures: 3 lines share req_fixture_001 -> counted once).
    assert_eq!(v["totals"]["requests"], 6);
    assert_eq!(v["totals"]["input"], 5600);
    assert_eq!(v["totals"]["output"], 980);
    assert_eq!(v["totals"]["cache_creation"], 500);
    assert_eq!(v["totals"]["cache_read"], 11000);
    assert_eq!(v["totals"]["incomplete_requests"], 2);
    assert_eq!(v["totals"]["unpriced_requests"], 0);

    // The dedup proof-of-work.
    assert_eq!(v["dedup"]["duplicate_lines_collapsed"], 2);
    assert_eq!(v["dedup"]["naive_known_tokens"], 30850);
    assert_eq!(v["dedup"]["thinking_suspect_requests"], 1);

    // Sub-agent attribution (§8.3): child transcript folded into the parent row.
    assert_eq!(v["sidechain_totals"]["requests"], 1);
    assert_eq!(v["sidechain_totals"]["input"], 900);
    let sessions = v["by_session"].as_array().expect("array");
    assert_eq!(sessions.len(), 2, "child transcript is folded, not listed");
    assert_eq!(sessions[0]["id"], "3e9d2c41-7b5a-4f2e-9c1d-2f6b8a1c0e11");
    assert_eq!(
        sessions[0]["rollup"]["input"], 4600,
        "parent 3700 + child 900"
    );
    assert_eq!(sessions[0]["sub_agents"], 1);
    assert_eq!(sessions[0]["sub_agent_tokens"], 1300);

    // Cost: every figure traces to the embedded unit prices (§8.7):
    //   sonnet-4-5:  4600*3 + 740*15 + 300*3.75(5m) + 11000*0.30   = $0.029325
    //   haiku-4-5:   1000*1 + 240*5  + 200*2.00(1h write!)         = $0.002600
    let cost = v["totals"]["cost_usd"].as_f64().expect("float");
    assert!(
        (cost - 0.031925).abs() < 1e-9,
        "expected 0.031925, got {cost}"
    );

    // Tools, incl. MCP server attribution and the Agent spawn call.
    assert_eq!(v["by_tool"]["Read"]["calls"], 1);
    assert_eq!(v["by_tool"]["Agent"]["calls"], 1);
    assert_eq!(
        v["by_tool"]["mcp__github__search_issues"]["server"],
        "github"
    );

    // Lenient parsing surfaced, models all priced.
    assert_eq!(v["skipped_lines"], 2);
    assert_eq!(v["compactions"], 1);
    assert_eq!(v["sessions_parsed"], 3);
    assert_eq!(v["unpriced_models"].as_array().unwrap().len(), 0);
}

#[test]
fn since_filters_by_utc_date() {
    let output = tokscope()
        .args([
            "summary",
            "--json",
            "--agent",
            "claude-code",
            "--since",
            "2026-06-02",
        ])
        .output()
        .expect("runs");
    assert!(output.status.success());
    let v: serde_json::Value = serde_json::from_slice(&output.stdout).expect("valid JSON");

    // Only session B (2026-06-02) remains.
    assert_eq!(v["totals"]["requests"], 2);
    assert_eq!(v["totals"]["input"], 1000);
    assert_eq!(v["totals"]["output"], 240);
    assert_eq!(v["by_session"].as_array().unwrap().len(), 1);
    assert_eq!(v["dedup"]["duplicate_lines_collapsed"], 0);
}

#[test]
fn unknown_agent_fails_with_known_ids() {
    tokscope()
        .args(["summary", "--agent", "nope"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unknown agent id `nope`"))
        .stderr(predicate::str::contains("claude-code"));
}

#[test]
fn stub_adapters_fail_loudly_not_silently() {
    tokscope()
        .args(["summary", "--agent", "codex"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("not yet implemented"));
}
