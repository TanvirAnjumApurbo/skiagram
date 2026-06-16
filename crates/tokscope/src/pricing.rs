//! Binary-side pricing: assemble the [`PricingTable`] the analysis passes use, and
//! (behind the off-by-default `network` feature) refresh model prices from LiteLLM.
//!
//! The embedded snapshot in `tokscope-core` is the always-present default. This
//! layer only ADDS overrides on top — from a locally cached refresh — so a default
//! build with no cache behaves exactly as before (every existing cost is
//! unchanged). Reading the cache is offline and always compiled; only the refresh
//! that WRITES it needs the network, which the `network` feature gates so the
//! standard build has zero network code (CLAUDE.md §12). Provenance is recorded in
//! the cache so every refreshed figure still traces to a source (§8.7).
//!
//! Cache path: `$TOKSCOPE_PRICING_CACHE`, else `<config_dir>/tokscope/pricing-cache.json`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};
use tokscope_core::pricing::{ModelPricing, PricingTable};

/// Upstream price table (BerriAI/LiteLLM mirror of public pricing — CLAUDE.md §7).
#[cfg(feature = "network")]
const LITELLM_URL: &str =
    "https://raw.githubusercontent.com/BerriAI/litellm/main/model_prices_and_context_window.json";

/// Resolve the refresh-cache path (`$TOKSCOPE_PRICING_CACHE` overrides, also the
/// test hook), else `<config_dir>/tokscope/pricing-cache.json`.
fn cache_path() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("TOKSCOPE_PRICING_CACHE") {
        if !p.trim().is_empty() {
            return Some(PathBuf::from(p));
        }
    }
    directories::ProjectDirs::from("", "", "tokscope")
        .map(|d| d.config_dir().join("pricing-cache.json"))
}

/// The on-disk refresh cache: provenance + per-model unit prices in USD per million
/// tokens (the same units as [`ModelPricing`]).
#[derive(Debug, Default, Serialize, Deserialize)]
struct PricingCache {
    /// Where the prices came from (so a cost figure always traces to a source).
    source: String,
    /// When they were fetched (RFC 3339).
    refreshed_at: String,
    /// model-id -> price.
    models: BTreeMap<String, CachedPrice>,
}

/// One model's unit prices, USD per million tokens.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CachedPrice {
    input: f64,
    output: f64,
    cache_read: f64,
    cache_write_5m: f64,
    cache_write_1h: f64,
}

impl From<CachedPrice> for ModelPricing {
    fn from(c: CachedPrice) -> Self {
        ModelPricing {
            input: c.input,
            output: c.output,
            cache_read: c.cache_read,
            cache_write_5m: c.cache_write_5m,
            cache_write_1h: c.cache_write_1h,
        }
    }
}

/// Build the pricing table for this run: the embedded snapshot plus any cached
/// refresh overrides. When `refresh` is set, fetch fresh prices first (network
/// feature), writing the cache and reporting to stderr; a refresh failure is
/// non-fatal — reading your spend must not depend on the network.
pub fn build_pricing(refresh: bool) -> Result<PricingTable> {
    if refresh {
        match refresh_now() {
            Ok(report) => eprintln!("{report}"),
            Err(e) => {
                eprintln!("warning: --refresh-pricing failed ({e:#}); using embedded/cached prices")
            }
        }
    }
    Ok(load_table())
}

/// Embedded snapshot plus cached overrides when a parseable cache is present.
fn load_table() -> PricingTable {
    match cache_path().and_then(|p| load_overrides(&p)) {
        Some(ov) if !ov.is_empty() => PricingTable::with_overrides(ov),
        _ => PricingTable::embedded(),
    }
}

/// Read overrides from a cache file; `None` on absence/parse error (degrade to the
/// embedded snapshot rather than failing — mirrors the lenient config loader).
fn load_overrides(path: &Path) -> Option<Vec<(String, ModelPricing)>> {
    let text = std::fs::read_to_string(path).ok()?;
    let cache: PricingCache = serde_json::from_str(&text).ok()?;
    Some(
        cache
            .models
            .into_iter()
            .map(|(k, v)| (k, v.into()))
            .collect(),
    )
}

/// Fetch + parse + cache fresh prices, returning a one-line human report.
#[cfg(feature = "network")]
fn refresh_now() -> Result<String> {
    use anyhow::Context;

    let body = ureq::get(LITELLM_URL)
        .call()
        .context("fetching LiteLLM prices")?
        .into_string()
        .context("reading LiteLLM response")?;
    let cache = parse_litellm(&body)?;
    let n = cache.models.len();
    let path = cache_path().context("locating the config dir for the price cache")?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    std::fs::write(&path, serde_json::to_string_pretty(&cache)?)
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(format!(
        "refreshed pricing: {n} models from LiteLLM -> {}",
        path.display()
    ))
}

