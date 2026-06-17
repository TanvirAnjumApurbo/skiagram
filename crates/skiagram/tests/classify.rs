//! End-to-end CLI tests for `skiagram classify` against the redacted fixture in
//! `fixtures/claude-code-classify`.
//!
//! `CLAUDE_CONFIG_DIR` points the adapter at the fixture, so these run the real
//! discover -> parse -> dedup -> classify::classify -> render pipeline.
//!
//! Four top-level sessions in project `-home-dev-classify-app`, all on the PRICED
//! model `claude-sonnet-4-5-20250929` (input $3/M, output $15/M — see
//! `crates/skiagram-core/src/pricing.rs`). Each prompt is front-loaded with
//! unambiguous keywords for its intended type and the tool mix matches, so the
//! heuristic label is stable. Spend is hand-derived from dedup + pricing:
//!
//! ```text
//! DEBUGGING    d1d1d1d1…01  "fix … bug … crashes … error";  Read+Bash+Edit
//!   req_dbg0   in=1000 out=200 -> 1,200 tok  cost 0.00600
//!   req_dbg1   in=1000 out=300 -> 1,300 tok  cost 0.00750  (written twice, one
//!              requestId -> dedup field-wise MAX collapses 3 lines to 2 requests)
//!   group: 2 req, 2,500 tok, cost 0.01350
//!
//! FEATURE      f1f1f1f1…02  "implement a new feature: add … create";  Write+Edit
//!   req_feat0  in=2000 out=500 -> 2,500 tok  cost 0.01350
//!   req_feat1  in=2000 out=500 -> 2,500 tok  cost 0.01350
//!   group: 2 req, 5,000 tok, cost 0.02700
//!
//! EXPLORATION  e1e1e1e1…03  "review and explain … analyze and trace";  Grep+Glob+Read
//!   req_expl0        in=800 out=100 -> 900 tok  cost 0.00390
//!   req_expl_spawn   in=900 out=60  -> 960 tok  cost 0.00360  (Agent spawn turn)
//!   req_sa0 (child)  in=500 out=50  -> 550 tok  cost 0.00225  (sidechain, folds §8.3)
//!   group: 3 req, 2,410 tok, cost 0.00975  (one row; the child is not separate)
//!
//! UNKNOWN      a0000000…04  "ok, go ahead and continue."  (no keyword, no tools)
//!   req_unk0   in=100 out=50 -> 150 tok  cost 0.00105  (dated 2026-06-09)
//!   group: 1 req, 150 tok, cost 0.00105, confidence 0.0
//!
//! totals: 10,060 tok, cost 0.05130; by cost desc Feature > Debug > Explore > Unknown.
//! FeatureWork token_share = 5000 / 10060 = 0.49701789264413…
//! --since 2026-06-10 drops the 2026-06-09 Unknown session:
//!   sessions_classified 3, total_tokens 9,910, total_cost_usd 0.05025.
//! ```

use std::path::{Path, PathBuf};

use assert_cmd::Command;
use predicates::prelude::*;

const FEATURE_ID: &str = "f1f1f1f1-0002-4000-8000-000000000002";
const DEBUG_ID: &str = "d1d1d1d1-0001-4000-8000-000000000001";
const EXPLORE_ID: &str = "e1e1e1e1-0003-4000-8000-000000000003";

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/claude-code-classify")
}

fn skiagram() -> Command {
    let mut cmd = Command::cargo_bin("skiagram").expect("binary builds");
    cmd.env("CLAUDE_CONFIG_DIR", fixtures_dir());
    cmd
}

/// Find the `by_task_type` bucket for a given serialized `TaskType` name.
fn bucket<'a>(report: &'a serde_json::Value, task_type: &str) -> &'a serde_json::Value {
    report["by_task_type"]
        .as_array()
        .expect("by_task_type array")
        .iter()
        .find(|b| b["task_type"] == task_type)
        .unwrap_or_else(|| panic!("bucket {task_type} present"))
}

/// Sum a serialized `Rollup`'s token fields (it exposes the components, not the
/// `total_tokens()` method) — mirrors `Rollup::total_tokens`.
fn rollup_tokens(rollup: &serde_json::Value) -> u64 {
    ["input", "output", "cache_creation", "cache_read"]
        .iter()
        .map(|k| rollup[k].as_u64().unwrap_or(0))
        .sum()
}

/// Find the per-session row for a given session id.
fn session<'a>(report: &'a serde_json::Value, id: &str) -> &'a serde_json::Value {
    report["sessions"]
        .as_array()
        .expect("sessions array")
        .iter()
        .find(|s| s["id"] == id)
        .unwrap_or_else(|| panic!("session {id} present"))
}

