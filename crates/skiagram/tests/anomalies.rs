//! End-to-end CLI tests for `skiagram anomalies` against the redacted fixture
//! in `fixtures/claude-code-anomaly`.
//!
//! `CLAUDE_CONFIG_DIR` points the adapter at the fixture, so these run the real
//! discover -> parse -> dedup -> anomaly::detect -> render pipeline.
//!
//! ---- Fixture layout ----
//! One session
//! (`projects/-home-dev-anomaly-app/aaaaaaaa-1111-4000-8000-000000000aaa.jsonl`)
//! with 11 assistant lines, all on the PRICED model `claude-sonnet-4-5-20250929`
//! (input $3/M, output $15/M, cache_read $0.30/M, cache_write_5m $3.75/M —
//! see `crates/skiagram-core/src/pricing.rs`):
//!
//!   - req_s0 @ 10:00:00  input=100  output=50  cache_read=10000  -> 10,150 tok
//!   - req_s1 @ 10:00:05  input=100  output=50  cache_read=10000  -> 10,150 tok
//!   - req_s2 @ 10:00:10  input=100  output=50  cache_read=10000  -> 10,150 tok
//!   - req_s3 @ 10:00:14  input=100  output=50  cache_read=10000  -> 10,150 tok
//!   - req_s4 @ 10:00:18  input=100  output=50  cache_read=10000  -> 10,150 tok
//!     (consecutive gaps 5,5,4,4 s, all <= 15s, span = 18 - 0 = 18s, 5 reqs:
//!     exactly meets STORM_MIN_REQUESTS (5) / STORM_MAX_GAP_SECONDS (15). The
//!     NEXT request (req_dup) is at 10:05:00, a 282s gap -> storm ends at
//!     exactly 5.)
//!   - req_dup line 1 @ 10:05:00  input=200 output=30 cache_read=1000 -> 1,230 tok
//!   - req_dup line 2 @ 10:05:01  input=200 output=80 cache_read=1000 -> 1,280 tok
//!     (SAME requestId, field-wise MAX (dedup.rs) -> input=200 output=80
//!     cache_read=1000 -> 1,280 tok. Two lines collapse to ONE request, so
//!     requests_analyzed (9) < assistant lines with usage (10). Far outside
//!     the storm window: gap from req_s4 = 282s > 15s.)
//!   - req_big @ 11:00:00  input=5000 output=300 cache_creation(5m)=200000
//!     cache_read=0 -> 205,300 tok. Dominant fat-tail request: alone it is
//!     ~79.7% of all tokens. Gap from req_dup = 3540s > 15s, so NOT in a storm.
//!   - req_f1 @ 12:00:00  input=50 output=20 -> 70 tok (filler)
//!   - req_f2 @ 13:00:00  input=80 output=40 -> 120 tok (filler)
//!
//! ---- Hand-derived totals (n = 9 deduplicated requests) ----
//!   total_tokens = 5*10150 + 1280 + 205300 + 70 + 120
//!                = 50750 + 1280 + 205300 + 70 + 120 = 257520
//!
//! ---- Cost (USD per request, sonnet-4-5: in=$3/M out=$15/M cr=$0.30/M cw5m=$3.75/M) ----
//!   storm (each):  (100*3 + 50*15 + 10000*0.30) / 1e6
//!                = (300 + 750 + 3000) / 1e6 = 4050 / 1e6 = 0.00405
//!   storm (x5)   = 0.0202500
//!   req_dup:       (200*3 + 80*15 + 1000*0.30) / 1e6
//!                = (600 + 1200 + 300) / 1e6 = 2100 / 1e6 = 0.00210
//!   req_big:       (5000*3 + 300*15 + 200000*3.75) / 1e6
//!                = (15000 + 4500 + 750000) / 1e6 = 769500 / 1e6 = 0.76950
//!   req_f1:        (50*3 + 20*15) / 1e6 = (150 + 300) / 1e6 = 0.00045
//!   req_f2:        (80*3 + 40*15) / 1e6 = (240 + 600) / 1e6 = 0.00084
//!   total_cost_usd = 0.02025 + 0.00210 + 0.76950 + 0.00045 + 0.00084
//!                  = 0.79314
//!
//! ---- Concentration (n = 9, ranked by tokens desc) ----
//!   order: req_big(205300), then the five storm reqs (10150 each, tied — broken
//!   by request_id ascending: req_s0..req_s4), then req_dup(1280), req_f2(120),
//!   req_f1(70).
//!   top  1% -> ceil(0.01*9)=ceil(0.09) = 1  -> req_big alone
//!   top  5% -> ceil(0.05*9)=ceil(0.45) = 1  -> req_big alone
//!   top 10% -> ceil(0.10*9)=ceil(0.90) = 1  -> req_big alone
//!     token_share = 205300 / 257520 = 0.79721963342653
//!     cost_share  = 0.76950 / 0.79314 = 0.9701944171268629
//!   top 25% -> ceil(0.25*9)=ceil(2.25) = 3  -> req_big + any 2 of the (tied)
//!     storm reqs (each 10150, so the sum is the same regardless of tiebreak):
//!     tokens = 205300 + 2*10150 = 225600
//!     token_share = 225600 / 257520 = 0.8760484622553588
//!
//! ---- Retry storm ----
//!   requests = 5, span_seconds = 18, total_tokens = 50750,
//!   cost_usd = storm(x5) = 0.02025, session_id = the fixture's session uuid.

