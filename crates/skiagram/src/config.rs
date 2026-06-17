//! User configuration: `<config-dir>/skiagram/config.toml` (per-OS via `directories`).
//!
//! Optional and lenient by design: a missing file means "all defaults", and a
//! malformed file is logged (`tracing::warn`) and treated as defaults rather than
//! aborting — reading your spend report must never fail because of a config typo.
//! Nothing here reaches the network or writes anything (local-first, CLAUDE.md §12).
//!
//! Resolution order for the file path: `$SKIAGRAM_CONFIG` (explicit override, also
//! the test hook), else `<config_dir>/skiagram/config.toml`.
//!
//! TODO(scope): `[pricing]` overrides + extra data dirs land with `--refresh-pricing`
//! (they need the core pricing-table plumbing); only `default_agent` is wired today.

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Parsed `config.toml`. Every field is optional and unknown keys are ignored, so a
/// newer config never breaks an older binary and vice-versa.
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Agent to read when `--agent` is not passed (and before auto-detection).
    /// `--agent` on the command line always wins over this.
    pub default_agent: Option<String>,
}

impl Config {
    /// `<config_dir>/skiagram/config.toml` for this OS, or `None` when the config
    /// directory cannot be determined.
    pub fn default_path() -> Option<PathBuf> {
        directories::ProjectDirs::from("", "", "skiagram")
            .map(|d| d.config_dir().join("config.toml"))
    }

    /// Load configuration, honoring `$SKIAGRAM_CONFIG` then the standard path, and
    /// falling back to defaults on any absence/error (see module docs).
    pub fn load() -> Config {
        if let Ok(p) = std::env::var("SKIAGRAM_CONFIG") {
            if !p.trim().is_empty() {
                return Self::load_from(Path::new(&p));
            }
        }
        match Self::default_path() {
            Some(p) => Self::load_from(&p),
            None => Config::default(),
        }
    }

    /// Load from an explicit path. A missing file yields defaults silently; an
    /// unreadable or malformed file is warned about and also yields defaults.
    pub fn load_from(path: &Path) -> Config {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Config::default(),
            Err(e) => {
                tracing::warn!("ignoring unreadable config {}: {e}", path.display());
                return Config::default();
            }
        };
        match toml::from_str(&text) {
            Ok(cfg) => cfg,
            Err(e) => {
                tracing::warn!("ignoring malformed config {}: {e}", path.display());
                Config::default()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_temp(name: &str, body: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("skiagram-cfg-{}-{name}", std::process::id()));
        let mut f = std::fs::File::create(&p).expect("temp config");
        f.write_all(body.as_bytes()).expect("write temp config");
        p
    }

    #[test]
    fn parses_default_agent() {
        let p = write_temp("ok.toml", "default_agent = \"codex\"\n");
        let cfg = Config::load_from(&p);
        assert_eq!(cfg.default_agent.as_deref(), Some("codex"));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn missing_file_is_defaults_not_error() {
        let p = std::env::temp_dir().join("skiagram-cfg-does-not-exist.toml");
        let _ = std::fs::remove_file(&p);
        assert_eq!(Config::load_from(&p).default_agent, None);
    }

    #[test]
    fn malformed_file_falls_back_to_defaults() {
        // Not valid TOML — must degrade to defaults, not panic/abort.
        let p = write_temp("bad.toml", "default_agent = = oops");
        assert_eq!(Config::load_from(&p).default_agent, None);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn unknown_keys_are_ignored_for_forward_compat() {
        let p = write_temp("fwd.toml", "default_agent = \"cursor\"\nfuture_key = 42\n");
        assert_eq!(
            Config::load_from(&p).default_agent.as_deref(),
            Some("cursor")
        );
        let _ = std::fs::remove_file(&p);
    }
}