#[test]
fn classify_json_has_exact_spend_and_stable_labels() {
    let output = skiagram()
        .args(["classify", "--json", "--agent", "claude-code"])
        .output()
        .expect("runs");
    assert!(output.status.success());
    let v: serde_json::Value = serde_json::from_slice(&output.stdout).expect("valid JSON");

    // ---- grand totals (heuristic-independent: dedup + pricing only) ----
    assert_eq!(v["sessions_classified"], 4);
    assert_eq!(v["total_tokens"], 10_060);
    assert_eq!(v["has_unpriced"], false);
    let total_cost = v["total_cost_usd"].as_f64().expect("float");
    assert!(
        (total_cost - 0.0513).abs() < 1e-9,
        "expected 0.0513, got {total_cost}"
    );

    // ---- by task type: ordered by cost desc, with the exact rolled-up spend ----
    let by_type = v["by_task_type"].as_array().expect("array");
    let order: Vec<&str> = by_type
        .iter()
        .map(|b| b["task_type"].as_str().unwrap())
        .collect();
    assert_eq!(
        order,
        vec!["FeatureWork", "Debugging", "Exploration", "Unknown"]
    );

    let feature = bucket(&v, "FeatureWork");
    assert_eq!(feature["sessions"], 1);
    assert_eq!(feature["rollup"]["requests"], 2);
    assert_eq!(rollup_tokens(&feature["rollup"]), 5_000);
    let feat_share = feature["token_share"].as_f64().expect("float");
    assert!(
        (feat_share - (5_000.0 / 10_060.0)).abs() < 1e-9,
        "feature token_share {feat_share}"
    );

    // Dedup ran inside classify: the debugging session's duplicate req_dbg1 line
    // collapsed (3 assistant lines -> 2 requests), not summed to 3.
    assert_eq!(bucket(&v, "Debugging")["rollup"]["requests"], 2);

    // Sub-agent transcript folded into the parent's Exploration bucket (§8.3):
    // the parent's 2 requests + the child's 1 = 3, in one bucket / one session row.
    let explore = bucket(&v, "Exploration");
    assert_eq!(explore["sessions"], 1);
    assert_eq!(explore["rollup"]["requests"], 3);
    assert_eq!(rollup_tokens(&explore["rollup"]), 2_410);

    // ---- per-session rows: labels, confidence, folded spend ----
    let feat = session(&v, FEATURE_ID);
    assert_eq!(feat["task_type"], "FeatureWork");
    assert_eq!(feat["confidence"].as_f64(), Some(1.0));

    let dbg = session(&v, DEBUG_ID);
    assert_eq!(dbg["task_type"], "Debugging");
    assert_eq!(dbg["rollup"]["requests"], 2);

    let expl = session(&v, EXPLORE_ID);
    assert_eq!(expl["task_type"], "Exploration");
    assert_eq!(
        expl["rollup"]["requests"], 3,
        "child folded into the parent row"
    );

    // Unknown is an honest bucket: no signal -> confidence 0, empty signals.
    let unknown = v["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["task_type"] == "Unknown")
        .expect("an Unknown session");
    assert_eq!(unknown["confidence"].as_f64(), Some(0.0));
    assert_eq!(unknown["signals"].as_array().map(Vec::len), Some(0));
}

#[test]
fn classify_since_drops_out_of_window_sessions() {
    let output = skiagram()
        .args([
            "classify",
            "--json",
            "--agent",
            "claude-code",
            "--since",
            "2026-06-10",
        ])
        .output()
        .expect("runs");
    assert!(output.status.success());
    let v: serde_json::Value = serde_json::from_slice(&output.stdout).expect("valid JSON");

    // The 2026-06-09 Unknown session is out of window and drops entirely.
    assert_eq!(v["sessions_classified"], 3);
    assert_eq!(v["total_tokens"], 9_910);
    assert_eq!(v["since"], "2026-06-10");
    let total_cost = v["total_cost_usd"].as_f64().expect("float");
    assert!(
        (total_cost - 0.05025).abs() < 1e-9,
        "expected 0.05025, got {total_cost}"
    );
    assert!(
        v["by_task_type"]
            .as_array()
            .unwrap()
            .iter()
            .all(|b| b["task_type"] != "Unknown"),
        "no Unknown bucket once its only session is filtered out"
    );
}

#[test]
fn classify_table_surfaces_sections_and_labels() {
    skiagram()
        .args(["classify", "--agent", "claude-code"])
        .assert()
        .success()
        // Header + heuristic disclaimer.
        .stdout(predicate::str::contains("session(s) classified"))
        .stdout(predicate::str::contains("heuristic"))
        // Verbatim section headers (contract with the renderer).
        .stdout(predicate::str::contains("BY TASK TYPE"))
        .stdout(predicate::str::contains("SESSIONS"))
        // Human task labels and the deduplicated grand total.
        .stdout(predicate::str::contains("Feature work"))
        .stdout(predicate::str::contains("Debugging"))
        .stdout(predicate::str::contains("Exploration"))
        .stdout(predicate::str::contains("10,060 tokens"))
        // A surfaced signal proves the evidence is shown.
        .stdout(predicate::str::contains("prompt keyword"));
}