use std::path::{Path, PathBuf};

use assert_cmd::Command;
use predicates::prelude::*;

const SESSION_ID: &str = "aaaaaaaa-1111-4000-8000-000000000aaa";

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/claude-code-anomaly")
}

fn skiagram() -> Command {
    let mut cmd = Command::cargo_bin("skiagram").expect("binary builds");
    cmd.env("CLAUDE_CONFIG_DIR", fixtures_dir());
    cmd
}

#[test]
fn anomalies_json_has_exact_fat_tail_and_storm_numbers() {
    let output = skiagram()
        .args(["anomalies", "--json", "--agent", "claude-code"])
        .output()
        .expect("runs");
    assert!(output.status.success());
    let v: serde_json::Value = serde_json::from_slice(&output.stdout).expect("valid JSON");

    // 10 assistant lines with usage, but req_dup's two lines share one
    // requestId and dedup (field-wise MAX) collapses them -> 9 requests.
    assert_eq!(v["requests_analyzed"], 9);
    assert_eq!(v["total_tokens"], 257520);
    // Every line uses a priced model (claude-sonnet-4-5-20250929).
    assert_eq!(v["has_unpriced"], false);

    // total_cost_usd = 0.79314 (see derivation above).
    let total_cost = v["total_cost_usd"].as_f64().expect("float");
    assert!(
        (total_cost - 0.79314).abs() < 1e-9,
        "expected 0.79314, got {total_cost}"
    );

    // ---- concentration ----
    let concentration = v["concentration"].as_array().expect("array");
    assert_eq!(concentration.len(), 4);

    let bucket = |frac: f64| -> &serde_json::Value {
        concentration
            .iter()
            .find(|b| (b["request_fraction"].as_f64().unwrap() - frac).abs() < 1e-9)
            .unwrap_or_else(|| panic!("bucket for fraction {frac} present"))
    };

    // top 10%: ceil(0.10 * 9) = 1 request (req_big alone).
    let b10 = bucket(0.10);
    assert_eq!(b10["requests"], 1);
    let share10 = b10["token_share"].as_f64().expect("float");
    assert!(
        (share10 - (205_300.0 / 257_520.0)).abs() < 1e-9,
        "expected ~0.79722, got {share10}"
    );
    let cost_share10 = b10["cost_share"].as_f64().expect("float");
    assert!(
        (cost_share10 - (0.76950 / 0.79314)).abs() < 1e-9,
        "expected ~0.97019, got {cost_share10}"
    );

    // top 25%: ceil(0.25 * 9) = 3 requests (req_big + 2 of the tied storm reqs,
    // each 10,150 tok, so the sum is deterministic regardless of tiebreak).
    let b25 = bucket(0.25);
    assert_eq!(b25["requests"], 3);
    let share25 = b25["token_share"].as_f64().expect("float");
    assert!(
        (share25 - (225_600.0 / 257_520.0)).abs() < 1e-9,
        "expected ~0.87605, got {share25}"
    );

    // ---- heaviest individual requests ----
    let heaviest = v["heaviest"].as_array().expect("array");
    assert_eq!(heaviest[0]["request_id"], "req_big");
    assert_eq!(heaviest[0]["total_tokens"], 205_300);
    let big_share = heaviest[0]["token_share"].as_f64().expect("float");
    assert!(
        (big_share - (205_300.0 / 257_520.0)).abs() < 1e-9,
        "expected ~0.79722, got {big_share}"
    );
    let big_cost = heaviest[0]["cost_usd"].as_f64().expect("float");
    assert!(
        (big_cost - 0.76950).abs() < 1e-9,
        "expected 0.76950, got {big_cost}"
    );

    // ---- retry storms ----
    let storms = v["retry_storms"].as_array().expect("array");
    assert_eq!(storms.len(), 1, "exactly one storm detected");
    let storm = &storms[0];
    assert_eq!(storm["requests"], 5);
    assert_eq!(storm["span_seconds"], 18);
    assert_eq!(storm["session_id"], SESSION_ID);
    assert_eq!(storm["total_tokens"], 50_750);
    let storm_cost = storm["cost_usd"].as_f64().expect("float");
    assert!(
        (storm_cost - 0.02025).abs() < 1e-9,
        "expected 0.02025, got {storm_cost}"
    );

    // Heuristic params are echoed so the report is self-describing.
    assert_eq!(v["storm_min_requests"], 5);
    assert_eq!(v["storm_max_gap_seconds"], 15);
}

#[test]
fn anomalies_table_surfaces_storm_and_fat_tail() {
    skiagram()
        .args(["anomalies", "--agent", "claude-code"])
        .assert()
        .success()
        // "N request(s) analyzed" header.
        .stdout(predicate::str::contains("request(s) analyzed"))
        // Verbatim section headers (contract with the renderer).
        .stdout(predicate::str::contains("CONCENTRATION"))
        .stdout(predicate::str::contains("HEAVIEST REQUESTS"))
        .stdout(predicate::str::contains("RETRY STORMS"))
        // The dominant fat-tail request is named and dominates the share.
        .stdout(predicate::str::contains("req_big"))
        .stdout(predicate::str::contains("205,300"))
        // Storm evidence: NOT the "none detected." empty state, and the
        // burst's request count (5) / span (18 s) / tokens (50,750) appear.
        .stdout(predicate::str::contains("none detected.").not())
        .stdout(predicate::str::contains("50,750"))
        .stdout(predicate::str::contains("18"));
}
