//! Model pricing: embedded snapshot, USD per **million** tokens.
//!
//! Source: Anthropic public pricing as mirrored by LiteLLM's
//! `model_prices_and_context_window.json` — manually curated subset, snapshot
//! taken 2026-06. Cache rates are Anthropic's published multipliers materialized
//! as absolute numbers: read = 0.1x input, 5m write = 1.25x input, 1h write = 2x
//! input.
//!
//! RULES (CLAUDE.md §8):
//! - cache-read and cache-creation are priced separately (§8.4), and the 5m/1h
//!   write TTLs differ too — never lump them.
//! - models NOT in this table are never guessed at; they surface as "unpriced"
//!   (e.g. non-Anthropic models like `gpt-*`/`gemini-*`, or a future Claude
//!   generation past this snapshot — a bare numeric suffix like `claude-opus-4-9`
//!   must NOT inherit `claude-opus-4`'s price).
//! - every cost figure traces to (model, token type, unit price) via this table —
//!   no magic numbers anywhere else (§8.7).
//!
//! TODO(scope): optional refresh from LiteLLM behind a `--refresh-pricing` flag /
//! `network` cargo feature, OFF by default (local-first, CLAUDE.md §12).

use crate::model::Usage;

/// USD per 1,000,000 tokens.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ModelPricing {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write_5m: f64,
    pub cache_write_1h: f64,
}

/// Embedded pricing snapshot (2026-06). Keys are model-id prefixes; a dated
/// release suffix (`-20250929` / `@20250929`) is accepted, a minor-version
/// suffix is not (so `claude-opus-4-8` does NOT silently price as
/// `claude-opus-4`).
pub const SNAPSHOT: &[(&str, ModelPricing)] = &[
    (
        "claude-fable-5",
        ModelPricing {
            input: 10.0,
            output: 50.0,
            cache_read: 1.0,
            cache_write_5m: 12.50,
            cache_write_1h: 20.0,
        },
    ),
    (
        "claude-mythos-5",
        ModelPricing {
            input: 10.0,
            output: 50.0,
            cache_read: 1.0,
            cache_write_5m: 12.50,
            cache_write_1h: 20.0,
        },
    ),
    (
        "claude-sonnet-4-6",
        ModelPricing {
            input: 3.0,
            output: 15.0,
            cache_read: 0.30,
            cache_write_5m: 3.75,
            cache_write_1h: 6.0,
        },
    ),
    (
        "claude-sonnet-4-5",
        ModelPricing {
            input: 3.0,
            output: 15.0,
            cache_read: 0.30,
            cache_write_5m: 3.75,
            cache_write_1h: 6.0,
        },
    ),
    (
        "claude-sonnet-4",
        ModelPricing {
            input: 3.0,
            output: 15.0,
            cache_read: 0.30,
            cache_write_5m: 3.75,
            cache_write_1h: 6.0,
        },
    ),
    (
        "claude-3-7-sonnet",
        ModelPricing {
            input: 3.0,
            output: 15.0,
            cache_read: 0.30,
            cache_write_5m: 3.75,
            cache_write_1h: 6.0,
        },
    ),
    (
        "claude-opus-4-8",
        ModelPricing {
            input: 5.0,
            output: 25.0,
            cache_read: 0.50,
            cache_write_5m: 6.25,
            cache_write_1h: 10.0,
        },
    ),
    (
        "claude-opus-4-7",
        ModelPricing {
            input: 5.0,
            output: 25.0,
            cache_read: 0.50,
            cache_write_5m: 6.25,
            cache_write_1h: 10.0,
        },
    ),
    (
        "claude-opus-4-6",
        ModelPricing {
            input: 5.0,
            output: 25.0,
            cache_read: 0.50,
            cache_write_5m: 6.25,
            cache_write_1h: 10.0,
        },
    ),
    (
        "claude-opus-4-5",
        ModelPricing {
            input: 5.0,
            output: 25.0,
            cache_read: 0.50,
            cache_write_5m: 6.25,
            cache_write_1h: 10.0,
        },
    ),
    (
        "claude-opus-4-1",
        ModelPricing {
            input: 15.0,
            output: 75.0,
            cache_read: 1.50,
            cache_write_5m: 18.75,
            cache_write_1h: 30.0,
        },
    ),
    (
        "claude-opus-4",
        ModelPricing {
            input: 15.0,
            output: 75.0,
            cache_read: 1.50,
            cache_write_5m: 18.75,
            cache_write_1h: 30.0,
        },
    ),
    (
        "claude-haiku-4-5",
        ModelPricing {
            input: 1.0,
            output: 5.0,
            cache_read: 0.10,
            cache_write_5m: 1.25,
            cache_write_1h: 2.0,
        },
    ),
    (
        "claude-3-5-haiku",
        ModelPricing {
            input: 0.80,
            output: 4.0,
            cache_read: 0.08,
            cache_write_5m: 1.0,
            cache_write_1h: 1.6,
        },
    ),
];

