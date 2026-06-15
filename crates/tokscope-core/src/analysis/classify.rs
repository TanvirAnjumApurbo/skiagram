//! Heuristic task-type classification (roadmap v0.3, CLAUDE.md §2 "where did it
//! go / why is context full").
//!
//! Answers "*what was I actually doing when I spent these tokens?*" by labeling
//! each session with a coarse activity [`TaskType`] — debugging vs feature work
//! vs refactor vs review/exploration vs tests vs docs vs config/ops — from its
//! **tool mix + prompt shape**, then rolling DEDUPLICATED spend up per type.
//!
//! Like [`super::aggregate`] / [`super::anomaly`] / [`super::drilldown`], spend
//! is taken from per-request records via [`super::dedup::dedup_session`] (never
//! raw JSONL lines — request dedup is THE accounting step, §8.1) and priced via
//! [`crate::pricing`] (cache-read vs cache-write separately, unpriced models
//! surfaced not guessed, §8.7). Sub-agent transcript spend folds into the parent
//! session's bucket (§8.3) exactly as `aggregate` does, so the per-type totals
//! reconcile to the `summary` grand total.
//!
//! Classification is **explicitly heuristic** — there is no ground truth in the
//! files. We stay honest about that the same way the rest of the tool stays
//! honest about unknowns (§8.5): an explicit [`TaskType::Unknown`] bucket when no
//! signal fires (never a forced guess), a `confidence` score, and the matched
//! `signals` surfaced so every label is traceable to its evidence.

use jiff::civil::Date;
use serde::Serialize;

use crate::analysis::aggregate::{Filter, Rollup};
use crate::model::Session;

/// Heuristic activity category a session is classified into. Small, defensible
/// set; [`TaskType::Unknown`] is the honest fallback when no signal dominates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum TaskType {
    /// Fixing errors/bugs (keywords like "fix"/"bug"/"error"; test-run + edit loops).
    Debugging,
    /// Adding new functionality (keywords like "add"/"implement"/"create"; new files).
    FeatureWork,
    /// Restructuring without behavior change (keywords like "refactor"/"rename"; edits, few writes).
    Refactoring,
    /// Writing/running tests (keywords like "test"/"coverage"; edits to test files).
    Testing,
    /// Docs/comments (keywords like "document"/"readme"; edits to `.md` files).
    Documentation,
    /// Reading/understanding/reviewing code (Read/Grep/Glob-heavy, few or no edits).
    Exploration,
    /// Build/CI/deps/config/ops (keywords like "ci"/"deploy"/"dependency"; edits to config files).
    ConfigOps,
    /// No signal dominated — not guessed (§8.5 ethos applied to a heuristic).
    Unknown,
}

/// One session's classification with the evidence behind it.
#[derive(Debug, Clone, Serialize)]
pub struct SessionClass {
    pub id: String,
    pub project: Option<String>,
    pub task_type: TaskType,
    /// Heuristic confidence in `0.0..=1.0` (winning score / total score); `0.0`
    /// for [`TaskType::Unknown`].
    pub confidence: f64,
    /// Human-readable signals that drove the decision (for transparency).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub signals: Vec<String>,
    /// Deduplicated spend for this session, sub-agent transcripts folded in (§8.3).
    pub rollup: Rollup,
}

/// Spend rolled up across all sessions of one [`TaskType`].
#[derive(Debug, Clone, Serialize)]
pub struct TaskTypeRollup {
    pub task_type: TaskType,
    /// Sessions classified into this type.
    pub sessions: u64,
    pub rollup: Rollup,
    /// Share of total known tokens (`0.0..=1.0`).
    pub token_share: f64,
    /// Share of total priced cost, or `None` when total cost is zero / this
    /// bucket is entirely unpriced (§8.5/§8.7 — never a $0 stand-in for unknown).
    pub cost_share: Option<f64>,
}

/// Spend broken down by heuristic task type, plus the per-session labels.
#[derive(Debug, Clone, Serialize)]
pub struct ClassifyReport {
    pub agent: String,
    pub since: Option<Date>,
    pub sessions_classified: u64,
    pub total_tokens: u64,
    /// Total priced cost; a lower bound when `has_unpriced` is true (§8.5/§8.7).
    pub total_cost_usd: f64,
    /// At least one classified session had an unpriced request — cost figures are
    /// lower bounds.
    pub has_unpriced: bool,
    /// Spend by task type, sorted by cost desc, then tokens desc, then a stable
    /// type order.
    pub by_task_type: Vec<TaskTypeRollup>,
    /// Per-session classifications, sorted by spend desc (cost, then tokens, then id).
    pub sessions: Vec<SessionClass>,
}

/// Classify each (top-level) session by activity type and roll deduplicated
/// spend up per type.
///
/// `filter.since` is applied inside [`super::dedup::dedup_session`], so the
/// report covers the same window as `summary`; a session group with no in-window
/// spend is omitted. Sub-agent transcripts fold into their parent's bucket (§8.3).
///
/// NOTE(scaffold): this is the v0.3 contract with a placeholder body that labels
/// everything [`TaskType::Unknown`]. The real tool-mix + prompt-shape heuristic
/// lands in the same file; the public types/signature above are fixed.
pub fn classify(sessions: &[Session], filter: &Filter, agent: &str) -> ClassifyReport {
    // Minimal, honest placeholder: no sessions classified yet.
    let _ = sessions;
    ClassifyReport {
        agent: agent.to_string(),
        since: filter.since,
        sessions_classified: 0,
        total_tokens: 0,
        total_cost_usd: 0.0,
        has_unpriced: false,
        by_task_type: Vec::new(),
        sessions: Vec::new(),
    }
}
