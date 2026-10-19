//! Application events.
//!
//! The event loop is a single `tokio::select!` over three sources:
//! crossterm key/resize events, a tick, and results from background tasks.
//! Each background request is tagged with an id so stale results can be
//! ignored.

use crossterm::event::KeyEvent;

/// An event produced by one of the event sources.
#[derive(Debug)]
pub enum AppEvent {
    /// A crossterm key event.
    Key(KeyEvent),
    /// A terminal resize (columns, rows).
    Resize(u16, u16),
    /// A periodic tick for redraws/animations.
    Tick,
    /// A background task has finished. `id` matches a pending request id.
    /// `ok` is true when the task succeeded; `message` carries the result or
    /// error text.
    BackgroundResult { id: u64, ok: bool, message: String },
}

/// Monotonically increasing id for tagging background requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RequestId(pub u64);

impl RequestId {
    pub fn next(counter: &mut u64) -> Self {
        *counter += 1;
        Self(*counter)
    }
}
