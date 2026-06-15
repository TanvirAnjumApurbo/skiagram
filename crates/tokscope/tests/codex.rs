//! End-to-end CLI tests for the Codex adapter against the synthetic fixtures in
//! `fixtures/codex`.
//!
//! `CODEX_HOME` points the adapter at the fixtures, so these run the real
//! discover -> parse -> dedup -> aggregate -> render pipeline. Numbers below are
//! hand-derived from the two fixture sessions.

use std::path::{Path, PathBuf};

use assert_cmd::Command;
use predicates::prelude::*;

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/codex")
}

fn tokscope() -> Command {
    let mut cmd = Command::cargo_bin("tokscope").expect("binary builds");
    cmd.env("CODEX_HOME", fixtures_dir());
    cmd
}

/// The stub used to `bail!("not yet implemented")`; the real adapter must not.
#[test]
fn codex_agent_is_implemented_now() {
    let output = tokscope()
        .args(["summary", "--json", "--agent", "codex"])
        .output()
        .expect("runs");
    assert!(
        output.status.success(),
        "codex summary must succeed now: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("not yet implemented"),
        "codex adapter must no longer report 'not yet implemented'"
    );
}

#[test]
fn summary_json_has_exact_cumulative_totals() {
    let output = tokscope()
        .args(["summary", "--json", "--agent", "codex"])
        .output()
        .expect("runs");
    assert!(output.status.success());
    let v: serde_json::Value = serde_json::from_slice(&output.stdout).expect("valid JSON");

    // Two fixture sessions parsed (main + archived), with the corrupt + unknown
    // lines skipped leniently in the main session.
    assert_eq!(v["sessions_parsed"], 2);
    assert_eq!(v["skipped_lines"], 2);

    // Cumulative totals: each token_count's per-request delta mapped DISJOINTLY,
    // so summing them reconstructs Codex's cumulative spend (no ~100x overcount).
    //   main session deltas: (1000/0/200/50) + (2000/500/300/100) + (3000/1000/500/0)
    //   archived:            (800/200/100/0)
    // mapped to Usage {input = in-cached, cache_read = cached, output = out-reason,
    // thinking = reason}:
    //   input        = (1000+1500+2000) + 600  = 5100
    //   cache_read   = (0+500+1000)      + 200  = 1700
    //   output       = (150+200+500)     + 100  = 950
    //   thinking is tracked on the request but Rollup.total_tokens excludes it.
    assert_eq!(
        v["totals"]["requests"], 4,
        "3 main token_counts + 1 archived"
    );
    assert_eq!(v["totals"]["input"], 5100);
    assert_eq!(v["totals"]["output"], 950);
    assert_eq!(v["totals"]["cache_creation"], 0);
    assert_eq!(v["totals"]["cache_read"], 1700);

    // Codex never has a request_id on these events, so dedup never merges them:
    // every per-request delta survives as its own request.
    assert_eq!(v["dedup"]["duplicate_lines_collapsed"], 0);
    // The naive sum here EQUALS the deduped sum precisely because we emit
    // per-request deltas, not the cumulative totals — the overcount is avoided in
    // the adapter, before dedup ever runs.
    assert_eq!(
        v["dedup"]["naive_known_tokens"], 7900,
        "incl. thinking subset"
    );
    assert_eq!(v["dedup"]["requests_with_thinking"], 2);

    // context_compacted → one compaction.
    assert_eq!(v["compactions"], 1);

    // gpt-* models are NOT in the embedded snapshot → unpriced, never guessed.
    let unpriced = v["unpriced_models"].as_array().expect("array");
    assert!(
        unpriced.iter().any(|m| m == "gpt-5.5"),
        "gpt-5.5 must be surfaced as unpriced: {unpriced:?}"
    );
    assert!(
        unpriced.iter().any(|m| m == "gpt-5.5-codex"),
        "gpt-5.5-codex must be surfaced as unpriced: {unpriced:?}"
    );
    assert_eq!(
        v["totals"]["unpriced_requests"], 4,
        "all 4 requests on unpriced gpt models"
    );
    assert_eq!(
        v["totals"]["cost_usd"].as_f64().expect("float"),
        0.0,
        "no priced spend — cost is honestly zero, not guessed"
    );

    // MCP-server attribution survives: the mcp__acme-db__query call is bucketed
    // under its server, both via the response_item function_call and the
    // mcp_tool_call_end event.
    assert_eq!(
        v["by_tool"]["mcp__acme-db__query"]["server"], "acme-db",
        "MCP server parsed from the tool name"
    );

    // Models surfaced per-model.
    assert!(v["by_model"].get("gpt-5.5").is_some());
    assert!(v["by_model"].get("gpt-5.5-codex").is_some());
}

#[test]
fn summary_table_renders_unpriced_gpt_models() {
    tokscope()
        .args(["summary", "--agent", "codex"])
        .assert()
        .success()
        // The model shows up in the human-readable table...
        .stdout(predicate::str::contains("gpt-5.5"))
        // ...and the unpriced-models notice names it (cost not guessed, §8.7).
        .stdout(predicate::str::contains("unpriced models"));
}
