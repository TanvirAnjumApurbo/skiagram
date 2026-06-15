//! Cursor adapter — STUB, intentionally deferred (CLAUDE.md §11 v0.4).
//!
//! Schema VERIFIED against real local data 2026-06-16 (Windows, `%APPDATA%`):
//! - Chat lives in the **`cursorDiskKV`** table of `state.vscdb` (NOT `ItemTable`,
//!   which only holds UI/workbench keys). Cross-workspace conversations are in
//!   `User/globalStorage/state.vscdb`; per-workspace ones under
//!   `User/workspaceStorage/<hash>/state.vscdb`.
//! - Keys: `composerData:<composerId>` = one conversation
//!   (`modelConfig.modelName`, `contextTokensUsed`/`contextTokenLimit`,
//!   `fullConversationHeadersOnly` ordering); `bubbleId:<composerId>:<bubbleId>`
//!   = one message (`type`, `requestId`, `toolFormerData`, `toolResults`, and a
//!   `tokenCount: { inputTokens, outputTokens }`).
//!
//! WHY DEFERRED (not a difficulty — a payoff problem): on real data the per-message
//! `tokenCount` is **~99% zeroed** (3 of 275 bubbles non-zero in a real
//! globalStorage db) and `modelConfig.modelName` is frequently the literal
//! `"default"` (unresolvable to a price). So Cursor cannot deliver tokscope's core
//! "correct accounting" wedge — it would be a structural-only adapter (sessions /
//! models / context-window fill) like the Copilot one. Implementing it also pulls
//! in `rusqlite` with the bundled C SQLite (a compiled dep on a project that prizes
//! a lean static binary, §12). The cost/benefit doesn't clear the bar yet; revisit
//! if Cursor starts recording real per-request usage. Open READ-ONLY when built.

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
        anyhow::bail!("cursor adapter not yet implemented — deferred by design (Cursor's per-request token counts are ~99% zeroed on real data); see adapters/cursor.rs module docs")
    }

    fn parse(&self, _r: &SessionRef) -> anyhow::Result<Session> {
        anyhow::bail!("cursor adapter not yet implemented")
    }
}
