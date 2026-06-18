//! Pixel "skiagram" wordmark + slogan, shown on bare `skiagram`, the first
//! interactive run, and atop the TUI.
//!
//! Lowercase letterforms inspired by **Architype Ingenieur Dot**, rendered as a
//! grid of braille `⣿` characters for a pixel/LED-terminal look (rather than the
//! seamless strokes a full block would give) — a terminal can't ship that TrueType
//! face, so we emulate it with braille cells. Colored with a red→orange gradient.
//! The art is hand-built (no font dependency), preserving the single-binary edge
//! (CLAUDE.md §12). Color is raw ANSI on the CLI paths here, and rebuilt as
//! `ratatui` spans in the TUI (which consumes [`wordmark_rows`] + [`gradient_rgb`]).
//!
//! Gating — so we never bloat or corrupt output, the whole point of this tool:
//! color only on a TTY and only when `$NO_COLOR` is unset; the block art degrades
//! to a one-line wordmark when the terminal is too narrow to hold it.

use std::io::{IsTerminal, Write};

/// The slogan, shown under the wordmark everywhere.
pub const TAGLINE: &str = "The flamegraph for your AI agent's token spend";

/// The lit cell — a full braille block (U+28FF) so the wordmark reads as a grid
/// of dots rather than seamless strokes. Treated as single width by terminals.
const PIXEL: &str = "⣿";

// Lowercase pixel glyphs on an 11-row grid with 2-cell-thick strokes for weight:
// rows 0–1 ascender (k stem, i dot), rows 2–8 x-height (baseline at row 8), rows
// 9–10 descender (g). `X` is a lit cell, space is unlit. Every row of a glyph is
// padded to that glyph's fixed width so the assembled wordmark stays aligned.
const G_S: [&str; 11] = [
    "      ", "      ", "XXXXXX", "XX    ", "XX    ", "XXXXXX", "    XX", "    XX", "XXXXXX",
    "      ", "      ",
];
const G_K: [&str; 11] = [
    "XX    ", "XX    ", "XX  XX", "XX XX ", "XXXX  ", "XXXX  ", "XX XX ", "XX  XX", "XX  XX",
    "      ", "      ",
];
const G_I: [&str; 11] = [
    "XX", "XX", "  ", "XX", "XX", "XX", "XX", "XX", "XX", "  ", "  ",
];
const G_A: [&str; 11] = [
    "      ", "      ", " XXXX ", "XX  XX", "XX  XX", "XX  XX", "XX  XX", "XX  XX", " XXXXX",
    "      ", "      ",
];
const G_G: [&str; 11] = [
    "      ", "      ", " XXXX ", "XX  XX", "XX  XX", "XX  XX", "XX  XX", "XX  XX", " XXXXX",
    "    XX", "XXXXX ",
];
const G_R: [&str; 11] = [
    "     ", "     ", "XX XX", "XXXX ", "XX   ", "XX   ", "XX   ", "XX   ", "XX   ", "     ",
    "     ",
];
const G_M: [&str; 11] = [
    "        ", "        ", "XXXXXXXX", "XX XX XX", "XX XX XX", "XX XX XX", "XX XX XX", "XX XX XX",
    "XX XX XX", "        ", "        ",
];

/// The glyphs spelling s·k·i·a·g·r·a·m, in order.
const GLYPHS: &[[&str; 11]] = &[G_S, G_K, G_I, G_A, G_G, G_R, G_A, G_M];

/// Height of the block wordmark, in rows.
pub const WORDMARK_ROWS: usize = 11;

/// The wordmark as full-width rows of blocks/spaces, letters joined by one column.
/// Both the CLI gradient and the TUI spans build on these exact strings.
pub(crate) fn wordmark_rows() -> Vec<String> {
    (0..WORDMARK_ROWS)
        .map(|r| {
            GLYPHS
                .iter()
                .map(|g| g[r])
                .collect::<Vec<_>>()
                .join(" ")
                .replace('X', PIXEL)
        })
        .collect()
}

