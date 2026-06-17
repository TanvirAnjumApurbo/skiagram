//! The `Adapter` trait — tokscope's extension point (CLAUDE.md §5).
//!
//! Adding a new agent = implement [`Adapter`] + register it in [`all`] + commit
//! redacted fixtures and snapshot tests. Keep the trait minimal and stable.

pub mod claude_code;
pub mod codex;
pub mod copilot;
pub mod cursor;
pub mod gemini;

use crate::error::CoreError;
use crate::model::{Session, SessionRef};

/// One supported agent's on-disk format.
pub trait Adapter {
    /// Stable identifier used by `--agent`, e.g. `"claude-code"`.
    fn id(&self) -> &'static str;
    /// Cheap check: are this agent's data files present on this machine?
    fn detect(&self) -> bool;
    /// Find session files (read-only; never modifies anything).
    fn discover(&self) -> anyhow::Result<Vec<SessionRef>>;
    /// Parse one session file into the normalized model. Must be lenient:
    /// unknown/corrupt lines are skipped with a `tracing::debug`, never a panic,
    /// and never abort the whole file (CLAUDE.md §9).
    fn parse(&self, r: &SessionRef) -> anyhow::Result<Session>;
}

/// Every known adapter, in priority order (fully implemented ones first; the
/// deferred Cursor stub is last so a real coding-agent wins auto-detect).
pub fn all() -> Vec<Box<dyn Adapter>> {
    vec![
        Box::new(claude_code::ClaudeCode),
        Box::new(codex::Codex),
        Box::new(gemini::Gemini),
        Box::new(copilot::Copilot),
        Box::new(cursor::Cursor),
    ]
}

/// Look an adapter up by id (the `--agent` flag).
pub fn by_id(id: &str) -> Result<Box<dyn Adapter>, CoreError> {
    all()
        .into_iter()
        .find(|a| a.id() == id)
        .ok_or_else(|| CoreError::UnknownAgent {
            requested: id.to_string(),
            known: all().iter().map(|a| a.id()).collect::<Vec<_>>().join(", "),
        })
}

/// First adapter whose data files exist on this machine (used when `--agent` is
/// not given).
pub fn auto_detect() -> Option<Box<dyn Adapter>> {
    all().into_iter().find(|a| a.detect())
}
