//! Command-line surface (`clap` derive).

use anyhow::Context;
use clap::{Parser, Subcommand};
use tokscope_core::analysis::aggregate::{aggregate, Filter};
use tokscope_core::model::Session;
use tokscope_core::{adapters, analysis::aggregate::Summary};

#[derive(Parser)]
#[command(
    name = "tokscope",
    version,
    about = "Profile where your AI coding agent's tokens actually went — locally, offline.",
    long_about = "Reads the session files your AI coding agent already writes (read-only), \
deduplicates per-request token usage, and shows where the spend went.\n\
Nothing ever leaves this machine."
)]
pub struct Cli {
    /// Agent to read (default: auto-detect). Known: claude-code, codex, cursor, gemini, copilot.
    #[arg(long, global = true)]
    pub agent: Option<String>,

    /// Only count usage on/after this UTC date (YYYY-MM-DD).
    #[arg(long, global = true, value_parser = parse_date)]
    pub since: Option<jiff::civil::Date>,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand)]
pub enum Command {
    /// Token + cost summary (the default command).
    Summary {
        /// Emit machine-readable JSON instead of tables.
        #[arg(long)]
        json: bool,
    },
    /// Context-window breakdown: startup overhead, by-source, by-MCP-server,
    /// and the heaviest individual contributors (CLAUDE.md §2.2 / v0.2).
    Context {
        /// Emit machine-readable JSON instead of tables.
        #[arg(long)]
        json: bool,
    },
    /// Interactive session browser (arrow keys / j k, q to quit).
    Tui,
}

fn parse_date(s: &str) -> Result<jiff::civil::Date, String> {
    s.parse().map_err(|e| format!("expected YYYY-MM-DD: {e}"))
}

pub fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();

    let adapter = match &cli.agent {
        Some(id) => adapters::by_id(id)?,
        None => adapters::auto_detect().ok_or_else(|| {
            anyhow::anyhow!(
                "no supported agent data found on this machine; \
                 pass --agent <id> (known: claude-code, codex, cursor, gemini, copilot)"
            )
        })?,
    };

    match cli.command.unwrap_or(Command::Summary { json: false }) {
        Command::Summary { json } => {
            let summary = collect(adapter.as_ref(), cli.since)?;
            if json {
                crate::render::json::print(&summary)?;
            } else {
                crate::render::table::print(&summary);
            }
        }
        Command::Context { json } => {
            let (sessions, _failed) = collect_sessions(adapter.as_ref())?;
            let report =
                tokscope_core::analysis::context::profile(&sessions, adapter.id(), cli.since);
            if json {
                crate::render::json::print(&report)?;
            } else {
                crate::render::table::print_context(&report);
            }
        }
        Command::Tui => {
            let summary = collect(adapter.as_ref(), cli.since)?;
            crate::tui::run(&summary)?;
        }
    }
    Ok(())
}

/// Discover -> parse (leniently) all of an adapter's session files.
///
/// One unreadable file must not kill the report (CLAUDE.md §12): failures are
/// counted and logged, not propagated.
fn collect_sessions(adapter: &dyn adapters::Adapter) -> anyhow::Result<(Vec<Session>, u64)> {
    let refs = adapter
        .discover()
        .with_context(|| format!("discovering {} sessions", adapter.id()))?;

    let mut sessions = Vec::with_capacity(refs.len());
    let mut failed = 0u64;
    for r in &refs {
        match adapter.parse(r) {
            Ok(s) => sessions.push(s),
            Err(e) => {
                failed += 1;
                tracing::warn!("failed to parse {}: {e:#}", r.path.display());
            }
        }
    }
    Ok((sessions, failed))
}

/// Discover -> parse (leniently) -> dedup + aggregate.
fn collect(
    adapter: &dyn adapters::Adapter,
    since: Option<jiff::civil::Date>,
) -> anyhow::Result<Summary> {
    let (sessions, failed) = collect_sessions(adapter)?;
    Ok(aggregate(
        &sessions,
        &Filter { since },
        failed,
        adapter.id(),
    ))
}
