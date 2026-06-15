//! End-to-end CLI tests for `tokscope flame` against the redacted fixtures in
//! `fixtures/claude-code` (the SAME tree the `summary` tests use).
//!
//! `CLAUDE_CONFIG_DIR` points the adapter at the fixtures, so these run the real
//! discover -> parse -> dedup -> flame::fold -> render pipeline. The point of
//! testing against the summary fixture is to prove the flamegraph's frame widths
//! AGREE with the deduplicated `summary` totals (the whole "correct accounting"
//! wedge): the folded weights must sum to the same numbers.
//!
//! Known facts about this fixture (mirrors `tests/cli.rs`):
//!   - deduplicated grand totals: input 5,600 + output 980 + cache_read 11,000 +
//!     cache_creation 500 = 18,080 tokens.
//!   - total cost $0.031925 -> 31,925 µ$.
//!   - two projects: `-home-dev-acme-app`, `-home-dev-blog`.
//!   - models: `claude-sonnet-4-5-20250929`, `claude-haiku-4-5-20251001`.
//!   - one sub-agent transcript (`agent-fix0001`) folds into parent session
//!     `3e9d2c41-7b5a-4f2e-9c1d-2f6b8a1c0e11`.

use std::path::{Path, PathBuf};

use assert_cmd::Command;
use predicates::prelude::*;

const PARENT_SESSION_ID: &str = "3e9d2c41-7b5a-4f2e-9c1d-2f6b8a1c0e11";
/// The sub-agent transcript's own session id (file stem). It must NEVER appear
/// as a session frame — sub-agent spend folds into its parent (§8.3).
const SUBAGENT_SESSION_ID: &str = "agent-fix0001";

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/claude-code")
}

fn tokscope() -> Command {
    let mut cmd = Command::cargo_bin("tokscope").expect("binary builds");
    cmd.env("CLAUDE_CONFIG_DIR", fixtures_dir());
    cmd
}

/// Sum the trailing whitespace-separated integer of every non-empty folded line.
fn sum_folded_values(stdout: &[u8]) -> u64 {
    let text = std::str::from_utf8(stdout).expect("utf8 stdout");
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            l.split_whitespace()
                .next_back()
                .unwrap_or_else(|| panic!("folded line has a value: {l:?}"))
                .parse::<u64>()
                .unwrap_or_else(|_| panic!("trailing token is a u64: {l:?}"))
        })
        .sum()
}

/// The folded weights must sum to the SAME deduplicated grand total the
/// `summary` command reports (18,080 tokens) — the flamegraph and the table
/// agree by construction.
#[test]
fn fold_tokens_sum_equals_known_dedup_total() {
    let output = tokscope()
        .args(["flame", "--fold", "--agent", "claude-code"])
        .output()
        .expect("runs");
    assert!(output.status.success());

    assert_eq!(
        sum_folded_values(&output.stdout),
        18_080,
        "folded token weights must match the deduplicated summary total"
    );

    let text = std::str::from_utf8(&output.stdout).expect("utf8");
    // Both projects, a real model, and a token-type leaf appear as frames.
    assert!(text.contains("-home-dev-acme-app"), "{text}");
    assert!(text.contains("-home-dev-blog"), "{text}");
    assert!(text.contains("claude-sonnet-4-5-20250929"), "{text}");
    assert!(text.contains("cache-read"), "{text}");

    // Every summed line is a 4-frame `;`-delimited path (project;session;model;type),
    // so it carries at least three `;` separators.
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let path = line.rsplit_once(' ').map(|(p, _)| p).unwrap_or(line);
        assert!(
            path.matches(';').count() >= 3,
            "expected a 4-frame path, got {line:?}"
        );
    }
}

/// §8.3: the sub-agent transcript folds into its PARENT session frame, so its
/// own id never appears as a session frame; the parent id does.
#[test]
fn fold_folds_subagent_into_parent_session() {
    let output = tokscope()
        .args(["flame", "--fold", "--agent", "claude-code"])
        .output()
        .expect("runs");
    assert!(output.status.success());
    let text = std::str::from_utf8(&output.stdout).expect("utf8");

    assert!(
        text.contains(PARENT_SESSION_ID),
        "parent session frame must be present: {text}"
    );
    assert!(
        !text.contains(SUBAGENT_SESSION_ID),
        "sub-agent id must NOT appear as a session frame (it folds into the parent): {text}"
    );
}

/// `flame --out <file>` writes a real SVG that embeds the frame labels and the
/// unit word (inferno renders both as SVG text).
#[test]
fn writes_a_valid_svg_file() {
    let out = Path::new(env!("CARGO_TARGET_TMPDIR")).join("flame.svg");

    tokscope()
        .args([
            "flame",
            "--out",
            out.to_str().expect("utf8 path"),
            "--agent",
            "claude-code",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("wrote flamegraph:"));

    let svg = std::fs::read_to_string(&out).expect("svg written");
    assert!(
        svg.starts_with("<?xml") || svg.contains("<svg"),
        "expected SVG markup, got: {}",
        &svg[..svg.len().min(120)]
    );
    assert!(svg.contains("-home-dev-acme-app"), "frame label embedded");
    assert!(svg.contains("tokens"), "unit (count_name) embedded");
}

/// Cost metric: the folded micro-USD weights sum to the fixture's total cost
/// ($0.031925 -> 31,925 µ$), within ±1% for per-leaf rounding.
#[test]
fn cost_metric_produces_micro_usd_stacks() {
    let output = tokscope()
        .args([
            "flame",
            "--fold",
            "--metric",
            "cost",
            "--agent",
            "claude-code",
        ])
        .output()
        .expect("runs");
    assert!(output.status.success());
    assert!(!output.stdout.is_empty(), "cost graph is non-empty");

    let sum = sum_folded_values(&output.stdout) as f64;
    let expected = 31_925.0;
    assert!(
        (sum - expected).abs() <= expected * 0.01,
        "expected ~{expected} µ$ (±1%), got {sum}"
    );
}
