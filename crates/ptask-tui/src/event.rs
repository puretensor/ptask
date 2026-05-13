//! crossterm event poller.

use anyhow::Result;
use crossterm::event::{self, KeyEvent};
use std::time::Duration;

#[derive(Debug, Clone)]
pub enum Event {
    Key(KeyEvent),
    /// Terminal resize — the next draw uses the new dimensions automatically;
    /// no state to capture here.
    Resize,
    Tick,
}

/// Poll for an event with a timeout. Returns `None` if no event arrived
/// within the timeout (callers can use that as a redraw tick).
pub fn poll_event(timeout: Duration) -> Result<Option<Event>> {
    if !event::poll(timeout)? {
        return Ok(None);
    }
    let raw = event::read()?;
    Ok(match raw {
        event::Event::Key(k) if k.kind == event::KeyEventKind::Press => Some(Event::Key(k)),
        event::Event::Resize(_, _) => Some(Event::Resize),
        _ => Some(Event::Tick),
    })
}
