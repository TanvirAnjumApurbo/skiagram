//! Terminal event polling (crossterm via the ratatui re-export).

use std::time::Duration;

use anyhow::Result;
use ratatui::crossterm::event::{self, Event, KeyEvent};

/// Next key event, or `None` after a tick timeout. Resize is handled implicitly
/// by the redraw on the following loop iteration.
pub fn next_key(timeout: Duration) -> Result<Option<KeyEvent>> {
    if event::poll(timeout)? {
        if let Event::Key(key) = event::read()? {
            return Ok(Some(key));
        }
    }
    Ok(None)
}
