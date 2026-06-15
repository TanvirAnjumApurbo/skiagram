//! Output rendering: human tables, machine JSON, (later) flamegraph SVG.

pub mod anomalies;
pub mod flame;
pub mod json;
pub mod table;

/// `1234567` -> `"1,234,567"`.
pub(crate) fn fmt_count(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// Dollar formatting with enough precision for sub-cent CLI sessions.
pub(crate) fn fmt_cost(cost: f64) -> String {
    if cost >= 1.0 {
        format!("${cost:.2}")
    } else {
        format!("${cost:.4}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_get_thousands_separators() {
        assert_eq!(fmt_count(0), "0");
        assert_eq!(fmt_count(999), "999");
        assert_eq!(fmt_count(18080), "18,080");
        assert_eq!(fmt_count(1_234_567), "1,234,567");
    }
}
