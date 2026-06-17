//! Flamegraph SVG export of token spend (project -> session -> model -> token-type).
//!
//! Renders an SVG "flamegraph for agent token spend" (CLAUDE.md §2.4) via the
//! `inferno` crate. The folded-stack data arrives pre-aggregated and
//! deduplicated from [`skiagram_core::analysis::flame`] — request-level dedup is
//! THE accounting step (§8.1), so the graph's frame widths agree with the
//! `summary` totals. We never re-sum raw JSONL lines here; the binary only owns
//! the rendering (and the `inferno` dependency the core crate must never gain).
//!
//! `--fold` skips the SVG entirely and emits the raw folded text (one
//! `frame;frame;… value` line per stack) to stdout, for piping into other
//! flamegraph tooling or inspection.

use anyhow::Context;
use inferno::flamegraph::{from_lines, Options};
use skiagram_core::analysis::flame::FlameData;

/// Render `data` as an SVG flamegraph into `out`.
///
/// Writer-generic so it is unit-testable and usable with a `BufWriter<File>`.
/// Frames are coloured by name (`hash = true`) so the output is stable and
/// reproducible across runs — no RNG, no per-run diff churn.
pub fn write_svg(data: &FlameData, out: impl std::io::Write, title: &str) -> anyhow::Result<()> {
    let mut opts = Options::default();
    opts.title = title.to_string();
    opts.count_name = data.unit.to_string();
    let since = data
        .since
        .map(|d| format!(" · since {d}"))
        .unwrap_or_default();
    opts.subtitle = Some(format!(
        "{} · {} {} total{}",
        data.agent,
        super::fmt_count(data.total_value),
        data.unit,
        since
    ));
    opts.hash = true; // colour frames by name -> stable, reproducible SVG (no RNG)

    // inferno wants `Item = &'a str`; materialise the owned lines first, then
    // borrow them, so the iterator's references outlive the call.
    let lines: Vec<String> = data.stacks.iter().map(|s| s.to_folded_line()).collect();
    from_lines(&mut opts, lines.iter().map(String::as_str), out)
        .context("rendering flamegraph SVG")?;
    Ok(())
}

/// Print the raw folded stacks to stdout, one `frame;frame;… value` line each
/// (for piping into other flamegraph tooling / inspection). Nothing else.
pub fn print_folded(data: &FlameData) {
    for s in &data.stacks {
        println!("{}", s.to_folded_line());
    }
}
