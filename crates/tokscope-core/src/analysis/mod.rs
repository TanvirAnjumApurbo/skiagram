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