/// Width of the wordmark in columns (characters, not bytes — `⣿` is 3 bytes).
pub(crate) fn wordmark_width() -> usize {
    wordmark_rows().first().map_or(0, |r| r.chars().count())
}

/// Red→orange gradient with `t` in `[0, 1]` across the logo's width (left red,
/// right orange) — matching the requested Architype-Ingenieur-Dot rendering.
pub(crate) fn gradient_rgb(t: f32) -> (u8, u8, u8) {
    const RED: (f32, f32, f32) = (255.0, 80.0, 80.0); // left
    const ORANGE: (f32, f32, f32) = (255.0, 180.0, 50.0); // right
    let t = t.clamp(0.0, 1.0);
    let lerp = |x: f32, y: f32| (x + (y - x) * t).round() as u8;
    (
        lerp(RED.0, ORANGE.0),
        lerp(RED.1, ORANGE.1),
        lerp(RED.2, ORANGE.2),
    )
}

/// One art row as a printable string: a per-cell truecolor gradient when `color`,
/// else the plain blocks (byte-identical to the source). Spaces stay uncolored.
fn ansi_row(row: &str, width: usize, color: bool) -> String {
    if !color {
        return row.to_string();
    }
    let mut s = String::with_capacity(row.len() + width * 22);
    for (i, ch) in row.chars().enumerate() {
        if ch == ' ' {
            s.push(' ');
            continue;
        }
        let t = if width <= 1 {
            0.0
        } else {
            i as f32 / (width - 1) as f32
        };
        let (r, g, b) = gradient_rgb(t);
        s.push_str(&format!("\x1b[38;2;{r};{g};{b}m{ch}\x1b[0m"));
    }
    s
}

/// Emit color on a stream that `is_tty`? Honors `$NO_COLOR` (any value disables).
fn use_color(is_tty: bool) -> bool {
    is_tty && std::env::var_os("NO_COLOR").is_none()
}

/// Terminal width if known (via crossterm), else `None` (not a TTY / query failed).
fn term_width() -> Option<usize> {
    ratatui::crossterm::terminal::size()
        .ok()
        .map(|(cols, _)| cols as usize)
}

/// Is the terminal wide enough for the block art? `None` width (piped/redirected)
/// counts as wide enough — write the full art.
fn width_ok() -> bool {
    term_width().is_none_or(|w| w > wordmark_width())
}

/// Write the wordmark + tagline to `w`. `color` toggles the gradient; `width_ok`
/// chooses the block art vs the one-line compact fallback.
fn write_banner(w: &mut impl Write, color: bool, width_ok: bool) -> std::io::Result<()> {
    if width_ok {
        let width = wordmark_width();
        for row in wordmark_rows() {
            writeln!(w, "{}", ansi_row(&row, width, color))?;
        }
    } else {
        // Narrow terminal: just the styled name on one line.
        let name = if color {
            "\x1b[38;2;255;80;80m\x1b[1mskiagram\x1b[0m".to_string()
        } else {
            "skiagram".to_string()
        };
        writeln!(w, "{name}")?;
    }
    if color {
        writeln!(w, "\x1b[2;3m{TAGLINE}\x1b[0m") // dim + italic
    } else {
        writeln!(w, "{TAGLINE}")
    }
}

/// The short command list under the welcome banner.
fn write_commands(w: &mut impl Write) -> std::io::Result<()> {
    writeln!(w, "USAGE")?;
    for (cmd, desc) in [
        ("summary", "Token + cost summary (deduplicated)"),
        ("context", "What is filling your context window"),
        ("anomalies", "Fat-tail requests + retry storms"),
        ("classify", "Spend broken down by task type"),
        ("flame", "Export a flamegraph SVG of token spend"),
        ("tui", "Interactive drill-down browser"),
        ("watch", "Live-tail: re-render the summary on change"),
    ] {
        writeln!(w, "  skiagram {cmd:<10} {desc}")?;
    }
    writeln!(w)?;
    writeln!(
        w,
        "Run `skiagram <command> --help` for options. Everything stays on this machine."
    )
}

