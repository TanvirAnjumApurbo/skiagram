//! Interactive drill-down browser (`ratatui` + crossterm backend).
//!
//! Three levels (v0.2): **Sessions → Turns → Context** — `Enter` descends,
//! `Esc`/`Backspace` ascends, `q` quits. The session list shows the same folded,
//! sub-agent-inclusive rows as `summary`; drilling in reuses the deduplicated
//! per-request turns and the per-session context-bloat breakdown.

mod app;
mod event;
mod views;

pub use app::run;
