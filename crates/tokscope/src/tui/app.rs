//! TUI app loop, drill-down state machine, and key handling.
//!
//! Three levels (CLAUDE.md §2.4 / STATUS v0.2): **Sessions → Turns → Context**.
//! `Enter` descends a level, `Esc`/`Backspace`/`h` ascends, `q`/`Ctrl-C` quits
//! from anywhere. At the top level `Esc` also quits.

use std::time::Duration;

use anyhow::Result;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::widgets::TableState;
use ratatui::Frame;
use tokscope_core::analysis::aggregate::Summary;
use tokscope_core::analysis::drilldown::SessionDetail;

use super::event::next_key;
use super::views;

pub fn run(summary: &Summary, details: &[SessionDetail]) -> Result<()> {
    // `ratatui::init` enters the alternate screen, enables raw mode, and installs
    // a panic hook that restores the terminal.
    let mut terminal = ratatui::init();
    let mut app = App::new(summary, details);
    let result = loop {
        if let Err(e) = terminal.draw(|frame| app.draw(frame)) {
            break Err(e.into());
        }
        match next_key(Duration::from_millis(250)) {
            Ok(Some(key)) => {
                if app.handle_key(key) {
                    break Ok(());
                }
            }
            Ok(None) => {} // tick — nothing live to refresh yet
            Err(e) => break Err(e),
        }
    };
    ratatui::restore();
    result
}

/// Which drill-down level is on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Level {
    Sessions,
    Turns,
    Context,
}

pub struct App<'a> {
    summary: &'a Summary,
    details: &'a [SessionDetail],
    level: Level,
    /// Highlighted row in the session list.
    sessions_state: TableState,
    /// The session currently drilled into (index into `details`).
    session_idx: usize,
    /// Highlighted row in the turns list of the drilled session.
    turns_state: TableState,
    /// Vertical scroll offset in the context view.
    context_scroll: u16,
}

impl<'a> App<'a> {
    fn new(summary: &'a Summary, details: &'a [SessionDetail]) -> Self {
        let mut sessions_state = TableState::default();
        if !details.is_empty() {
            sessions_state.select(Some(0));
        }
        Self {
            summary,
            details,
            level: Level::Sessions,
            sessions_state,
            session_idx: 0,
            turns_state: TableState::default(),
            context_scroll: 0,
        }
    }

    /// The session currently drilled into, if any.
    fn current(&self) -> Option<&'a SessionDetail> {
        self.details.get(self.session_idx)
    }

    fn turns_len(&self) -> usize {
        self.current().map_or(0, |d| d.turns.len())
    }

    /// Returns `true` when the app should quit.
    fn handle_key(&mut self, key: KeyEvent) -> bool {
        // Windows terminals deliver Release events too — act on Press only.
        if key.kind != KeyEventKind::Press {
            return false;
        }
        // Global quits, available at every level.
        match key.code {
            KeyCode::Char('q') => return true,
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => return true,
            _ => {}
        }
        match self.level {
            Level::Sessions => self.handle_sessions(key),
            Level::Turns => self.handle_turns(key),
            Level::Context => self.handle_context(key),
        }
    }

    fn handle_sessions(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Esc => return true, // top level: Esc quits
            KeyCode::Enter | KeyCode::Char('l') | KeyCode::Right => self.enter_turns(),
            KeyCode::Down | KeyCode::Char('j') => {
                step(&mut self.sessions_state, self.details.len(), 1)
            }
            KeyCode::Up | KeyCode::Char('k') => {
                step(&mut self.sessions_state, self.details.len(), -1)
            }
            KeyCode::PageDown => step(&mut self.sessions_state, self.details.len(), 10),
            KeyCode::PageUp => step(&mut self.sessions_state, self.details.len(), -10),
            KeyCode::Home | KeyCode::Char('g') => {
                select(&mut self.sessions_state, self.details.len(), 0)
            }
            KeyCode::End | KeyCode::Char('G') => {
                select(&mut self.sessions_state, self.details.len(), usize::MAX)
            }
            _ => {}
        }
        false
    }

    fn handle_turns(&mut self, key: KeyEvent) -> bool {
        let len = self.turns_len();
        match key.code {
            KeyCode::Esc | KeyCode::Backspace | KeyCode::Char('h') | KeyCode::Left => {
                self.level = Level::Sessions;
            }
            KeyCode::Enter | KeyCode::Char('l') | KeyCode::Right => {
                self.level = Level::Context;
                self.context_scroll = 0;
            }
            KeyCode::Down | KeyCode::Char('j') => step(&mut self.turns_state, len, 1),
            KeyCode::Up | KeyCode::Char('k') => step(&mut self.turns_state, len, -1),
            KeyCode::PageDown => step(&mut self.turns_state, len, 10),
            KeyCode::PageUp => step(&mut self.turns_state, len, -10),
            KeyCode::Home | KeyCode::Char('g') => select(&mut self.turns_state, len, 0),
            KeyCode::End | KeyCode::Char('G') => select(&mut self.turns_state, len, usize::MAX),
            _ => {}
        }
        false
    }

    fn handle_context(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Esc | KeyCode::Backspace | KeyCode::Char('h') | KeyCode::Left => {
                self.level = Level::Turns;
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.context_scroll = self.context_scroll.saturating_add(1)
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.context_scroll = self.context_scroll.saturating_sub(1)
            }
            KeyCode::PageDown => self.context_scroll = self.context_scroll.saturating_add(10),
            KeyCode::PageUp => self.context_scroll = self.context_scroll.saturating_sub(10),
            KeyCode::Home | KeyCode::Char('g') => self.context_scroll = 0,
            _ => {}
        }
        false
    }

    /// Descend from the session list into the selected session's turns.
    fn enter_turns(&mut self) {
        if self.details.is_empty() {
            return;
        }
        self.session_idx = self.sessions_state.selected().unwrap_or(0);
        self.turns_state = TableState::default();
        if self.turns_len() > 0 {
            self.turns_state.select(Some(0));
        }
        self.level = Level::Turns;
    }

    fn draw(&mut self, frame: &mut Frame) {
        match self.level {
            Level::Sessions => {
                views::sessions::draw(frame, self.details, self.summary, &mut self.sessions_state)
            }
            Level::Turns => match self.details.get(self.session_idx) {
                Some(detail) => views::turns::draw(frame, detail, &mut self.turns_state),
                None => {
                    self.level = Level::Sessions;
                    views::sessions::draw(
                        frame,
                        self.details,
                        self.summary,
                        &mut self.sessions_state,
                    )
                }
            },
            Level::Context => match self.details.get(self.session_idx) {
                Some(detail) => views::context::draw(frame, detail, self.context_scroll),
                None => {
                    self.level = Level::Sessions;
                    views::sessions::draw(
                        frame,
                        self.details,
                        self.summary,
                        &mut self.sessions_state,
                    )
                }
            },
        }
    }
}

