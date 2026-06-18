//! Heuristic task-type classification (debugging vs feature work vs refactor…).
//!
//! Breaks total spend down by *what the user was actually doing*, so a glance at
//! the report answers "where did my tokens go?" in human terms rather than per
//! model/day. Each session is bucketed into one [`TaskType`] from two weak
//! signals only — the prompt text the user typed ([`Event::content_summary`] on
//! [`EventKind::User`] events) and the histogram of tool *names* the agent ran.
//! No file contents, paths, or tool inputs are available (see [`ToolCall`]), so
//! the classifier is deliberately coarse and reports a `confidence` and the
//! `signals` it fired on rather than pretending to be certain.
//!
//! Spend is rolled up the same way as [`super::aggregate`]: deduplicated per
//! request (CLAUDE.md §8.1) and folded from sub-agent transcripts into the
//! parent session's row (§8.3), so the per-type totals here reconcile with the
//! `summary` grand totals. Classification of a folded group is taken from its
//! representative (non-sidechain) session; spend always sums across the whole
//! group.

use std::collections::BTreeMap;

use jiff::civil::Date;
use serde::Serialize;

use crate::analysis::aggregate::{Filter, Rollup};
use crate::analysis::dedup::{dedup_session, UsageRecord};
use crate::model::{Event, EventKind, Session};
use crate::pricing::PricingTable;

/// What the user was doing in a session, inferred from prompt text + tool mix.
///
/// `Unknown` is a real bucket: it means *no* signal fired, not "a bit of
/// everything". The ordering of the non-`Unknown` variants is the deterministic
/// tie-breaker rank used when two types score or cost the same.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub enum TaskType {
    Debugging,
    FeatureWork,
    Refactoring,
    Testing,
    Documentation,
    Exploration,
    ConfigOps,
    Unknown,
}

impl TaskType {
    /// All classifiable (non-`Unknown`) types, in tie-break rank order.
    const SCORABLE: [TaskType; 7] = [
        TaskType::Debugging,
        TaskType::FeatureWork,
        TaskType::Refactoring,
        TaskType::Testing,
        TaskType::Documentation,
        TaskType::Exploration,
        TaskType::ConfigOps,
    ];

    /// Stable rank for deterministic sorting (lower sorts first). `Unknown` last.
    fn rank(self) -> u8 {
        match self {
            TaskType::Debugging => 0,
            TaskType::FeatureWork => 1,
            TaskType::Refactoring => 2,
            TaskType::Testing => 3,
            TaskType::Documentation => 4,
            TaskType::Exploration => 5,
            TaskType::ConfigOps => 6,
            TaskType::Unknown => 7,
        }
    }

    /// Lowercased prompt substrings that vote for this type. Each *distinct*
    /// phrase matched in a session's prompts contributes one keyword hit.
    fn keywords(self) -> &'static [&'static str] {
        match self {
            TaskType::Debugging => &[
                "fix",
                "bug",
                "error",
                "broken",
                "crash",
                "fails",
                "debug",
                "stack trace",
                "exception",
                "regression",
            ],
            TaskType::FeatureWork => &[
                "add",
                "implement",
                "create",
                "build",
                "feature",
                "support",
                "new",
                "endpoint",
            ],
            TaskType::Refactoring => &[
                "refactor",
                "rename",
                "clean up",
                "extract",
                "simplify",
                "reorganize",
                "move",
                "dedupe",
            ],
            TaskType::Testing => &[
                "test",
                "tests",
                "coverage",
                "assert",
                "unit test",
                "integration test",
            ],
            TaskType::Documentation => &[
                "document",
                "docs",
                "readme",
                "comment",
                "changelog",
                "explain in writing",
            ],
            TaskType::Exploration => &[
                "review",
                "explain",
                "understand",
                "how does",
                "what does",
                "look at",
                "analyze",
                "investigate",
                "find",
                "trace",
            ],
            TaskType::ConfigOps => &[
                "ci",
                "cd",
                "deploy",
                "pipeline",
                "config",
                "dependency",
                "dependencies",
                "upgrade",
                "bump",
                "cargo",
                "npm",
                "docker",
                "workflow",
                "version",
            ],
            // Never scored against; lives here so the match stays exhaustive.
            TaskType::Unknown => &[],
        }
    }
}

/// Counts of each tool *name* across a session's events (`MultiEdit` folded into
/// `Edit`). The only structural signal available — there is no tool input text.
#[derive(Debug, Default, Clone, Copy)]
struct ToolMix {
    edit: u64,
    write: u64,
    bash: u64,
    read: u64,
    grep: u64,
    glob: u64,
}

