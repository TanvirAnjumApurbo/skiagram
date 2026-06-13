//! Token-accounting analysis passes over the normalized model.

pub mod aggregate;
pub mod anomaly;
pub mod classify;
pub mod context;
pub mod dedup;

use jiff::civil::Date;
use jiff::{tz::TimeZone, Timestamp};

/// Civil date used for day bucketing and `--since`.
///
/// UTC on purpose: deterministic across machines/timezones. TODO(scope):
/// local-timezone day option.
pub(crate) fn utc_date(ts: Timestamp) -> Date {
    ts.to_zoned(TimeZone::UTC).date()
}

/// Rough chars-per-token ratio for English/code. ONLY used for heuristics and
/// the context-bloat ESTIMATE — NEVER for billing (billing always uses the
/// agent-reported token counts; CLAUDE.md §8.7).
pub(crate) const EST_CHARS_PER_TOKEN: u64 = 4;

/// Estimate tokens from a char count. Estimate only — never a billed figure.
pub(crate) fn est_tokens(chars: u64) -> u64 {
    chars / EST_CHARS_PER_TOKEN
}