/// Move a table selection by `delta`, clamped to `[0, len-1]`. No-op when empty.
fn step(state: &mut TableState, len: usize, delta: isize) {
    if len == 0 {
        return;
    }
    let cur = state.selected().unwrap_or(0);
    let next = cur.saturating_add_signed(delta).min(len - 1);
    state.select(Some(next));
}

/// Select an absolute index, clamped to the last row (`usize::MAX` = End).
fn select(state: &mut TableState, len: usize, index: usize) {
    if len == 0 {
        state.select(None);
        return;
    }
    state.select(Some(index.min(len - 1)));
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use tokscope_core::analysis::aggregate::{aggregate, Filter};
    use tokscope_core::analysis::drilldown::build_details;
    use tokscope_core::model::{Event, EventKind, Session, Usage};

    fn asst(rid: &str, input: u64) -> Event {
        Event {
            kind: EventKind::Assistant,
            ts: Some("2026-06-02T10:00:00Z".parse().unwrap()),
            request_id: Some(rid.into()),
            model: Some("claude-sonnet-4-5".into()),
            usage: Some(Usage {
                input: Some(input),
                output: Some(10),
                cache_creation: Some(0),
                cache_read: Some(0),
                ..Usage::default()
            }),
            tool_calls: Vec::new(),
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

    fn session(id: &str, events: Vec<Event>) -> Session {
        Session {
            id: id.into(),
            agent: "claude-code".into(),
            project: Some("p".into()),
            model: Some("claude-sonnet-4-5".into()),
            parent_session: None,
            started_at: None,
            ended_at: None,
            events,
            sub_agents: Vec::new(),
            skipped_lines: 0,
        }
    }

    /// Two sessions: `big` (sorted first by cost, 2 turns) and `small` (1 turn).
    fn fixture() -> (Summary, Vec<SessionDetail>) {
        let sessions = vec![
            session("big", vec![asst("r1", 100_000), asst("r2", 5)]),
            session("small", vec![asst("r3", 10)]),
        ];
        let filter = Filter::default();
        let summary = aggregate(&sessions, &filter, 0, "claude-code");
        let details = build_details(&sessions, &filter, "claude-code");
        (summary, details)
    }

    fn press(app: &mut App<'_>, code: KeyCode) -> bool {
        app.handle_key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    #[test]
    fn enter_descends_and_esc_ascends_then_quits() {
        let (summary, details) = fixture();
        let mut app = App::new(&summary, &details);
        assert_eq!(app.level, Level::Sessions);
        assert!(!press(&mut app, KeyCode::Enter));
        assert_eq!(app.level, Level::Turns);
        assert!(!press(&mut app, KeyCode::Enter));
        assert_eq!(app.level, Level::Context);
        // Esc walks back up one level at a time...
        assert!(!press(&mut app, KeyCode::Esc));
        assert_eq!(app.level, Level::Turns);
        assert!(!press(&mut app, KeyCode::Esc));
        assert_eq!(app.level, Level::Sessions);
        // ...and quits only from the top level.
        assert!(press(&mut app, KeyCode::Esc));
    }

    #[test]
    fn drilling_targets_the_highlighted_session() {
        let (summary, details) = fixture();
        let mut app = App::new(&summary, &details);
        // Highlight the second row (`small`) then drill into it.
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.level, Level::Turns);
        assert_eq!(app.session_idx, 1);
        assert_eq!(app.turns_len(), 1, "`small` has one turn");
    }

    #[test]
    fn q_quits_from_any_level() {
        let (summary, details) = fixture();
        let mut app = App::new(&summary, &details);
        press(&mut app, KeyCode::Enter);
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.level, Level::Context);
        assert!(press(&mut app, KeyCode::Char('q')));
    }

    #[test]
    fn enter_is_a_noop_with_no_sessions() {
        // Empty details: nothing to drill into; stay put and don't panic.
        let sessions: Vec<Session> = Vec::new();
        let summary = aggregate(&sessions, &Filter::default(), 0, "claude-code");
        let details: Vec<SessionDetail> = Vec::new();
        let mut app = App::new(&summary, &details);
        assert!(!press(&mut app, KeyCode::Enter));
        assert_eq!(app.level, Level::Sessions);
    }
}
