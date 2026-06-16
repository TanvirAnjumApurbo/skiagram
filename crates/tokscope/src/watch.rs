//! Live-tail: re-render the summary whenever the agent's session files change.
//!
//! `tokscope watch` prints the summary once, then watches the directories that hold
//! the discovered session files (via `notify`) and re-renders on change, coalescing
//! the burst of filesystem events a single write produces. Runs until interrupted
//! (Ctrl-C). Strictly read-only — it only watches, never writes (CLAUDE.md §12).

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

use anyhow::{Context, Result};
use jiff::civil::Date;
use notify::{RecursiveMode, Watcher};
use ratatui::crossterm::{
    cursor::MoveTo,
    execute,
    terminal::{Clear, ClearType},
};
use tokscope_core::adapters::Adapter;
use tokscope_core::pricing::PricingTable;

/// Quiet period after the last change before re-rendering — coalesces the burst of
/// events a single save produces into one refresh.
const DEBOUNCE: Duration = Duration::from_millis(400);

/// Distinct parent directories of the discovered session files: the set of dirs to
/// watch. Pure, so it is unit-tested without touching the filesystem.
fn distinct_parents(paths: &[PathBuf]) -> Vec<PathBuf> {
    paths
        .iter()
        .filter_map(|p| p.parent().map(|d| d.to_path_buf()))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// Print the summary once, then re-print on every (debounced) change until Ctrl-C.
pub fn run(adapter: &dyn Adapter, since: Option<Date>, pricing: &PricingTable) -> Result<()> {
    render(adapter, since, pricing)?;

    let paths: Vec<PathBuf> = adapter
        .discover()
        .unwrap_or_default()
        .into_iter()
        .map(|r| r.path)
        .collect();
    let roots = distinct_parents(&paths);
    if roots.is_empty() {
        anyhow::bail!(
            "nothing to watch for `{}` — no session files found yet",
            adapter.id()
        );
    }

    let (tx, rx) = mpsc::channel();
    let mut watcher = notify::recommended_watcher(move |res| {
        // The event content doesn't matter — any change triggers a refresh.
        let _ = tx.send(res);
    })
    .context("creating the file watcher")?;
    for root in &roots {
        // Recursive so newly-created date-bucketed subdirs (e.g. Codex) are seen.
        if let Err(e) = watcher.watch(root, RecursiveMode::Recursive) {
            tracing::warn!("cannot watch {}: {e}", root.display());
        }
    }
    eprintln!(
        "watching {} dir(s) for `{}` — Ctrl-C to stop",
        roots.len(),
        adapter.id()
    );

    // Block for a change, drain the ensuing burst within the debounce window, then
    // re-render. `recv` only errors once the watcher is dropped (process exit).
    while rx.recv().is_ok() {
        while rx.recv_timeout(DEBOUNCE).is_ok() {}
        render(adapter, since, pricing)?;
    }
    Ok(())
}

/// Clear the screen and print a fresh summary in place.
fn render(adapter: &dyn Adapter, since: Option<Date>, pricing: &PricingTable) -> Result<()> {
    let summary = crate::cli::collect(adapter, since, pricing)?;
    // crossterm handles the Windows/Unix differences (and enables VT on Windows).
    let mut out = std::io::stdout();
    let _ = execute!(out, Clear(ClearType::All), MoveTo(0, 0));
    crate::render::table::print(&summary);
    println!("\n(live — refreshes on change · Ctrl-C to stop)");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distinct_parents_dedups_and_sorts() {
        let paths = vec![
            PathBuf::from("/a/proj1/s1.jsonl"),
            PathBuf::from("/a/proj1/s2.jsonl"),
            PathBuf::from("/a/proj2/s3.jsonl"),
        ];
        assert_eq!(
            distinct_parents(&paths),
            vec![PathBuf::from("/a/proj1"), PathBuf::from("/a/proj2")]
        );
    }

    #[test]
    fn distinct_parents_is_empty_for_no_paths() {
        assert!(distinct_parents(&[]).is_empty());
    }
}
