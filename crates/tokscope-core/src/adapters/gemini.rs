//! Gemini CLI adapter — STUB, unverified (CLAUDE.md §7 calls Gemini "less stable").
//!
//! VERIFIED 2026-06-16: on this machine `~/.gemini/` is **Google Antigravity** (an
//! IDE), not the Gemini CLI — it holds `antigravity*/`, `config/projects/*.json`
//! (project resources, no usage), and `tmp/<hash>/logs.json` (a few bytes of UI
//! telemetry: `{sessionId, messageId, type, message, timestamp}` — no token
//! transcript). No per-session token/usage data was found to implement against, so
//! per CLAUDE.md §13 ("trust the file; don't fabricate") this stays a stub rather
//! than a speculative parser. NOTE: [`detect`] therefore false-positives on an
//! Antigravity install; it stays last in the adapter priority list so a real
//! coding-agent (claude-code/codex/...) is auto-detected first. Revisit if a real
//! Gemini-CLI session layout is confirmed.

use crate::adapters::Adapter;
use crate::model::{Session, SessionRef};

pub struct Gemini;

impl Adapter for Gemini {
    fn id(&self) -> &'static str {
        "gemini"
    }

    fn detect(&self) -> bool {
        directories::BaseDirs::new().is_some_and(|b| b.home_dir().join(".gemini").is_dir())
    }

    fn discover(&self) -> anyhow::Result<Vec<SessionRef>> {
        anyhow::bail!("gemini adapter not yet implemented — contributions welcome, see README \"Adding an agent\"")
    }

    fn parse(&self, _r: &SessionRef) -> anyhow::Result<Session> {
        anyhow::bail!("gemini adapter not yet implemented")
    }
}