impl ToolMix {
    /// Tally tool-call names across a session's in-window events.
    fn from_session(session: &Session, since: Option<Date>) -> ToolMix {
        let mut mix = ToolMix::default();
        for event in &session.events {
            if !in_window(since, event) {
                continue;
            }
            for call in &event.tool_calls {
                match call.name.as_str() {
                    "Edit" | "MultiEdit" => mix.edit += 1,
                    "Write" => mix.write += 1,
                    "Bash" => mix.bash += 1,
                    "Read" => mix.read += 1,
                    "Grep" => mix.grep += 1,
                    "Glob" => mix.glob += 1,
                    _ => {}
                }
            }
        }
        mix
    }

    /// Reads-and-searches without writes — the Exploration shape.
    fn is_read_dominant(&self) -> bool {
        let browse = self.read + self.grep + self.glob;
        browse > 0 && self.edit == 0 && self.write == 0
    }
}

/// Spend for one [`TaskType`], summed over every session classified into it.
#[derive(Debug, Clone, Serialize)]
pub struct TaskTypeRollup {
    pub task_type: TaskType,
    /// Number of (folded) sessions classified into this type.
    pub sessions: u64,
    pub rollup: Rollup,
    /// This bucket's share of `total_tokens` in `[0,1]` (0.0 when total is 0).
    pub token_share: f64,
    /// This bucket's share of `total_cost_usd`, or `None` when nothing was
    /// priceable (so a 0/0 share is never shown as a real 0.0).
    pub cost_share: Option<f64>,
}

/// One classified session row (sub-agent transcript spend folded into it).
#[derive(Debug, Clone, Serialize)]
pub struct SessionClass {
    pub id: String,
    pub project: Option<String>,
    pub model: Option<String>,
    pub task_type: TaskType,
    /// Winning score / sum of all type scores, clamped to `[0,1]`. `0.0` for
    /// `Unknown` (no signal); `1.0` when exactly one type scored.
    pub confidence: f64,
    /// Small, human-readable, deterministically ordered notes on why this call
    /// was made (e.g. `prompt keyword: "fix"`, `tool mix: 12 Edit, 8 Bash`).
    pub signals: Vec<String>,
    pub rollup: Rollup,
}

/// Everything the renderers need for the task-type breakdown.
#[derive(Debug, Serialize)]
pub struct ClassifyReport {
    pub agent: String,
    /// Window applied to the spend (UTC date, inclusive), echoed for the header.
    pub since: Option<Date>,
    /// Number of (folded) sessions that had in-window spend and were classified.
    pub sessions_classified: u64,
    /// Deduplicated known tokens across all in-window sessions (incl. sub-agents).
    pub total_tokens: u64,
    /// USD cost of the priceable share of that spend.
    pub total_cost_usd: f64,
    /// True if any in-window request could not be priced (§8.5 / §8.7): the
    /// cost figures are then a lower bound.
    pub has_unpriced: bool,
    /// Per-type rollups, sorted by cost desc, tokens desc, then `TaskType` rank.
    pub by_task_type: Vec<TaskTypeRollup>,
    /// Per-session rows, sorted by cost desc, tokens desc, then id asc.
    pub sessions: Vec<SessionClass>,
}

/// Accumulates a session group (a parent plus its folded sub-agent transcripts).
struct GroupAcc {
    project: Option<String>,
    model: Option<String>,
    rollup: Rollup,
    /// The session used to classify the group: the non-sidechain one if seen,
    /// else the first session for the row id (an orphan sub-agent).
    representative: Option<Session>,
    /// Set once the representative is a real (non-sidechain) parent session, so
    /// a later orphan ordering never overrides it.
    has_parent: bool,
}

impl GroupAcc {
    fn new() -> GroupAcc {
        GroupAcc {
            project: None,
            model: None,
            rollup: Rollup::default(),
            representative: None,
            has_parent: false,
        }
    }
}

