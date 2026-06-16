//! End-to-end tests for `config.toml` routing via `$TOKSCOPE_CONFIG`.
//!
//! `default_agent` is honored when `--agent` is absent, and `--agent` overrides it.

use std::path::{Path, PathBuf};

use assert_cmd::Command;
use predicates::prelude::*;

fn fixtures_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures")
}

fn config_path(name: &str) -> PathBuf {
    fixtures_root().join("config").join(name)
}

#[test]
fn default_agent_from_config_is_used_when_no_flag() {
    // config points default_agent at the still-stubbed `gemini`; with no --agent
    // the CLI must route there (and reach the stub's loud failure), proving the
    // config value was honored rather than auto-detection kicking in.
    Command::cargo_bin("tokscope")
        .expect("binary builds")
        .env("TOKSCOPE_CONFIG", config_path("gemini.toml"))
        .arg("summary")
        .assert()
        .failure()
        .stderr(predicate::str::contains("not yet implemented"));
}

#[test]
fn explicit_agent_flag_overrides_config_default() {
    // Same config (default_agent = gemini), but --agent claude-code must win and
    // produce the real deduplicated summary from the claude fixtures.
    Command::cargo_bin("tokscope")
        .expect("binary builds")
        .env("TOKSCOPE_CONFIG", config_path("gemini.toml"))
        .env("CLAUDE_CONFIG_DIR", fixtures_root().join("claude-code"))
        .args(["summary", "--agent", "claude-code"])
        .assert()
        .success()
        .stdout(predicate::str::contains("18,080"));
}