/// Find the price for a model id, tolerating provider prefixes
/// (`anthropic/...`) and dated release suffixes (`-20250929`, `@20250929`).
/// Returns `None` for unknown models — callers must surface that, not guess.
pub fn lookup(model: &str) -> Option<&'static ModelPricing> {
    let m = normalize_model(model);
    SNAPSHOT
        .iter()
        .filter(|(key, _)| key_matches(&m, key))
        .max_by_key(|(key, _)| key.len()) // longest prefix wins (sonnet-4-5 over sonnet-4)
        .map(|(_, p)| p)
}

/// Lower-case and strip provider prefixes (`anthropic/`, `us.anthropic.`,
/// `anthropic.`) so matching is provider-agnostic. Shared by [`lookup`] and
/// [`PricingTable`] so embedded and override matching behave identically.
fn normalize_model(model: &str) -> String {
    let mut m = model.trim().to_ascii_lowercase();
    for prefix in ["anthropic/", "us.anthropic.", "anthropic."] {
        if let Some(rest) = m.strip_prefix(prefix) {
            m = rest.to_string();
            break;
        }
    }
    m
}

/// Exact match, or `<key>-YYYYMMDD` / `<key>@YYYYMMDD`. A bare numeric suffix
/// like `-8` is a DIFFERENT model generation and must not match.
/// TODO(scope): Bedrock-style `...-v1:0` suffixes are not recognized yet.
fn key_matches(model: &str, key: &str) -> bool {
    match model.strip_prefix(key) {
        None => false,
        Some("") => true,
        Some(rest) => {
            (rest.starts_with('-') || rest.starts_with('@'))
                && rest.len() >= 9
                && rest[1..].chars().all(|c| c.is_ascii_digit())
        }
    }
}

/// Cost of one request's usage in USD, or `None` when the model is unknown /
/// unpriced. Unknown usage fields contribute nothing (absence ≠ zero — the
/// result is a lower bound, and aggregation reports incompleteness separately).
pub fn cost_usd(model: Option<&str>, usage: &Usage) -> Option<f64> {
    Some(price_usage(lookup(model?)?, usage))
}

/// USD for one request's `usage` at the given unit prices. Pure arithmetic shared
/// by the free [`cost_usd`] and [`PricingTable::cost_usd`] so embedded and
/// overridden pricing never diverge in how a request is costed (CLAUDE.md §8.7).
fn price_usage(p: &ModelPricing, usage: &Usage) -> f64 {
    let per_m = |tokens: Option<u64>, rate: f64| tokens.unwrap_or(0) as f64 * rate / 1e6;

    let mut cost = per_m(usage.input, p.input)
        + per_m(usage.output, p.output)
        // Thinking tokens bill at the output rate when an agent reports them.
        + per_m(usage.thinking, p.output)
        + per_m(usage.cache_read, p.cache_read);

    // Cache writes: use the per-TTL split when reported; otherwise assume the
    // 5m (default) rate — the cheaper one, so unsplit totals stay a lower bound.
    cost += match (usage.cache_creation_5m, usage.cache_creation_1h) {
        (None, None) => per_m(usage.cache_creation, p.cache_write_5m),
        (m5, h1) => per_m(m5, p.cache_write_5m) + per_m(h1, p.cache_write_1h),
    };
    cost
}

/// A pricing lookup layering optional runtime `overrides` over the embedded
/// [`SNAPSHOT`]. Pure and owned — no global state (CLAUDE.md §9). The binary builds
/// one (from the `--refresh-pricing` cache / config) and threads it into the
/// analysis passes; [`PricingTable::embedded`] is byte-for-byte identical to the
/// free [`lookup`] / [`cost_usd`], so an empty table never changes a number.
#[derive(Debug, Clone, Default)]
pub struct PricingTable {
    /// `(model-id-prefix, price)`, matched with the same prefix / longest-wins rule
    /// as the snapshot and consulted BEFORE it (so an override wins on a tie).
    overrides: Vec<(String, ModelPricing)>,
}

impl PricingTable {
    /// Snapshot only — no overrides (identical to the free functions).
    pub fn embedded() -> Self {
        Self::default()
    }

