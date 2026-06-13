//! TUI app loop and key handling.

use std::time::Duration;

use anyhow::Result;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::widgets::TableState;
use ratatui::Frame;
use tokscope_core::analysis::aggregate::Summary;

use super::event::next_key;
use super::views;

pub fn run(summary: &Summary) -> Result<()> {
    // `ratatui::init` enters the alternate screen, enables raw mode, and installs
    // a panic hook that restores the terminal.
    let mut terminal = ratatui::init();
    let mut app = App::new(summary);
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

pub struct App<'a> {
    summary: &'a Summary,
    pub table_state: TableState,
    selected: usize,
}

impl<'a> App<'a> {
    fn new(summary: &'a Summary) -> Self {
        let mut table_state = TableState::default();
        if !summary.by_session.is_empty() {
            table_state.select(Some(0));
        }
        Self {
            summary,
            table_state,
            selected: 0,
        }
    }

    fn len(&self) -> usize {
        self.summary.by_session.len()
    }

    /// Returns `true` when the app should quit.
    fn handle_key(&mut self, key: KeyEvent) -> bool {
        // Windows terminals deliver Release events too — act on Press only.
        if key.kind != KeyEventKind::Press {
            return false;
        }
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => return true,
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => return true,
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::PageDown => self.move_selection(10),
            KeyCode::PageUp => self.move_selection(-10),
            KeyCode::Home | KeyCode::Char('g') => self.select(0),
            KeyCode::End | KeyCode::Char('G') => self.select(self.len().saturating_sub(1)),
            // TODO(scope): Enter = drill into the selected session (v0.2).
            _ => {}
        }
        false
    }

    fn move_selection(&mut self, delta: isize) {
        if self.len() == 0 {
            return;
        }
        let max = self.len() - 1;
        let next = self.selected.saturating_add_signed(delta).min(max);
        self.select(next);
    }

    fn select(&mut self, index: usize) {
        self.selected = index;
        self.table_state.select(Some(index));
    }

    fn draw(&mut self, frame: &mut Frame) {
        views::sessions::draw(frame, self.summary, &mut self.table_state);
    }
}
