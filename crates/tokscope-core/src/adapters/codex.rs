//! Codex CLI adapter — STUB (roadmap v0.4).
//!
//! Data lives in `~/.codex/sessions/YYYY/MM/DD/<session>.jsonl` (+
//! `archived_sessions/`), `CODEX_HOME` override possible. Three schema
//! generations exist (>=0.44 "new", "mid", 2025-08 "old") — the parser must
//! branch on version. See CLAUDE.md §7.

use crate::adapters::Adapter;
use crate::model::{Session, SessionRef};

pub struct Codex;

impl Adapter for Codex {
    fn id(&self) -> &'static str {
        "codex"
    }

    fn detect(&self) -> bool {
        directories::BaseDirs::new()
            .is_some_and(|b| b.home_dir().join(".codex").join("sessions").is_dir())
    }

    fn discover(&self) -> anyhow::Result<Vec<SessionRef>> {
        anyhow::bail!("codex adapter not yet implemented (roadmap v0.4) — contributions welcome, see README \"Adding an agent\"")
    }

    fn parse(&self, _r: &SessionRef) -> anyhow::Result<Session> {
        anyhow::bail!("codex adapter not yet implemented")
    }
}
