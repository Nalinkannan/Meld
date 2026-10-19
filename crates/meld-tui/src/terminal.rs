//! Terminal lifecycle management.
//!
//! Provides an RAII guard that always restores the terminal (raw mode,
//! alternate screen, and cursor) on drop, plus a panic hook that restores
//! the terminal before re-invoking the original hook. This guarantees the
//! terminal is usable again after normal exit, an error return, a panic,
//! or Ctrl-C.

use crossterm::ExecutableCommand;
use std::io;

/// RAII guard for the terminal.
///
/// On creation, raw mode and the alternate screen are enabled. On drop
/// (including during unwinding), they are restored and the cursor is shown.
pub struct TerminalGuard {
    alternate_screen: bool,
    raw_mode: bool,
}

impl TerminalGuard {
    /// Enter raw mode and the alternate screen.
    pub fn enter() -> io::Result<Self> {
        crossterm::terminal::enable_raw_mode()?;
        io::stdout().execute(crossterm::terminal::EnterAlternateScreen)?;
        io::stdout().execute(crossterm::cursor::Hide)?;
        Ok(Self {
            alternate_screen: true,
            raw_mode: true,
        })
    }

    /// Restore the terminal to its original state.
    pub fn restore(&mut self) -> io::Result<()> {
        if self.alternate_screen {
            let _ = io::stdout().execute(crossterm::terminal::LeaveAlternateScreen);
            self.alternate_screen = false;
        }
        if self.raw_mode {
            let _ = crossterm::terminal::disable_raw_mode();
            self.raw_mode = false;
        }
        let _ = io::stdout().execute(crossterm::cursor::Show);
        Ok(())
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

/// Install a panic hook that restores the terminal before panicking.
///
/// The original panic hook is invoked after restoration so the panic message
/// is still reported.
pub fn install_panic_hook() {
    let original = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = crossterm::terminal::disable_raw_mode();
        let _ = io::stdout().execute(crossterm::terminal::LeaveAlternateScreen);
        let _ = io::stdout().execute(crossterm::cursor::Show);
        original(info);
    }));
}
