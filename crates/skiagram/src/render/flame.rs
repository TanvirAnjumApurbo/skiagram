//! Flamegraph SVG export of token spend (project -> session -> model -> token-type).
//!
//! Renders an SVG "flamegraph for agent token spend" (CLAUDE.md §2.4) via the
//! `inferno` crate. The folded-stack data arrives pre-aggregated and
//! deduplicated from [`skiagram_core::analysis::flame`] — request-level dedup is
//! THE accounting step (§8.1), so the graph's frame widths agree with the
//! `summary` totals. We never re-sum raw JSONL lines here; the binary only owns
//! the rendering (and the `inferno` dependency the core crate must never gain).
//!
//! Token-type leaves are colored by a FIXED categorical palette (so cache-read is
//! always green, output always orange, …) and structural frames get a muted gray,
//! so the graph reads by color instead of by squinting at truncated labels. A
//! color legend is appended (inferno has no legend, so we post-process its SVG).
//!
//! `--fold` skips the SVG entirely and emits the raw folded text (one
//! `frame;frame;… value` line per stack) to stdout, for piping into other
//! flamegraph tooling or inspection.

use std::io::Write;

use anyhow::Context;
use inferno::flamegraph::color::{Color, PaletteMap};
use inferno::flamegraph::{from_lines, Options};
use skiagram_core::analysis::flame::FlameData;

/// Fixed color per token-type leaf (a categorical, distinct, legible palette).
/// The frame fills AND the legend both read from this single source so the two
/// can never drift. Order here is the canonical legend order.
const TYPE_COLORS: &[(&str, Color)] = &[
    ("input", rgb(0x4e, 0x79, 0xa7)),       // blue
    ("output", rgb(0xf2, 0x8e, 0x2b)),      // orange
    ("cache-read", rgb(0x59, 0xa1, 0x4f)),  // green
    ("cache-write", rgb(0xe1, 0x57, 0x59)), // red
    ("thinking", rgb(0xb0, 0x7a, 0xa1)),    // purple
];

/// `Color` (an `rgb::RGB8`) from its components, usable in `const` context.
const fn rgb(r: u8, g: u8, b: u8) -> Color {
    Color { r, g, b }
}

/// Render `data` as an SVG flamegraph into `out`.
///
/// Writer-generic so it is unit-testable and usable with a `BufWriter<File>`.
/// Token-type frames are colored from [`TYPE_COLORS`] and structural frames from
/// [`structural_gray`]; every frame name present is pre-seeded into inferno's
/// palette map, so coloring is fully deterministic (no RNG, no per-run diff
/// churn) and inferno never falls back to its hash palette.
pub fn write_svg(data: &FlameData, mut out: impl Write, title: &str) -> anyhow::Result<()> {
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
    // Hover detail prefix: inferno's profiler default is "Function:", which is
    // nonsense for a project/session/model/token-type frame. "Frame:" is honest.
    opts.name_type = "Frame:".to_string();
    opts.hash = true; // deterministic fallback for any name we didn't pre-seed

    // Pre-seed a color for every frame name: token-types -> their fixed swatch
    // color, everything structural -> a muted gray. inferno consults the map
    // first (see PaletteMap::find_color_for), so this fully controls the colors.
    let mut palette = PaletteMap::default();
    for stack in &data.stacks {
        for frame in &stack.frames {
            palette.insert(frame.as_str(), frame_color(frame));
        }
    }
    opts.palette_map = Some(&mut palette);

    // Render to a buffer first so we can append a legend (inferno has none).
    let lines: Vec<String> = data.stacks.iter().map(|s| s.to_folded_line()).collect();
    let mut buf = Vec::new();
    from_lines(&mut opts, lines.iter().map(String::as_str), &mut buf)
        .context("rendering flamegraph SVG")?;

    let mut svg = String::from_utf8(buf).context("flamegraph SVG was not UTF-8")?;
    inject_legend(&mut svg, data);

    out.write_all(svg.as_bytes())
        .context("writing flamegraph SVG")?;
    out.flush().context("flushing flamegraph SVG")?;
    Ok(())
}

/// Color for one frame: a fixed token-type swatch, or a muted structural gray.
fn frame_color(frame: &str) -> Color {
    TYPE_COLORS
        .iter()
        .find(|(label, _)| *label == frame)
        .map(|(_, c)| *c)
        .unwrap_or_else(|| structural_gray(frame))
}

/// Deterministic light gray, slightly varied per name so adjacent same-row frames
/// (projects, sessions, models) stay distinguishable without competing with the
/// vivid token-type leaves. FNV-1a hash mapped into a tight band kept clearly
/// below the pale (~#eee) background so the boxes still read.
fn structural_gray(name: &str) -> Color {
    let mut h: u32 = 0x811c_9dc5;
    for b in name.bytes() {
        h ^= u32::from(b);
        h = h.wrapping_mul(0x0100_0193);
    }
    let v = 188 + (h % 36) as u8; // 188..=223
    Color { r: v, g: v, b: v }
}

