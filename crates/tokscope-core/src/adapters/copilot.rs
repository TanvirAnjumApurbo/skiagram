//! GitHub Copilot CLI adapter — STUB.
//!
//! TODO(verify): config/log dir is platform-specific; confirm on a real install
//! (ccusage supports it — reference its source for exact paths).

use crate::adapters::Adapter;
use crate::model::{Session, SessionRef};

pub struct Copilot;

impl Adapter for Copilot {
    fn id(&self) -> &'static str {
        "copilot"
    }

    fn detect(&self) -> bool {
        directories::BaseDirs::new().is_some_and(|b| b.home_dir().join(".copilot").is_dir())
    }

    fn discover(&self) -> anyhow::Result<Vec<SessionRef>> {
        anyhow::bail!("copilot adapter not yet implemented — contributions welcome, see README \"Adding an agent\"")
    }

    fn parse(&self, _r: &SessionRef) -> anyhow::Result<Session> {
        anyhow::bail!("copilot adapter not yet implemented")
    }
}