/// Without the `network` feature there is no fetch path at all (offline default).
#[cfg(not(feature = "network"))]
fn refresh_now() -> Result<String> {
    anyhow::bail!(
        "this build has no network support; rebuild with `cargo build --features network` \
         to use --refresh-pricing (the embedded snapshot stays the offline default)"
    )
}

/// Parse the LiteLLM JSON into our cache shape. LiteLLM costs are per-token; we
/// store per-million (×1e6) to match [`ModelPricing`]. When LiteLLM doesn't break
/// out the 1h cache-write TTL, it is derived from the 5m write via Anthropic's
/// published ratio (1h = 2× input, 5m = 1.25× input ⇒ 1h = 1.6× 5m) — documented so
/// every figure still traces to a published price (§8.7).
#[cfg(feature = "network")]
fn parse_litellm(body: &str) -> Result<PricingCache> {
    use anyhow::Context;
    use serde_json::Value;

    let root: Value = serde_json::from_str(body).context("parsing LiteLLM JSON")?;
    let obj = root.as_object().context("LiteLLM JSON is not an object")?;
    let per_m = |v: &Value, key: &str| v.get(key).and_then(Value::as_f64).map(|c| c * 1e6);

    let mut models = BTreeMap::new();
    for (name, v) in obj {
        // Only models we can actually cost (need input + output token prices).
        let (Some(input), Some(output)) = (
            per_m(v, "input_cost_per_token"),
            per_m(v, "output_cost_per_token"),
        ) else {
            continue;
        };
        let cache_read = per_m(v, "cache_read_input_token_cost").unwrap_or(input * 0.1);
        let cache_write_5m = per_m(v, "cache_creation_input_token_cost").unwrap_or(input * 1.25);
        models.insert(
            name.clone(),
            CachedPrice {
                input,
                output,
                cache_read,
                cache_write_5m,
                cache_write_1h: cache_write_5m * 1.6,
            },
        );
    }
    if models.is_empty() {
        anyhow::bail!("LiteLLM response contained no priceable models");
    }
    Ok(PricingCache {
        source: LITELLM_URL.to_string(),
        refreshed_at: jiff::Timestamp::now().to_string(),
        models,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_cache(name: &str, body: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("tokscope-price-{}-{name}", std::process::id()));
        std::fs::File::create(&p)
            .unwrap()
            .write_all(body.as_bytes())
            .unwrap();
        p
    }

    #[test]
    fn missing_cache_yields_embedded_table() {
        let ov = cache_path()
            .filter(|p| p.exists())
            .and_then(|p| load_overrides(&p));
        // With no TOKSCOPE_PRICING_CACHE set and (normally) no cache file, the table
        // is the embedded snapshot.
        let table = match ov {
            Some(o) if !o.is_empty() => PricingTable::with_overrides(o),
            _ => PricingTable::embedded(),
        };
        // Embedded snapshot still prices a known model and refuses an unknown one.
        assert!(table
            .cost_usd(Some("claude-sonnet-4-5"), &Default::default())
            .is_some());
        assert!(table.lookup("gpt-5.5").is_none());
    }

    #[test]
    fn cache_overrides_apply_and_price_a_new_model() {
        // A normally-unpriced model (gpt-5.5) becomes priced via a cache override.
        let json = r#"{
            "source": "test",
            "refreshed_at": "2026-06-16T00:00:00Z",
            "models": {
                "gpt-5.5": { "input": 1.0, "output": 2.0, "cache_read": 0.1, "cache_write_5m": 1.25, "cache_write_1h": 2.0 }
            }
        }"#;
        let p = write_cache("ov.json", json);
        let ov = load_overrides(&p).expect("overrides parse");
        let table = PricingTable::with_overrides(ov);

        let usage = tokscope_core::model::Usage {
            input: Some(1_000_000),
            output: Some(1_000_000),
            ..Default::default()
        };
        // 1.0 + 2.0 per million each = $3.00, where the embedded snapshot had nothing.
        let cost = table.cost_usd(Some("gpt-5.5"), &usage).expect("now priced");
        assert!((cost - 3.0).abs() < 1e-9, "got {cost}");
        // The embedded snapshot still wins for models not in the override set.
        assert!(table.lookup("claude-sonnet-4-5").is_some());
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn malformed_cache_is_ignored() {
        let p = write_cache("bad.json", "{ not valid json ");
        assert!(load_overrides(&p).is_none());
        let _ = std::fs::remove_file(&p);
    }
}
