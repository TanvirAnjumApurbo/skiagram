//! Command-line surface (`clap` derive).

use anyhow::Context;
use clap::{Parser, Subcommand};
use tokscope_core::analysis::aggregate::{aggregate, Filter};
use tokscope_core::analysis::drilldown::build_details;
use tokscope_core::model::Session;
use tokscope_core::pricing::PricingTable;
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

    /// Refresh model prices from LiteLLM before costing, then cache them for
    /// later offline runs. Requires the `network` build feature; OFF by default so
    /// the standard build never touches the network (CLAUDE.md §12).
    #[arg(long, global = true)]
    pub refresh_pricing: bool,

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
    /// Anomalies: the fat-tail requests that dominate spend, plus retry storms
    /// (rapid request bursts — candidate retry/loop episodes) (CLAUDE.md §6 / v0.3).
    Anomalies {
        /// Emit machine-readable JSON instead of tables.
        #[arg(long)]
        json: bool,
    },
    /// Task-type classification: break spend down by what you were doing
    /// (debugging / feature work / refactor / …), a heuristic inferred from each
    /// session's tool mix + prompt keywords (CLAUDE.md §2 / v0.3).
    Classify {
        /// Emit machine-readable JSON instead of tables.
        #[arg(long)]
        json: bool,
    },
    /// Flamegraph SVG of token spend (project → session → model → token-type).
    Flame {
        /// Output SVG path.
        #[arg(long, default_value = "tokscope-flame.svg")]
        out: std::path::PathBuf,
        /// Frame width metric.
        #[arg(long, value_enum, default_value = "tokens")]
        metric: MetricArg,
        /// Print the folded stacks to stdout instead of writing an SVG (for piping).
        #[arg(long)]
        fold: bool,
    },
    /// Interactive session browser (arrow keys / j k, q to quit).
    Tui,
    /// Live-tail: print the summary, then re-render whenever session files change
    /// (watches the agent's data dirs via `notify`; Ctrl-C to stop).
    Watch,
}

/// Width metric for the flamegraph. CLI-side mirror of
/// [`tokscope_core::analysis::flame::FlameMetric`] (the core crate has no clap
/// dependency, so the `ValueEnum` lives here).
#[derive(Clone, Copy, Debug, clap::ValueEnum)]
pub(crate) enum MetricArg {
    Tokens,
    Cost,
}

impl From<MetricArg> for tokscope_core::analysis::flame::FlameMetric {
    fn from(m: MetricArg) -> Self {
        match m {
            MetricArg::Tokens => Self::Tokens,
            MetricArg::Cost => Self::Cost,
        }
    }
}

fn parse_date(s: &str) -> Result<jiff::civil::Date, String> {
    s.parse().map_err(|e| format!("expected YYYY-MM-DD: {e}"))
}

pub fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let config = crate::config::Config::load();

    // Agent precedence: `--agent` > config `default_agent` > auto-detect.
    let adapter = match cli.agent.as_deref().or(config.default_agent.as_deref()) {
        Some(id) => adapters::by_id(id)?,
        None => adapters::auto_detect().ok_or_else(|| {
            anyhow::anyhow!(
                "no supported agent data found on this machine; \
                 pass --agent <id> (known: claude-code, codex, cursor, gemini, copilot) \
                 or set default_agent in config.toml"
            )
        })?,
    };

    // Pricing table threaded into every cost computation. The embedded snapshot is
    // the default; `--refresh-pricing` / a cached refresh layer overrides on top.
    let pricing = crate::pricing::build_pricing(cli.refresh_pricing)?;

    match cli.command.unwrap_or(Command::Summary { json: false }) {
        Command::Summary { json } => {
            let summary = collect(adapter.as_ref(), cli.since, &pricing)?;
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
        Command::Anomalies { json } => {
            let (sessions, _failed) = collect_sessions(adapter.as_ref())?;
            let filter = Filter { since: cli.since };
            let report = tokscope_core::analysis::anomaly::detect(
                &sessions,
                &filter,
                adapter.id(),
                &pricing,
            );
            if json {
                crate::render::json::print(&report)?;
            } else {
                crate::render::anomalies::print(&report);
            }
        }
        Command::Classify { json } => {
            let (sessions, _failed) = collect_sessions(adapter.as_ref())?;
            let filter = Filter { since: cli.since };
            let report = tokscope_core::analysis::classify::classify(
                &sessions,
                &filter,
                adapter.id(),
                &pricing,
            );
            if json {
                crate::render::json::print(&report)?;
            } else {
                crate::render::classify::print(&report);
            }
        }
        Command::Flame { out, metric, fold } => {
            let (sessions, _failed) = collect_sessions(adapter.as_ref())?;
            let filter = Filter { since: cli.since };
            let data = tokscope_core::analysis::flame::fold(
                &sessions,
                &filter,
                adapter.id(),
                metric.into(),
                &pricing,
            );
            if data.stacks.is_empty() {
                // Honest empty state; inferno would otherwise emit a "No stack
                // counts" SVG. Under the Cost metric, point at unpriced models
                // (the likely reason a non-empty token graph is empty here, §8.7).
                let why = if data.unpriced_requests > 0 {
                    format!(
                        " ({} request(s) on unpriced models excluded)",
                        data.unpriced_requests
                    )
                } else {
                    String::new()
                };
                println!("no token spend found{why} — nothing to graph.");
            } else if fold {
                crate::render::flame::print_folded(&data);
            } else {
                let file = std::fs::File::create(&out)
                    .with_context(|| format!("creating {}", out.display()))?;
                crate::render::flame::write_svg(
                    &data,
                    std::io::BufWriter::new(file),
                    "tokscope — token spend",
                )?;
                println!(
                    "wrote flamegraph: {} ({} stacks · {} {} total)",
                    out.display(),
                    data.stacks.len(),
                    crate::render::fmt_count(data.total_value),
                    data.unit
                );
                if data.unpriced_requests > 0 {
                    // §8.7: unpriced models are excluded from a cost graph, never guessed.
                    println!(
                        "note: {} request(s) on unpriced models were excluded (cost graph)",
                        data.unpriced_requests
                    );
                }
            }
        }
        Command::Tui => {
            // Parse once, then build both the summary (header/totals) and the
            // per-session drill-down details (turns + context) for the TUI.
            let (sessions, failed) = collect_sessions(adapter.as_ref())?;
            let filter = Filter { since: cli.since };
            let summary = aggregate(&sessions, &filter, failed, adapter.id(), &pricing);
            let details = build_details(&sessions, &filter, adapter.id(), &pricing);
            crate::tui::run(&summary, &details)?;
        }
        Command::Watch => {
            crate::watch::run(adapter.as_ref(), cli.since, &pricing)?;
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

/// Discover -> parse (leniently) -> dedup + aggregate. Shared by `summary` and the
/// live-tail `watch` loop.
pub(crate) fn collect(
    adapter: &dyn adapters::Adapter,
    since: Option<jiff::civil::Date>,
    pricing: &PricingTable,
) -> anyhow::Result<Summary> {
    let (sessions, failed) = collect_sessions(adapter)?;
    Ok(aggregate(
        &sessions,
        &Filter { since },
        failed,
        adapter.id(),
        pricing,
    ))
}
