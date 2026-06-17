//! End-to-end test that a cached pricing override (what `--refresh-pricing` writes)
//! actually flows through the whole pipeline and changes the cost numbers: a
//! normally-unpriced Codex (gpt-*) session becomes priced once the cache supplies a
//! price. Fully offline — uses a synthetic cache fixture via `$SKIAGRAM_PRICING_CACHE`,
//! never the network.

use std::path::{Path, PathBuf};

use assert_cmd::Command;
use serde_json::Value;

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures")
}

/// Run `summary --json --agent codex` against the codex fixtures, optionally with
/// the synthetic price cache applied. The baseline forces a non-existent cache path
/// so no ambient `~/.config` cache can perturb it.
fn codex_summary(cache: Option<&str>) -> Value {
    let mut cmd = Command::cargo_bin("skiagram").expect("binary builds");
    cmd.env("CODEX_HOME", fixtures().join("codex"));
    match cache {
        Some(rel) => cmd.env("SKIAGRAM_PRICING_CACHE", fixtures().join(rel)),
        None => cmd.env(
            "SKIAGRAM_PRICING_CACHE",
            fixtures().join("pricing/__none__.json"),
        ),
    };
    let out = cmd
        .args(["summary", "--json", "--agent", "codex"])
        .output()
        .expect("runs");
    assert!(out.status.success(), "summary should succeed");
    serde_json::from_slice(&out.stdout).expect("valid JSON")
}

#[test]
fn cached_override_prices_a_normally_unpriced_codex_session() {
    // Baseline: the embedded snapshot has no gpt-* prices, so every request is
    // unpriced and the total cost is exactly zero.
    let base = codex_summary(None);
    assert_eq!(
        base["totals"]["cost_usd"].as_f64().expect("float"),
        0.0,
        "gpt-* is unpriced in the embedded snapshot"
    );
    assert!(
        base["totals"]["unpriced_requests"].as_u64().expect("int") > 0,
        "baseline has unpriced requests"
    );
    assert!(
        !base["unpriced_models"].as_array().unwrap().is_empty(),
        "baseline surfaces unpriced gpt models"
    );

    // With the cached override, the SAME session is now priced end-to-end.
    let priced = codex_summary(Some("pricing/litellm-cache.json"));
    assert!(
        priced["totals"]["cost_usd"].as_f64().expect("float") > 0.0,
        "the override must flow through to cost"
    );
    assert_eq!(
        priced["totals"]["unpriced_requests"].as_u64().expect("int"),
        0,
        "every gpt request is now priced by the override"
    );
    assert!(
        priced["unpriced_models"].as_array().unwrap().is_empty(),
        "no models remain unpriced once the cache supplies gpt prices"
    );
}
