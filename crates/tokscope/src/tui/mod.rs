//! Minimal interactive session browser (`ratatui` + crossterm backend).
//!
//! MVP scope: a scrollable session list. TODO(scope): drill-down — session ->
//! turns -> context breakdown — lands with v0.2 context attribution.

mod app;
mod event;
mod views;

pub use app::run;