/// Classify each session by task type and roll deduplicated spend up per type.
///
/// Spend is folded from sub-agent transcripts into the parent session's row
/// (§8.3) exactly as [`super::aggregate::aggregate`] does, so the per-type
/// totals reconcile with the summary's grand totals. Groups with no in-window
/// spend are dropped; empty input yields a well-formed empty report.
pub fn classify(
    sessions: &[Session],
    filter: &Filter,
    agent: &str,
    pricing: &PricingTable,
) -> ClassifyReport {
    let mut groups: BTreeMap<String, GroupAcc> = BTreeMap::new();

    for session in sessions {
        let row_id = session
            .parent_session
            .clone()
            .unwrap_or_else(|| session.id.clone());
        let group = groups.entry(row_id).or_insert_with(GroupAcc::new);

        // Representative = the non-sidechain parent if present, else the first
        // session seen for this row id (an orphan sub-agent transcript).
        let is_parent = session.parent_session.is_none();
        if is_parent {
            group.project.clone_from(&session.project);
            group.model.clone_from(&session.model);
        } else if group.project.is_none() {
            group.project.clone_from(&session.project);
        }
        if (is_parent && !group.has_parent) || group.representative.is_none() {
            group.representative = Some(session.clone());
            group.has_parent |= is_parent;
        }

        let (records, _) = dedup_session(session, filter.since);
        for rec in &records {
            add_record(&mut group.rollup, rec, pricing);
        }
    }

    // Drop groups with no in-window spend; they did not happen in this window.
    let groups: Vec<(String, GroupAcc)> = groups
        .into_iter()
        .filter(|(_, acc)| acc.rollup.requests > 0)
        .collect();

    let mut total_tokens: u64 = 0;
    let mut total_cost_usd: f64 = 0.0;
    let mut has_unpriced = false;
    for (_, acc) in &groups {
        total_tokens += acc.rollup.total_tokens();
        total_cost_usd += acc.rollup.cost_usd;
        has_unpriced |= acc.rollup.unpriced_requests > 0;
    }

    let mut sessions_out: Vec<SessionClass> = Vec::with_capacity(groups.len());
    let mut by_type: BTreeMap<u8, (TaskType, u64, Rollup)> = BTreeMap::new();

    for (id, acc) in groups {
        let (task_type, confidence, signals) = match &acc.representative {
            Some(rep) => classify_session(rep, filter.since),
            // A group can only exist with >0 requests, which require events, so a
            // representative is always present; classify Unknown if it somehow is not.
            None => (TaskType::Unknown, 0.0, Vec::new()),
        };

        let entry = by_type
            .entry(task_type.rank())
            .or_insert_with(|| (task_type, 0, Rollup::default()));
        entry.1 += 1;
        merge_rollup(&mut entry.2, &acc.rollup);

        sessions_out.push(SessionClass {
            id,
            project: acc.project,
            model: acc.model,
            task_type,
            confidence,
            signals,
            rollup: acc.rollup,
        });
    }

    let mut by_task_type: Vec<TaskTypeRollup> = by_type
        .into_values()
        .map(|(task_type, sessions, rollup)| {
            let token_share = if total_tokens == 0 {
                0.0
            } else {
                rollup.total_tokens() as f64 / total_tokens as f64
            };
            let cost_share = (total_cost_usd > 0.0).then(|| rollup.cost_usd / total_cost_usd);
            TaskTypeRollup {
                task_type,
                sessions,
                rollup,
                token_share,
                cost_share,
            }
        })
        .collect();

    by_task_type.sort_by(|a, b| {
        b.rollup
            .cost_usd
            .total_cmp(&a.rollup.cost_usd)
            .then_with(|| b.rollup.total_tokens().cmp(&a.rollup.total_tokens()))
            .then_with(|| a.task_type.rank().cmp(&b.task_type.rank()))
    });

    sessions_out.sort_by(|a, b| {
        b.rollup
            .cost_usd
            .total_cmp(&a.rollup.cost_usd)
            .then_with(|| b.rollup.total_tokens().cmp(&a.rollup.total_tokens()))
            .then_with(|| a.id.cmp(&b.id))
    });

    ClassifyReport {
        agent: agent.to_string(),
        since: filter.since,
        sessions_classified: sessions_out.len() as u64,
        total_tokens,
        total_cost_usd,
        has_unpriced,
        by_task_type,
        sessions: sessions_out,
    }
}

/// Add one deduplicated request into a [`Rollup`].
///
/// `Rollup`'s own per-record `add` is private to [`super::aggregate`], so this
/// mirrors it exactly (same fields, same `pricing::cost_usd` call) over the
/// already-deduplicated record — keeping per-type totals reconciled with the
/// summary's grand totals (CLAUDE.md §8.1, §8.4, §8.5).
fn add_record(into: &mut Rollup, rec: &UsageRecord, pricing: &PricingTable) {
    let u = &rec.usage;
    into.requests += 1;
    into.input += u.input.unwrap_or(0);
    into.output += u.output.unwrap_or(0);
    into.cache_creation += u.cache_creation.unwrap_or(0);
    into.cache_read += u.cache_read.unwrap_or(0);
    if u.input.is_none()
        || u.output.is_none()
        || u.cache_creation.is_none()
        || u.cache_read.is_none()
    {
        into.incomplete_requests += 1;
    }
    match pricing.cost_usd(rec.model.as_deref(), u) {
        Some(cost) => into.cost_usd += cost,
        None => into.unpriced_requests += 1,
    }
}

/// Fold one [`Rollup`]'s totals into another (for per-type aggregation of
/// already-built per-group rollups).
fn merge_rollup(into: &mut Rollup, from: &Rollup) {
    into.requests += from.requests;
    into.input += from.input;
    into.output += from.output;
    into.cache_creation += from.cache_creation;
    into.cache_read += from.cache_read;
    into.incomplete_requests += from.incomplete_requests;
    into.cost_usd += from.cost_usd;
    into.unpriced_requests += from.unpriced_requests;
}