/// Append a token-type color legend to an inferno SVG.
///
/// inferno has no legend concept, so we post-process its output: grow the canvas
/// by one row and draw a swatch + label for each token-type that ACTUALLY appears
/// in the graph (canonical order). Skipped entirely when there is no token-type
/// level (e.g. `--group-by project,model`), so we never advertise colors that are
/// not on screen. If the SVG isn't shaped the way we expect, we leave it
/// untouched — better no legend than a corrupt file.
fn inject_legend(svg: &mut String, data: &FlameData) {
    let present: Vec<(&str, Color)> = TYPE_COLORS
        .iter()
        .filter(|(label, _)| {
            data.stacks
                .iter()
                .any(|s| s.frames.iter().any(|f| f == label))
        })
        .map(|(l, c)| (*l, *c))
        .collect();
    if present.is_empty() {
        return;
    }

    let Some((w, h)) = parse_viewbox(svg) else {
        return;
    };
    const BAND: u32 = 24; // height of the legend strip
    let new_h = h + BAND;

    // Grow the canvas. The old height appears verbatim only in the root <svg> tag
    // and the background <rect> (frames use height="15"), so replacing it there —
    // plus the viewBox — is safe and complete.
    *svg = svg.replace(&format!("height=\"{h}\""), &format!("height=\"{new_h}\""));
    *svg = svg.replace(
        &format!("viewBox=\"0 0 {w} {h}\""),
        &format!("viewBox=\"0 0 {w} {new_h}\""),
    );

    // Build the legend in the new bottom band. Swatch + label per type; the text
    // inherits inferno's monospace 12px style, so ~7px/char advances cleanly.
    let mut legend = String::from("<g id=\"legend\">");
    let mut x = 10u32;
    let swatch_y = h + 6;
    let text_y = h + 16;
    for (label, c) in &present {
        let fill = format!("rgb({},{},{})", c.r, c.g, c.b);
        legend.push_str(&format!(
            "<rect x=\"{x}\" y=\"{swatch_y}\" width=\"12\" height=\"12\" fill=\"{fill}\"/>"
        ));
        let tx = x + 16;
        legend.push_str(&format!(
            "<text x=\"{tx}\" y=\"{text_y}\" fill=\"rgb(0,0,0)\">{label}</text>"
        ));
        x = tx + (label.len() as u32) * 7 + 18;
    }
    legend.push_str("</g>");

    // Insert as the last child of the root <svg> (just before its closing tag).
    if let Some(pos) = svg.rfind("</svg>") {
        svg.insert_str(pos, &legend);
    }
}

/// Extract `(width, height)` from the first `viewBox="0 0 W H"` in the SVG.
fn parse_viewbox(svg: &str) -> Option<(u32, u32)> {
    const KEY: &str = "viewBox=\"0 0 ";
    let start = svg.find(KEY)? + KEY.len();
    let rest = &svg[start..];
    let end = rest.find('"')?;
    let mut nums = rest[..end].split_whitespace();
    let w: u32 = nums.next()?.parse().ok()?;
    let h: u32 = nums.next()?.parse().ok()?;
    Some((w, h))
}

/// Print the raw folded stacks to stdout, one `frame;frame;… value` line each
/// (for piping into other flamegraph tooling / inspection). Nothing else.
pub fn print_folded(data: &FlameData) {
    for s in &data.stacks {
        println!("{}", s.to_folded_line());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use skiagram_core::analysis::flame::{FlameData, FlameMetric, FoldedStack};

    /// A `FlameData` whose stacks are the given frame paths (each weight 100).
    fn data_with(paths: &[&[&str]]) -> FlameData {
        let stacks = paths
            .iter()
            .map(|p| FoldedStack {
                frames: p.iter().map(|s| s.to_string()).collect(),
                value: 100,
            })
            .collect();
        FlameData {
            agent: "claude-code".into(),
            since: None,
            metric: FlameMetric::Tokens,
            unit: "tokens",
            stacks,
            total_value: 100 * paths.len() as u64,
            unpriced_requests: 0,
        }
    }

    fn render(data: &FlameData) -> String {
        let mut buf = Vec::new();
        write_svg(data, &mut buf, "skiagram — token spend").expect("renders");
        String::from_utf8(buf).expect("utf8 svg")
    }

    /// A graph with a token-type level gets a legend listing exactly the types
    /// present, and the hover prefix is the domain-appropriate "Frame:".
    #[test]
    fn svg_has_token_type_legend_and_fixed_prefix() {
        let svg = render(&data_with(&[
            &["proj", "3e9d2c41", "claude-opus-4-8", "cache-read"],
            &["proj", "3e9d2c41", "claude-opus-4-8", "output"],
        ]));
        assert!(svg.contains("<g id=\"legend\">"), "legend injected");
        // Present types appear in the legend; absent ones don't.
        assert!(svg.contains(">cache-read</text>"));
        assert!(svg.contains(">output</text>"));
        assert!(
            !svg.contains(">thinking</text>"),
            "absent type not in legend"
        );
        // The cache-read swatch uses the fixed green from TYPE_COLORS.
        assert!(svg.contains("fill=\"rgb(89,161,79)\""));
        // Hover prefix fixed (was the misleading "Function:").
        assert!(svg.contains("var nametype = 'Frame:'"));
    }

    /// Dropping the token-type level (e.g. `--group-by project,model`) means no
    /// legend — we never show colors that aren't on the graph.
    #[test]
    fn no_legend_without_a_token_type_level() {
        let svg = render(&data_with(&[&["proj", "claude-opus-4-8"]]));
        assert!(
            !svg.contains("id=\"legend\""),
            "no legend without a type row"
        );
    }

    /// The legend grows the canvas by exactly one band so it doesn't overlap the
    /// frames. Comparing two graphs of the SAME depth — one with a token-type
    /// leaf (legend) and one without (no legend) — isolates the band height.
    #[test]
    fn legend_grows_canvas_by_one_band() {
        let with_legend = render(&data_with(&[&["proj", "input"]]));
        let no_legend = render(&data_with(&[&["proj", "other"]]));
        let (_, h1) = parse_viewbox(&with_legend).expect("viewBox present");
        let (_, h0) = parse_viewbox(&no_legend).expect("viewBox present");
        assert_eq!(h1, h0 + 24, "legend adds a fixed 24px band");
        assert!(with_legend.contains("<g id=\"legend\">"));
        assert!(!no_legend.contains("id=\"legend\""));
        assert!(with_legend.trim_end().ends_with("</svg>"));
    }
}
