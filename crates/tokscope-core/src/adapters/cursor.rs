//! Cursor adapter — STUB (roadmap v0.4).
//!
//! Data lives in `state.vscdb` (SQLite, key/value `ItemTable`) under
//! workspaceStorage; keys are undocumented and drift. Open READ-ONLY via
//! `rusqlite` (bundled) when implemented. See CLAUDE.md §7.

use crate::adapters::Adapter;
use crate::model::{Session, SessionRef};

pub struct Cursor;

impl Adapter for Cursor {
    fn id(&self) -> &'static str {
        "cursor"
    }

    fn detect(&self) -> bool {
        // `config_dir` = %APPDATA% / ~/Library/Application Support / ~/.config.
        directories::BaseDirs::new().is_some_and(|b| {
            b.config_dir()
                .join("Cursor")
                .join("User")
                .join("workspaceStorage")
                .is_dir()
        })
    }

    fn discover(&self) -> anyhow::Result<Vec<SessionRef>> {
        anyhow::bail!("cursor adapter not yet implemented (roadmap v0.4) — contributions welcome, see README \"Adding an agent\"")
    }

    fn parse(&self, _r: &SessionRef) -> anyhow::Result<Session> {
        anyhow::bail!("cursor adapter not yet implemented")
    }
}