/// Classify a single session into a [`TaskType`] with a confidence and the
/// signals that fired.
///
/// Scoring: each distinct prompt keyword matched adds its type's weight; tool-mix
/// shapes add bonuses (debugging = many Bash+Edit; feature = Write+Edit; refactor
/// = many Edit with ~no Write; exploration = Read/Grep/Glob with no Edit/Write).
/// The argmax type wins; `confidence = winning / Σ scores` clamped to `[0,1]`.
/// No signal at all → `Unknown`, confidence `0.0`, empty signals.
fn classify_session(session: &Session, since: Option<Date>) -> (TaskType, f64, Vec<String>) {
    let prompts = lowercased_prompts(session, since);
    let mix = ToolMix::from_session(session, since);

    // Score every scorable type, remembering which keywords fired for the winner.
    let mut scores: Vec<(TaskType, f64, Vec<&'static str>)> = Vec::with_capacity(7);
    for ty in TaskType::SCORABLE {
        let mut hits: Vec<&'static str> = Vec::new();
        for kw in ty.keywords() {
            if prompts.iter().any(|p| p.contains(kw)) {
                hits.push(kw);
            }
        }
        // Each distinct keyword is worth 2; tool-mix bonuses are scaled below so a
        // strong structural signal counts roughly like a keyword or two.
        let keyword_score = hits.len() as f64 * 2.0;
        let tool_score = tool_mix_bonus(ty, &mix);
        let total = keyword_score + tool_score;
        scores.push((ty, total, hits));
    }

    let sum: f64 = scores.iter().map(|(_, s, _)| *s).sum();

    // Argmax with a deterministic tie-break on rank (SCORABLE is already in rank
    // order, so the first max wins).
    let mut best_idx: Option<usize> = None;
    for (i, (_, score, _)) in scores.iter().enumerate() {
        if *score <= 0.0 {
            continue;
        }
        match best_idx {
            Some(b) if scores[b].1 >= *score => {}
            _ => best_idx = Some(i),
        }
    }

    let Some(best_idx) = best_idx else {
        // No keyword and no tool-mix signal fired.
        return (TaskType::Unknown, 0.0, Vec::new());
    };

    let (task_type, best_score, hits) = &scores[best_idx];
    let confidence = if sum <= 0.0 {
        0.0
    } else {
        (best_score / sum).clamp(0.0, 1.0)
    };

    let signals = build_signals(hits, *task_type, &mix);
    (*task_type, confidence, signals)
}

/// Structural (tool-mix) bonus for a type. Tuned so a clear shape is worth about
/// one or two keyword hits, never enough to overpower an explicit prompt.
fn tool_mix_bonus(ty: TaskType, mix: &ToolMix) -> f64 {
    match ty {
        // Iterative fix loop: editing and running things repeatedly.
        TaskType::Debugging => {
            if mix.bash >= 3 && mix.edit >= 1 {
                3.0
            } else if mix.bash >= 1 && mix.edit >= 1 {
                1.0
            } else {
                0.0
            }
        }
        // New code lands via Write; usually edited afterward too.
        TaskType::FeatureWork => {
            if mix.write >= 1 && mix.edit >= 1 {
                3.0
            } else if mix.write >= 1 {
                1.5
            } else {
                0.0
            }
        }
        // Lots of edits to existing files, (almost) no new files.
        TaskType::Refactoring => {
            if mix.edit >= 3 && mix.write == 0 {
                3.0
            } else {
                0.0
            }
        }
        // Reading and searching, not changing anything.
        TaskType::Exploration => {
            if mix.is_read_dominant() {
                3.0
            } else {
                0.0
            }
        }
        // No reliable tool-only shape for these; prompt keywords carry them.
        TaskType::Testing | TaskType::Documentation | TaskType::ConfigOps => 0.0,
        TaskType::Unknown => 0.0,
    }
}

/// Build the deterministic `signals` list: prompt-keyword notes first (in the
/// type's keyword order), then a single tool-mix note when a shape contributed.
fn build_signals(hits: &[&'static str], ty: TaskType, mix: &ToolMix) -> Vec<String> {
    let mut signals: Vec<String> = hits
        .iter()
        .map(|kw| format!("prompt keyword: \"{kw}\""))
        .collect();
    if tool_mix_bonus(ty, mix) > 0.0 {
        if let Some(note) = tool_mix_note(mix) {
            signals.push(note);
        }
    }
    signals
}

/// Render the counted tools as a stable `tool mix: 12 Edit, 8 Bash` note, in a
/// fixed column order. `None` when no counted tool ran.
fn tool_mix_note(mix: &ToolMix) -> Option<String> {
    let parts: [(u64, &str); 6] = [
        (mix.edit, "Edit"),
        (mix.write, "Write"),
        (mix.bash, "Bash"),
        (mix.read, "Read"),
        (mix.grep, "Grep"),
        (mix.glob, "Glob"),
    ];
    let listed: Vec<String> = parts
        .iter()
        .filter(|(n, _)| *n > 0)
        .map(|(n, name)| format!("{n} {name}"))
        .collect();
    if listed.is_empty() {
        None
    } else {
        Some(format!("tool mix: {}", listed.join(", ")))
    }
}

/// Lowercased visible prompt text from every in-window `User` event's
/// `content_summary`.
fn lowercased_prompts(session: &Session, since: Option<Date>) -> Vec<String> {
    session
        .events
        .iter()
        .filter(|e| e.kind == EventKind::User && in_window(since, e))
        .filter_map(|e| e.content_summary.as_deref())
        .map(|s| s.to_ascii_lowercase())
        .collect()
}

/// Window predicate matching the spend passes ([`super::dedup`] /
/// [`super::aggregate`]): with `since` set, an event counts only if dated on/after
/// it; undated events drop. Applied to the classification signals too, so a
/// windowed run classifies a session from the same events whose spend it reports —
/// not from prompts/tools that happened outside the window.
fn in_window(since: Option<Date>, event: &Event) -> bool {
    match (since, event.ts) {
        (None, _) => true,
        (Some(since), Some(ts)) => super::utc_date(ts) >= since,
        (Some(_), None) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Event, ToolCall, Usage};

    fn priced_usage(input: u64) -> Usage {
        Usage {
            input: Some(input),
            output: Some(10),
            cache_creation: Some(0),
            cache_read: Some(0),
            ..Usage::default()
        }
    }

    /// An assistant turn carrying usage (priced model) and optional tool calls.
    fn turn(request_id: &str, ts: &str, input: u64, tools: &[&str]) -> Event {
        Event {
            kind: EventKind::Assistant,
            ts: Some(ts.parse().unwrap()),
            request_id: Some(request_id.into()),
            model: Some("claude-sonnet-4-5".into()),
            usage: Some(priced_usage(input)),
            tool_calls: tools.iter().map(|name| tool_call(name)).collect(),
            sidechain: false,
            content_summary: None,
            content_chars: 0,
            thinking_chars: 0,
            has_thinking: false,
            tool_use_id: None,
            attachment_kind: None,
            item_count: 0,
        }
    }

    /// An assistant turn whose model is not in the pricing snapshot.
    fn unpriced_turn(request_id: &str, ts: &str, input: u64) -> Event {
        let mut e = turn(request_id, ts, input, &[]);
        e.model = Some("claude-opus-5-0".into()); // post-snapshot, unpriceable
        e
    }

    /// A `User` event carrying a prompt snippet.
    fn user(ts: &str, prompt: &str) -> Event {
        Event {
            kind: EventKind::User,
            ts: Some(ts.parse().unwrap()),
            request_id: None,
            model: None,
            usage: None,
            tool_calls: Vec::new(),
            sidechain: false,
            content_summary: Some(prompt.into()),
            content_chars: prompt.len() as u64,
            thinking_chars: 0,
            has_thinking: false,
            tool_use_id: None,
            attachment_kind: None,
            item_count: 0,
        }
    }

    fn tool_call(name: &str) -> ToolCall {
        ToolCall {
            id: format!("tc_{name}"),
            name: name.into(),
            input_bytes: 0,
            server: ToolCall::server_from_name(name),
        }
    }

    fn session(id: &str, parent: Option<&str>, events: Vec<Event>) -> Session {
        Session {
            id: id.into(),
            agent: "claude-code".into(),
            project: Some("proj".into()),
            model: Some("claude-sonnet-4-5".into()),
            parent_session: parent.map(str::to_string),
            started_at: None,
            ended_at: None,
            events,
            sub_agents: Vec::new(),
            skipped_lines: 0,
        }
    }

    /// Make N Edit/Bash/etc. tool calls inside a single assistant turn.
    fn tools_turn(request_id: &str, tools: &[&str]) -> Event {
        turn(request_id, "2026-06-01T10:00:00Z", 100, tools)
    }

    fn no_filter() -> Filter {
        Filter::default()
    }

    fn class_of<'a>(report: &'a ClassifyReport, id: &str) -> &'a SessionClass {
        report
            .sessions
            .iter()
            .find(|s| s.id == id)
            .expect("session present in report")
    }

    // ---- per-type firing -------------------------------------------------

    #[test]
    fn debugging_fires_on_prompt_and_fix_loop() {
        let s = session(
            "dbg",
            None,
            vec![
                user("2026-06-01T10:00:00Z", "Please fix the crash in the parser"),
                tools_turn("r1", &["Edit", "Bash", "Bash", "Bash", "Edit"]),
            ],
        );
        let report = classify(&[s], &no_filter(), "claude-code", &PricingTable::embedded());
        let c = class_of(&report, "dbg");
        assert_eq!(c.task_type, TaskType::Debugging);
        assert!(c.signals.iter().any(|s| s.contains("\"fix\"")));
        assert!(c.signals.iter().any(|s| s.contains("tool mix")));
    }

    #[test]
    fn feature_work_fires_on_implement_with_write() {
        let s = session(
            "feat",
            None,
            vec![
                user("2026-06-01T10:00:00Z", "Implement a new export endpoint"),
                tools_turn("r1", &["Write", "Edit"]),
            ],
        );
        let report = classify(&[s], &no_filter(), "claude-code", &PricingTable::embedded());
        assert_eq!(class_of(&report, "feat").task_type, TaskType::FeatureWork);
    }

    #[test]
    fn refactoring_fires_on_many_edits_no_write() {
        let s = session(
            "ref",
            None,
            vec![
                user("2026-06-01T10:00:00Z", "Refactor and simplify this module"),
                tools_turn("r1", &["Edit", "MultiEdit", "Edit", "Edit"]),
            ],
        );
        let report = classify(&[s], &no_filter(), "claude-code", &PricingTable::embedded());
        let c = class_of(&report, "ref");
        assert_eq!(c.task_type, TaskType::Refactoring);
        // MultiEdit folds into the Edit count: 3 Edit + 1 MultiEdit = 4 Edit.
        assert!(c.signals.iter().any(|s| s.contains("4 Edit")));
    }

    #[test]
    fn testing_fires_on_prompt() {
        let s = session(
            "tst",
            None,
            vec![user(
                "2026-06-01T10:00:00Z",
                "Add unit tests to raise coverage",
            )],
        );
        // Provide spend so the group survives.
        let s = with_spend(s);
        let report = classify(&[s], &no_filter(), "claude-code", &PricingTable::embedded());
        assert_eq!(class_of(&report, "tst").task_type, TaskType::Testing);
    }

    #[test]
    fn documentation_fires_on_prompt() {
        let s = with_spend(session(
            "doc",
            None,
            vec![user(
                "2026-06-01T10:00:00Z",
                "Update the README and changelog",
            )],
        ));
        let report = classify(&[s], &no_filter(), "claude-code", &PricingTable::embedded());
        assert_eq!(class_of(&report, "doc").task_type, TaskType::Documentation);
    }

    #[test]
    fn exploration_fires_on_read_dominant_mix() {
        let s = session(
            "exp",
            None,
            vec![
                user(
                    "2026-06-01T10:00:00Z",
                    "Help me understand how the dedup works",
                ),
                tools_turn("r1", &["Read", "Grep", "Glob", "Read"]),
            ],
        );
        let report = classify(&[s], &no_filter(), "claude-code", &PricingTable::embedded());
        let c = class_of(&report, "exp");
        assert_eq!(c.task_type, TaskType::Exploration);
        assert!(c.signals.iter().any(|s| s.contains("tool mix")));
    }

    #[test]
    fn config_ops_fires_on_prompt() {
        let s = with_spend(session(
            "cfg",
            None,
            vec![user(
                "2026-06-01T10:00:00Z",
                "Bump the cargo dependencies in the CI workflow",
            )],
        ));
        let report = classify(&[s], &no_filter(), "claude-code", &PricingTable::embedded());
        assert_eq!(class_of(&report, "cfg").task_type, TaskType::ConfigOps);
    }

    /// Attach a priced assistant turn so a prompt-only session has in-window spend.
    fn with_spend(mut s: Session) -> Session {
        s.events
            .push(turn("spend", "2026-06-01T10:00:00Z", 100, &[]));
        s
    }

    // ---- Unknown + confidence -------------------------------------------

    #[test]
    fn unknown_when_no_signal_fires() {
        let s = session(
            "blank",
            None,
            vec![
                user("2026-06-01T10:00:00Z", "hmm okay then proceed please"),
                turn("r1", "2026-06-01T10:00:00Z", 100, &[]),
            ],
        );
        let report = classify(&[s], &no_filter(), "claude-code", &PricingTable::embedded());
        let c = class_of(&report, "blank");
        assert_eq!(c.task_type, TaskType::Unknown);
        assert_eq!(c.confidence, 0.0);
        assert!(c.signals.is_empty());
    }

    #[test]
    fn confidence_is_one_when_only_one_type_scores() {
        let s = with_spend(session(
            "solo",
            None,
            vec![user("2026-06-01T10:00:00Z", "Update the readme docs")],
        ));
        let report = classify(&[s], &no_filter(), "claude-code", &PricingTable::embedded());
        let c = class_of(&report, "solo");
        assert_eq!(c.task_type, TaskType::Documentation);
        assert!((c.confidence - 1.0).abs() < 1e-9, "got {}", c.confidence);
    }

    #[test]
    fn confidence_in_range_and_below_one_when_types_compete() {
        // "fix" (Debugging) and "add tests" (Testing) both fire.
        let s = with_spend(session(
            "mix",
            None,
            vec![user(
                "2026-06-01T10:00:00Z",
                "fix the failing tests and add coverage",
            )],
        ));
        let report = classify(&[s], &no_filter(), "claude-code", &PricingTable::embedded());
        let c = class_of(&report, "mix");
        assert!(
            c.confidence > 0.0 && c.confidence < 1.0,
            "got {}",
            c.confidence
        );
        assert!((0.0..=1.0).contains(&c.confidence));
    }

    // ---- sub-agent folding ----------------------------------------------

    /// §8.3: a sub-agent transcript folds into the parent's bucket and is NOT a
    /// separate session row; the parent's classification still drives the type.
    #[test]
    fn sub_agent_spend_folds_into_parent() {
        let parent = session(
            "parent",
            None,
            vec![
                user("2026-06-01T10:00:00Z", "Refactor and simplify the module"),
                tools_turn("p1", &["Edit", "Edit", "Edit"]),
            ],
        );
        let child = session(
            "agent-x",
            Some("parent"),
            vec![turn("c1", "2026-06-01T10:01:00Z", 500, &[])],
        );
        let report = classify(
            &[parent, child],
            &no_filter(),
            "claude-code",
            &PricingTable::embedded(),
        );

        // One row only — the child folded in.
        assert_eq!(report.sessions.len(), 1);
        let row = &report.sessions[0];
        assert_eq!(row.id, "parent");
        assert_eq!(row.task_type, TaskType::Refactoring);
        // One parent request (input 100, three tool calls on the one turn) +
        // one child request (input 500) = 600, folded into the parent row.
        assert_eq!(row.rollup.requests, 2);
        assert_eq!(row.rollup.input, 600);

        // The Refactoring bucket holds the combined spend.
        let bucket = report
            .by_task_type
            .iter()
            .find(|b| b.task_type == TaskType::Refactoring)
            .unwrap();
        assert_eq!(bucket.sessions, 1);
        assert_eq!(bucket.rollup.input, 600);
    }

    /// An orphan sub-agent (parent file absent) is classified from itself.
    #[test]
    fn orphan_sub_agent_is_its_own_group() {
        let child = session(
            "agent-y",
            Some("missing-parent"),
            vec![
                user("2026-06-01T10:00:00Z", "Investigate and trace the bug"),
                tools_turn("c1", &["Read", "Grep"]),
            ],
        );
        let report = classify(
            &[child],
            &no_filter(),
            "claude-code",
            &PricingTable::embedded(),
        );
        assert_eq!(report.sessions.len(), 1);
        let row = &report.sessions[0];
        assert_eq!(row.id, "missing-parent");
        // "investigate"/"trace" + read-dominant mix -> Exploration.
        assert_eq!(row.task_type, TaskType::Exploration);
    }

    // ---- windowing -------------------------------------------------------

    #[test]
    fn since_drops_out_of_window_sessions() {
        let old = session(
            "old",
            None,
            vec![
                user("2026-06-01T10:00:00Z", "fix the bug"),
                turn("o1", "2026-06-01T10:00:00Z", 100, &[]),
            ],
        );
        let new = session(
            "new",
            None,
            vec![
                user("2026-06-03T10:00:00Z", "implement the feature"),
                turn("n1", "2026-06-03T10:00:00Z", 200, &["Write", "Edit"]),
            ],
        );
        let filter = Filter {
            since: Some("2026-06-03".parse().unwrap()),
        };
        let report = classify(
            &[old, new],
            &filter,
            "claude-code",
            &PricingTable::embedded(),
        );
        assert_eq!(report.sessions.len(), 1, "old dropped (no in-window spend)");
        assert_eq!(report.sessions[0].id, "new");
        assert_eq!(report.total_tokens, 200 + 10);
    }

    /// `--since` scopes the CLASSIFICATION signals, not just the spend: a session
    /// that debugged before the window but only explored within it is classified
    /// Exploration, and the out-of-window "fix" prompt is not a signal.
    #[test]
    fn since_scopes_classification_signals_not_just_spend() {
        let s = session(
            "spanning",
            None,
            vec![
                // Out of window: a debugging prompt + a fix-loop (Bash/Edit) turn.
                user("2026-06-01T10:00:00Z", "fix the crash"),
                tools_turn("dbg", &["Edit", "Bash", "Bash", "Bash"]),
                // In window: an exploration prompt + read-dominant mix (the only
                // in-window spend, so the group survives).
                user("2026-06-03T10:00:00Z", "review and understand the module"),
                turn(
                    "exp",
                    "2026-06-03T10:00:00Z",
                    200,
                    &["Read", "Grep", "Glob"],
                ),
            ],
        );
        let filter = Filter {
            since: Some("2026-06-03".parse().unwrap()),
        };
        let report = classify(&[s], &filter, "claude-code", &PricingTable::embedded());
        let c = class_of(&report, "spanning");
        // Without windowing the signals, the heavier out-of-window debugging
        // evidence would win; scoped to the window it is Exploration.
        assert_eq!(c.task_type, TaskType::Exploration);
        assert!(
            !c.signals.iter().any(|sig| sig.contains("\"fix\"")),
            "out-of-window debugging prompt must not be a signal"
        );
    }

    // ---- pricing / shares ------------------------------------------------

    #[test]
    fn unpriced_model_sets_flag_and_nulls_cost_share() {
        let s = session(
            "u",
            None,
            vec![
                user("2026-06-01T10:00:00Z", "fix the crash"),
                unpriced_turn("u1", "2026-06-01T10:00:00Z", 1000),
            ],
        );
        let report = classify(&[s], &no_filter(), "claude-code", &PricingTable::embedded());
        assert!(report.has_unpriced);
        assert_eq!(report.total_cost_usd, 0.0);
        assert_eq!(report.by_task_type.len(), 1);
        // total_cost_usd == 0 -> every cost_share is None (no 0/0 = 0.0 lie).
        assert!(report.by_task_type[0].cost_share.is_none());
        // Tokens are still counted even when unpriceable.
        assert_eq!(report.total_tokens, 1010);
    }

    #[test]
    fn token_shares_sum_to_one_across_buckets() {
        let dbg = session(
            "d",
            None,
            vec![
                user("2026-06-01T10:00:00Z", "fix the bug"),
                turn("d1", "2026-06-01T10:00:00Z", 1000, &[]),
            ],
        );
        let feat = session(
            "f",
            None,
            vec![
                user("2026-06-01T10:00:00Z", "implement the feature"),
                turn("f1", "2026-06-01T10:00:00Z", 3000, &["Write", "Edit"]),
            ],
        );
        let report = classify(
            &[dbg, feat],
            &no_filter(),
            "claude-code",
            &PricingTable::embedded(),
        );
        assert_eq!(report.by_task_type.len(), 2);
        let share_sum: f64 = report.by_task_type.iter().map(|b| b.token_share).sum();
        assert!((share_sum - 1.0).abs() < 1e-9, "shares sum to {share_sum}");
        let cost_sum: f64 = report
            .by_task_type
            .iter()
            .map(|b| b.cost_share.unwrap_or(0.0))
            .sum();
        assert!(
            (cost_sum - 1.0).abs() < 1e-9,
            "cost shares sum to {cost_sum}"
        );
    }

    // ---- ordering + empty -----------------------------------------------

    #[test]
    fn buckets_and_sessions_sort_by_cost_desc() {
        let small = session(
            "small",
            None,
            vec![
                user("2026-06-01T10:00:00Z", "fix the bug"),
                turn("s1", "2026-06-01T10:00:00Z", 100, &[]),
            ],
        );
        let big = session(
            "big",
            None,
            vec![
                user("2026-06-01T10:00:00Z", "implement the feature"),
                turn("b1", "2026-06-01T10:00:00Z", 9000, &["Write", "Edit"]),
            ],
        );
        let report = classify(
            &[small, big],
            &no_filter(),
            "claude-code",
            &PricingTable::embedded(),
        );
        // Sessions: higher cost first.
        assert_eq!(report.sessions[0].id, "big");
        assert_eq!(report.sessions[1].id, "small");
        // Buckets: FeatureWork (bigger spend) before Debugging.
        assert_eq!(report.by_task_type[0].task_type, TaskType::FeatureWork);
        assert_eq!(report.by_task_type[1].task_type, TaskType::Debugging);
    }

    #[test]
    fn empty_input_yields_empty_report() {
        let report = classify(&[], &no_filter(), "claude-code", &PricingTable::embedded());
        assert_eq!(report.total_tokens, 0);
        assert_eq!(report.total_cost_usd, 0.0);
        assert!(!report.has_unpriced);
        assert!(report.by_task_type.is_empty());
        assert!(report.sessions.is_empty());
    }

    #[test]
    fn sessions_without_spend_are_dropped() {
        // A prompt but no assistant usage -> no in-window spend -> no row.
        let s = session(
            "noissue",
            None,
            vec![user("2026-06-01T10:00:00Z", "fix the bug")],
        );
        let report = classify(&[s], &no_filter(), "claude-code", &PricingTable::embedded());
        assert!(report.sessions.is_empty());
        assert!(report.by_task_type.is_empty());
    }
}
