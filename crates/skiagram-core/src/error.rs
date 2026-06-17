//! Typed errors for skiagram-core. The binary wraps these with `anyhow` context.

use std::path::PathBuf;
use thiserror::Error;

/// Errors produced by core operations.
#[derive(Debug, Error)]
pub enum CoreError {
    /// A session file could not be opened/read at all (per-line corruption is
    /// NOT an error — parsers skip bad lines leniently).
    #[error("failed to read {path}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// `--agent` named an adapter that does not exist.
    #[error("unknown agent id `{requested}` (known: {known})")]
    UnknownAgent { requested: String, known: String },
}
