//! Gemini CLI adapter — STUB.
//!
//! TODO(verify): layout under `~/.gemini/` must be confirmed on a real install
//! before implementing (CLAUDE.md §7 calls it "less stable").

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