/// Bare `skiagram`: the full welcome screen to stdout — wordmark, slogan, and the
/// command list. Color/width auto-detected; best-effort (never errors the program).
pub fn print_welcome() {
    let stdout = std::io::stdout();
    let color = use_color(stdout.is_terminal());
    let wide = width_ok();
    let mut out = stdout.lock();
    let _ = write_banner(&mut out, color, wide);
    let _ = writeln!(out);
    let _ = write_commands(&mut out);
}

/// First interactive run: greet on stderr (so stdout stays clean for the command
/// that follows), then let the command proceed. Called only when stderr is a TTY.
pub fn print_first_run_greeting() {
    let stderr = std::io::stderr();
    let color = use_color(stderr.is_terminal());
    let wide = width_ok();
    let mut err = stderr.lock();
    let _ = write_banner(&mut err, color, wide);
    let _ = writeln!(err, "\nThanks for installing skiagram!");
    let _ = writeln!(
        err,
        "Run `skiagram` with no arguments anytime to see all commands.\n"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wordmark_rows_are_uniform_and_complete() {
        let rows = wordmark_rows();
        assert_eq!(rows.len(), WORDMARK_ROWS);
        let w = rows[0].chars().count();
        assert!(
            rows.iter().all(|r| r.chars().count() == w),
            "every row is the same width"
        );
        assert_eq!(w, wordmark_width());
        // s6 k6 i2 a6 g6 r5 a6 m8 = 45 cells + 7 single-column gaps.
        assert_eq!(w, 45 + 7);
    }

    #[test]
    fn rows_are_braille_pixels() {
        // The style is braille pixels: lit cells are `⣿`, never blocks/dots/marker.
        let joined = wordmark_rows().join("\n");
        assert!(joined.contains('⣿'));
        assert!(!joined.contains('■'));
        assert!(!joined.contains('█'));
        assert!(!joined.contains('●'));
        assert!(!joined.contains('X'), "template marker fully substituted");
    }

    #[test]
    fn gradient_runs_red_to_orange() {
        assert_eq!(gradient_rgb(0.0), (255, 80, 80)); // red, left
        assert_eq!(gradient_rgb(1.0), (255, 180, 50)); // orange, right
        assert_eq!(gradient_rgb(0.5), (255, 130, 65)); // midpoint
                                                       // Out-of-range clamps rather than wrapping.
        assert_eq!(gradient_rgb(-1.0), gradient_rgb(0.0));
        assert_eq!(gradient_rgb(2.0), gradient_rgb(1.0));
    }

    #[test]
    fn plain_rows_have_no_escapes_colored_do() {
        let row = &wordmark_rows()[2]; // an x-height row, plenty of blocks
        let w = wordmark_width();
        assert!(!ansi_row(row, w, false).contains('\x1b'));
        assert!(ansi_row(row, w, true).contains('\x1b'));
        // The no-color path must be byte-identical to the source blocks.
        assert_eq!(ansi_row(row, w, false), *row);
    }

    #[test]
    fn welcome_body_contains_slogan_and_commands() {
        let mut buf: Vec<u8> = Vec::new();
        write_banner(&mut buf, false, true).unwrap();
        write_commands(&mut buf).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(s.contains(TAGLINE));
        assert!(s.contains("skiagram summary"));
        assert!(s.contains("skiagram tui"));
    }

    #[test]
    fn narrow_fallback_is_name_plus_tagline() {
        let mut buf: Vec<u8> = Vec::new();
        write_banner(&mut buf, false, false).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(s.contains("skiagram"));
        assert!(s.contains(TAGLINE));
        // Compact = name line + tagline line, nothing more.
        assert_eq!(s.lines().count(), 2);
    }
}
