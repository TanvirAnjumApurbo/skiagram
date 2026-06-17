//! End-to-end CLI tests for the Gemini adapter against the synthetic fixture in
//! `fixtures/gemini`.
//!
//! `GEMINI_HOME` points the adapter at the fixtures, so these run the real
//! discover -> parse -> dedup -> aggregate -> render pipeline. Numbers below are
//! hand-derived from the one fixture session (two requests: g1 deduped from two
//! re-serialized lines, plus g2).

use std::path::{Path, PathBuf};

use assert_cmd::Command;
use predicates::prelude::*;

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/gemini")
}

fn skiagram() -> Command {
    let mut cmd = Command::cargo_bin("skiagram").expect("binary builds");
    cmd.env("GEMINI_HOME", fixtures_dir());
    cmd
}

/// The stub used to `bail!("not yet implemented")`; the real adapter must not.
#[test]
fn gemini_agent_is_implemented_now() {
    let output = skiagram()
        .args(["summary", "--json", "--agent", "gemini"])
        .output()
        .expect("runs");
    assert!(
        output.status.success(),
        "gemini summary must succeed now: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !String::from_utf8_lossy(&output.stderr).contains("not yet implemented"),
        "gemini adapter must no longer report 'not yet implemented'"
    );
}

#[test]
fn summary_json_has_exact_deduplicated_numbers() {
    let output = skiagram()
        .args(["summary", "--json", "--agent", "gemini"])
        .output()
        .expect("runs");
    assert!(output.status.success());
    let v: serde_json::Value = serde_json::from_slice(&output.stdout).expect("valid JSON");

    // One fixture session; the unknown-type + malformed lines skipped leniently.
    assert_eq!(v["sessions_parsed"], 1);
    assert_eq!(v["skipped_lines"], 2);

    // Two deduplicated requests (g1's two re-serialized lines collapse to one).
    //   g1 tokens {input:1000, cached:200, output:50, thoughts:80} ->
    //      input 800, cache_read 200, output 50, thinking 80
    //   g2 tokens {input:1200, cached:1100, output:30, thoughts:0} ->
    //      input 100, cache_read 1100, output 30, thinking 0
    assert_eq!(v["totals"]["requests"], 2);
    assert_eq!(v["totals"]["input"], 900);
    assert_eq!(v["totals"]["output"], 80);
    assert_eq!(v["totals"]["cache_read"], 1300);
    assert_eq!(v["totals"]["cache_creation"], 0);
    // Gemini reports no cache-creation, so that field is UNKNOWN (None) on every
    // request — honestly marked incomplete, not silently zeroed (§8.5).
    assert_eq!(v["totals"]["incomplete_requests"], 2);

    // gemini-* isn't in the embedded snapshot → unpriced, never guessed (§8.7).
    assert_eq!(v["totals"]["unpriced_requests"], 2);
    assert_eq!(v["totals"]["cost_usd"].as_f64().expect("float"), 0.0);
    let unpriced = v["unpriced_models"].as_array().expect("array");
    assert!(
        unpriced.iter().any(|m| m == "gemini-3-flash-preview"),
        "gemini-3-flash-preview must be surfaced as unpriced: {unpriced:?}"
    );

    // Dedup proof-of-work: the adapter already collapsed g1's re-serialization by
    // message id, so the requestId layer sees one line per request (no further
    // collapse). naive == deduped here, and includes the thinking subset.
    assert_eq!(v["dedup"]["duplicate_lines_collapsed"], 0);
    assert_eq!(v["dedup"]["naive_known_tokens"], 2360);

    // Thinking ATTRIBUTION: Gemini `thoughts` are plaintext, so measurable. g1 has
    // 37 chars ("Planning" + "Outline the summary approach."); none encrypted.
    assert_eq!(v["dedup"]["requests_with_thinking"], 1);
    assert_eq!(v["dedup"]["requests_with_encrypted_thinking"], 0);
    assert_eq!(v["dedup"]["thinking_chars_total"], 37);

    // MCP-server attribution survives via the gemini message's toolCalls.
    assert_eq!(v["by_tool"]["mcp__acme-db__query"]["calls"], 1);
    assert_eq!(v["by_tool"]["mcp__acme-db__query"]["server"], "acme-db");

    // Model surfaced per-model.
    assert!(v["by_model"].get("gemini-3-flash-preview").is_some());
}

#[test]
fn summary_table_renders_unpriced_gemini_model() {
    skiagram()
        .args(["summary", "--agent", "gemini"])
        .assert()
        .success()
        .stdout(predicate::str::contains("gemini-3-flash-preview"))
        .stdout(predicate::str::contains("unpriced models"));
}
