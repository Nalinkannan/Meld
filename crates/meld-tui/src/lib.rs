//! Meld terminal user interface.
//!
//! The TUI is the primary interface for Meld and is implemented with Ratatui
//! and Crossterm.

pub mod app;
pub mod command;
pub mod components;
pub mod event;
pub mod input;
pub mod screens;
pub mod terminal;
pub mod theme;
pub mod widgets;

pub use app::{App, run};
pub use event::{AppEvent, RequestId};
pub use input::{InputMode, TextBuffer};
pub use screens::{Screen, ScreenResult, ScreenType, ScreenType::*};
pub use terminal::{TerminalGuard, install_panic_hook};
pub use theme::{BorderChars, BorderStyle, DEFAULT_BORDER, Palette, Styles};