    /// Snapshot plus the given overrides (keys lower-cased to match
    /// [`normalize_model`]). Overrides take precedence over the snapshot.
    pub fn with_overrides(overrides: Vec<(String, ModelPricing)>) -> Self {
        let overrides = overrides
            .into_iter()
            .map(|(k, p)| (k.trim().to_ascii_lowercase(), p))
            .collect();
        Self { overrides }
    }

    /// Number of override entries (for the refresh report).
    pub fn override_count(&self) -> usize {
        self.overrides.len()
    }

    /// Look up a model: overrides first (longest-prefix wins), else the embedded
    /// snapshot. `None` is still surfaced for unknown models, never guessed.
    pub fn lookup(&self, model: &str) -> Option<&ModelPricing> {
        if !self.overrides.is_empty() {
            let m = normalize_model(model);
            if let Some((_, p)) = self
                .overrides
                .iter()
                .filter(|(key, _)| key_matches(&m, key))
                .max_by_key(|(key, _)| key.len())
            {
                return Some(p);
            }
        }
        lookup(model)
    }

    /// Cost of one request's usage under this table, or `None` when unpriced.
    pub fn cost_usd(&self, model: Option<&str>, usage: &Usage) -> Option<f64> {
        Some(price_usage(self.lookup(model?)?, usage))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dated_release_suffixes_match() {
        assert!(lookup("claude-sonnet-4-5-20250929").is_some());
        assert!(lookup("claude-haiku-4-5-20251001").is_some());
        assert!(lookup("anthropic/claude-sonnet-4-5").is_some());
        // Longest prefix wins: 4-5 pricing, not 4.
        assert_eq!(lookup("claude-sonnet-4-5").map(|p| p.input), Some(3.0));
    }

    #[test]
    fn unknown_models_are_never_guessed() {
        // A newer generation must NOT silently take an existing generation's price
        // (a bare numeric suffix is a different model, even when the base exists).
        assert!(lookup("claude-opus-4-9").is_none()); // not claude-opus-4
        assert!(lookup("claude-opus-5-0").is_none());
        assert!(lookup("claude-fable-6").is_none());
        assert!(lookup("<synthetic>").is_none());
        assert!(lookup("gpt-yolo").is_none());
    }

    #[test]
    fn current_models_from_official_snapshot_are_priced() {
        // The 2026-06 lineup pasted from Anthropic's published table. Asserting the
        // exact input also guards the generation boundary: opus-4-8 is $5 input, so
        // if it wrongly matched claude-opus-4 it would read $15 instead.
        assert_eq!(lookup("claude-opus-4-8").map(|p| p.input), Some(5.0));
        assert_eq!(lookup("claude-opus-4-7").map(|p| p.input), Some(5.0));
        assert_eq!(lookup("claude-opus-4-6").map(|p| p.input), Some(5.0));
        assert_eq!(lookup("claude-sonnet-4-6").map(|p| p.output), Some(15.0));
        assert_eq!(lookup("claude-fable-5").map(|p| p.output), Some(50.0));
        assert_eq!(lookup("claude-mythos-5").map(|p| p.input), Some(10.0));
        // Cache rates follow Anthropic's multipliers (fable-5: 0.1x/1.25x/2x of $10).
        let fable = lookup("claude-fable-5").unwrap();
        assert_eq!(fable.cache_read, 1.0);
        assert_eq!(fable.cache_write_5m, 12.50);
        assert_eq!(fable.cache_write_1h, 20.0);
    }

    #[test]
    fn cache_ttls_price_differently() {
        let split = Usage {
            cache_creation: Some(1_000_000),
            cache_creation_5m: Some(0),
            cache_creation_1h: Some(1_000_000),
            ..Usage::default()
        };
        // 1h write on sonnet-4-5 = $6/M, not the 5m $3.75/M.
        let cost = cost_usd(Some("claude-sonnet-4-5"), &split).unwrap();
        assert!((cost - 6.0).abs() < 1e-9, "got {cost}");

        let unsplit = Usage {
            cache_creation: Some(1_000_000),
            ..Usage::default()
        };
        let cost = cost_usd(Some("claude-sonnet-4-5"), &unsplit).unwrap();
        assert!(
            (cost - 3.75).abs() < 1e-9,
            "unsplit assumes 5m rate, got {cost}"
        );
    }

    #[test]
    fn cost_traces_to_unit_prices() {
        let usage = Usage {
            input: Some(1_000_000),
            output: Some(1_000_000),
            cache_read: Some(1_000_000),
            ..Usage::default()
        };
        let cost = cost_usd(Some("claude-haiku-4-5"), &usage).unwrap();
        assert!((cost - (1.0 + 5.0 + 0.10)).abs() < 1e-9);
        assert_eq!(cost_usd(None, &usage), None);
    }
}
