//! Meld TUI Application
//!
//! Main application state, event loop, and screen routing.
//!
//! The event loop is a single `tokio::select!` over crossterm events (read on
//! a dedicated thread and forwarded through an mpsc channel), a periodic
//! tick, and results from background tasks. No database or engine call ever
//! blocks the draw loop or a key handler: work is spawned on the tokio
//! runtime and the result is posted back as an `AppEvent::BackgroundResult`.

use std::io;
use std::time::{Duration, Instant};

use crate::components::ConfirmDialog;
use crate::event::AppEvent;
use crate::input::{InputMode, TextBuffer};
use crate::screens::{Screen, ScreenAction, ScreenContext, ScreenResult, ScreenType};
use crate::terminal::{TerminalGuard, install_panic_hook};
use crate::theme::Styles;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Rect},
    style::Modifier,
};

/// Minimum supported terminal size.
const MIN_WIDTH: u16 = 80;
const MIN_HEIGHT: u16 = 24;

/// Result of a background task posted back to the event loop.
type BackgroundResult = Result<String, String>;

/// Main application state.
pub struct App {
    /// Whether the application should quit.
    pub should_quit: bool,
    /// Current screen stack (for navigation).
    pub screen_stack: Vec<Box<dyn Screen>>,
    /// Input mode. Determines how keys are routed.
    pub input_mode: InputMode,
    /// Command bar buffer (active when `input_mode == Command`).
    pub command_buffer: TextBuffer,
    /// Pending background requests: id -> human-readable label.
    pub pending: Vec<(u64, String)>,
    /// Next request id.
    next_request_id: u64,
    /// Sender for background task results.
    event_tx: Option<tokio::sync::mpsc::UnboundedSender<AppEvent>>,
    /// Last tick time.
    last_tick: Instant,
    /// Terminal size.
    terminal_size: ratatui::layout::Size,
    /// Whether a background operation is in flight.
    pub busy: bool,
    /// Last status message.
    pub status: Option<String>,
    /// Last error message.
    pub error: Option<String>,
    /// Active confirmation dialog, if any.
    pub confirm: Option<ConfirmDialog>,
    /// Shared screen context, set by `run()` before the loop starts.
    ctx: Option<ScreenContext>,
}

impl App {
    pub fn new() -> Self {
        Self {
            should_quit: false,
            screen_stack: Vec::new(),
            input_mode: InputMode::Normal,
            command_buffer: TextBuffer::single_line(),
            pending: Vec::new(),
            next_request_id: 0,
            event_tx: None,
            last_tick: Instant::now(),
            terminal_size: ratatui::layout::Size {
                width: 80,
                height: 24,
            },
            busy: false,
            status: None,
            error: None,
            confirm: None,
            ctx: None,
        }
    }

    pub fn set_context(&mut self, ctx: ScreenContext) {
        self.ctx = Some(ctx.clone());
        // Apply to all screens currently on the stack.
        for screen in &mut self.screen_stack {
            screen.set_context(ctx.clone());
        }
    }

    pub fn init(&mut self) {
        self.push_screen(ScreenType::Dashboard);
    }

    pub fn push_screen(&mut self, screen_type: ScreenType) {
        let mut screen = screen_type.create_screen();
        if let Some(ctx) = &self.ctx {
            screen.set_context(ctx.clone());
        }
        self.screen_stack.push(screen);
    }

    pub fn pop_screen(&mut self) {
        if self.screen_stack.len() > 1 {
            self.screen_stack.pop();
        }
    }

    pub fn replace_screen(&mut self, screen_type: ScreenType) {
        if !self.screen_stack.is_empty() {
            self.screen_stack.pop();
        }
        self.push_screen(screen_type);
    }

    pub fn current_screen(&self) -> Option<&dyn Screen> {
        self.screen_stack.last().map(|s| s.as_ref())
    }

    pub fn current_screen_mut(&mut self) -> Option<&mut dyn Screen> {
        self.screen_stack
            .last_mut()
            .map(|s| &mut **s as &mut dyn Screen)
    }

    /// Request a background operation. Returns a request id used to match the
    /// result when it is posted back.
    pub fn request_background<F>(&mut self, label: impl Into<String>, f: F) -> u64
    where
        F: FnOnce() -> BackgroundResult + Send + 'static,
    {
        let id = {
            self.next_request_id += 1;
            self.next_request_id
        };
        let tx = self
            .event_tx
            .clone()
            .expect("event channel not initialized");
        self.pending.push((id, label.into()));
        self.busy = true;
        tokio::spawn(async move {
            let result = f();
            let (ok, message) = match &result {
                Ok(m) => (true, m.clone()),
                Err(e) => (false, e.clone()),
            };
            let _ = tx.send(AppEvent::BackgroundResult { id, ok, message });
        });
        id
    }

    /// Record a status message.
    pub fn set_status(&mut self, msg: impl Into<String>) {
        self.status = Some(msg.into());
    }

    /// Record an error message.
    pub fn set_error(&mut self, msg: impl Into<String>) {
        self.error = Some(msg.into());
    }

    /// Handle a key event. Key routing is decided by input mode in one place.
    pub fn handle_key(&mut self, key: KeyEvent) -> ScreenResult {
        // Ctrl-C always quits.
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.should_quit = true;
            return Ok(());
        }

        match self.input_mode {
            InputMode::Normal => self.handle_key_normal(key),
            InputMode::Editing | InputMode::Command => self.handle_key_editing(key),
            InputMode::Confirm => self.handle_key_confirm(key),
        }
    }

    fn handle_key_normal(&mut self, key: KeyEvent) -> ScreenResult {
        match key.code {
            KeyCode::Char(':') => {
                self.input_mode = InputMode::Command;
                self.command_buffer = TextBuffer::single_line();
                return Ok(());
            }
            KeyCode::Char('q') => {
                self.push_confirm(
                    "Quit Meld?".into(),
                    "Any unsaved work will be lost. Quit anyway?".into(),
                );
                return Ok(());
            }
            KeyCode::Char('?') => {
                self.push_screen(ScreenType::Help);
                return Ok(());
            }
            KeyCode::Esc => {
                if self.screen_stack.len() > 1 {
                    self.pop_screen();
                }
                return Ok(());
            }
            KeyCode::Tab => {
                self.focus_next();
                return Ok(());
            }
            _ => {}
        }

        // Delegate to current screen for screen-specific keys.
        if let Some(screen) = self.current_screen_mut() {
            match screen.handle_key(key)? {
                Some(action) => return self.apply_action(action),
                None => return Ok(()),
            }
        }
        Ok(())
    }

    /// Apply a ScreenAction requested by a screen.
    fn apply_action(&mut self, action: ScreenAction) -> ScreenResult {
        use crate::screens::ScreenAction::*;
        match action {
            Push(screen) => self.push_screen(screen),
            Pop => self.pop_screen(),
            Replace(screen) => self.replace_screen(screen),
            Quit => self.should_quit = true,
            Connect(profile) => self.set_status(format!("connecting to {profile}")),
            ConnectNew(config) => {
                self.set_status("connecting to new profile".to_string());
                // The app owns the connection; for now record status only.
                let _ = config;
            }
            Disconnect => self.set_status("disconnected".to_string()),
            DeleteProfile(name) => {
                self.set_status(format!("delete profile {name}"));
            }
            Snapshot(message) => self.set_status(format!("snapshot: {message}")),
            History => self.push_screen(ScreenType::History),
            Diff(a, b) => {
                let _ = (a, b);
                self.push_screen(ScreenType::Diff);
            }
            Branches => self.push_screen(ScreenType::Branches),
            Merge(source) => {
                let _ = source;
                self.push_screen(ScreenType::Merge);
            }
            Restore(commit) => {
                let _ = commit;
                self.push_screen(ScreenType::Restore);
            }
            Verify(commit) => {
                let _ = commit;
                self.push_screen(ScreenType::Verify);
            }
            Drift => self.push_screen(ScreenType::Diff),
        }
        Ok(())
    }

    fn handle_key_editing(&mut self, key: KeyEvent) -> ScreenResult {
        match key.code {
            KeyCode::Esc => {
                self.input_mode = InputMode::Normal;
                self.command_buffer = TextBuffer::single_line();
                return Ok(());
            }
            KeyCode::Enter => {
                if self.input_mode == InputMode::Command {
                    let text = self.command_buffer.text().trim().to_string();
                    self.input_mode = InputMode::Normal;
                    self.command_buffer = TextBuffer::single_line();
                    if !text.is_empty() {
                        return self.run_command(text);
                    }
                }
                return Ok(());
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.command_buffer.insert(&c.to_string());
                return Ok(());
            }
            KeyCode::Backspace => {
                self.command_buffer.delete_back();
                return Ok(());
            }
            KeyCode::Delete => {
                self.command_buffer.delete_forward();
                return Ok(());
            }
            KeyCode::Left => self.command_buffer.move_left(),
            KeyCode::Right => self.command_buffer.move_right(),
            KeyCode::Home => self.command_buffer.move_home(),
            KeyCode::End => self.command_buffer.move_end(),
            KeyCode::Tab => {
                if self.input_mode == InputMode::Command {
                    self.attempt_command_completion();
                }
                return Ok(());
            }
            _ => {}
        }
        Ok(())
    }

    fn handle_key_confirm(&mut self, key: KeyEvent) -> ScreenResult {
        if let Some(confirmed) = self.confirm.as_mut().and_then(|d| d.handle_key(key)) {
            self.pop_confirm();
            if confirmed {
                self.should_quit = true;
            }
        }
        Ok(())
    }

    /// Run one tick.
    pub fn tick(&mut self) -> ScreenResult {
        self.last_tick = Instant::now();
        if let Some(screen) = self.current_screen_mut() {
            screen.tick()?;
        }
        Ok(())
    }

    /// Draw the current frame.
    pub fn draw(&mut self, frame: &mut ratatui::Frame) {
        let area = frame.area();

        // Too small: show a message instead of panicking.
        if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
            self.draw_too_small(frame, area);
            return;
        }

        // Clear background.
        frame.render_widget(
            ratatui::widgets::Block::default().style(Styles::base()),
            area,
        );

        let chunks = ratatui::layout::Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1), // top bar
                Constraint::Min(0),    // main workspace
                Constraint::Length(1), // command bar
                Constraint::Length(1), // key hints
            ])
            .split(area);

        // Top bar: app, engine, connection, database, branch, HEAD.
        self.draw_top_bar(frame, chunks[0]);

        // Main workspace: screen stack.
        for screen in &self.screen_stack {
            screen.draw(frame, chunks[1]);
        }

        // Command bar (persistent).
        self.draw_command_bar(frame, chunks[2]);

        // Key hints.
        self.draw_key_hints(frame, chunks[3]);

        // Confirmation dialog overlay (covers the whole screen).
        if let Some(dialog) = &self.confirm {
            let popup = centered_popup(50, 22, area);
            frame.render_widget(ratatui::widgets::Clear, popup);
            dialog.draw(frame, popup);
        }
    }

    fn draw_top_bar(&mut self, frame: &mut ratatui::Frame, area: Rect) {
        let block = ratatui::widgets::Block::default()
            .borders(ratatui::widgets::Borders::ALL)
            .border_style(Styles::border())
            .style(Styles::panel());
        frame.render_widget(block.clone(), area);
        let inner = area.inner(ratatui::layout::Margin {
            vertical: 0,
            horizontal: 1,
        });

        let mut spans: Vec<ratatui::text::Span> = Vec::new();
        spans.push(ratatui::text::Span::styled(
            "MELD",
            Styles::header().add_modifier(Modifier::BOLD),
        ));
        spans.push(ratatui::text::Span::styled("  |  ", Styles::muted()));

        // Connection status.
        let connected = self.is_connected();
        let (icon, status_style, status_text) = if connected {
            ("●", Styles::success(), "connected")
        } else {
            ("○", Styles::danger(), "disconnected")
        };
        spans.push(ratatui::text::Span::styled(icon, status_style));
        spans.push(ratatui::text::Span::styled(" ", Styles::muted()));
        spans.push(ratatui::text::Span::styled(status_text, status_style));
        spans.push(ratatui::text::Span::styled("  |  ", Styles::muted()));

        // Branch / HEAD.
        let branch = self.current_branch();
        let head = self.current_head_short();
        spans.push(ratatui::text::Span::styled(
            format!(" {branch} @ {head} "),
            Styles::item_normal().add_modifier(Modifier::BOLD),
        ));

        // Busy indicator.
        if self.busy {
            spans.push(ratatui::text::Span::styled(
                "  [working]",
                Styles::warning(),
            ));
        }

        let line = ratatui::text::Line::from(spans);
        frame.render_widget(
            ratatui::widgets::Paragraph::new(line).wrap(ratatui::widgets::Wrap { trim: true }),
            inner,
        );
    }

    fn draw_command_bar(&mut self, frame: &mut ratatui::Frame, area: Rect) {
        let focused = self.input_mode == InputMode::Command;
        let style = if focused {
            Styles::input_focus()
        } else {
            Styles::input_blur()
        };
        let block = ratatui::widgets::Block::default()
            .title(if focused {
                " Command "
            } else {
                " command bar (:) "
            })
            .borders(ratatui::widgets::Borders::ALL)
            .border_style(if focused {
                crate::theme::Styles::border_focus()
            } else {
                crate::theme::Styles::border()
            })
            .style(style);
        frame.render_widget(block.clone(), area);
        let inner = area.inner(ratatui::layout::Margin {
            vertical: 0,
            horizontal: 1,
        });

        let prompt = if focused { ":" } else { "" };
        self.command_buffer.ensure_cursor_visible(inner.width);
        let hscroll = self.command_buffer.hscroll();
        let visible: String = self
            .command_buffer
            .current_line_display()
            .chars()
            .skip(hscroll as usize)
            .collect();
        let cursor_col = self.command_buffer.cursor_column().saturating_sub(hscroll);

        let mut text = format!("{prompt}{visible}");
        while (text.len() as u16) <= cursor_col.saturating_add(1) {
            text.push(' ');
        }
        let para = ratatui::widgets::Paragraph::new(text).style(style);
        frame.render_widget(para, inner);

        // Cursor position.
        if focused {
            let cursor_x = inner.x + cursor_col.min(inner.width.saturating_sub(1));
            frame.set_cursor_position((cursor_x, inner.y));
        } else {
            frame.set_cursor_position((inner.x, inner.y));
        }
    }

    fn draw_key_hints(&self, frame: &mut ratatui::Frame, area: Rect) {
        let mut hints: Vec<ratatui::text::Span> = Vec::new();
        match self.input_mode {
            InputMode::Normal => {
                hints.extend(crate::theme::keybinding_span("↑/↓", "Navigate"));
                hints.extend(crate::theme::keybinding_span("Tab", "Focus"));
                hints.extend(crate::theme::keybinding_span(":", "Command"));
                hints.extend(crate::theme::keybinding_span("q", "Quit"));
                hints.extend(crate::theme::keybinding_span("?", "Help"));
            }
            InputMode::Command | InputMode::Editing => {
                hints.extend(crate::theme::keybinding_span("Enter", "Run"));
                hints.extend(crate::theme::keybinding_span("Esc", "Cancel"));
                hints.extend(crate::theme::keybinding_span("Tab", "Complete"));
            }
            InputMode::Confirm => {
                hints.extend(crate::theme::keybinding_span("Enter", "Confirm"));
                hints.extend(crate::theme::keybinding_span("Esc", "Cancel"));
            }
        }
        if let Some(screen) = self.current_screen() {
            hints.extend(screen.key_hints());
        }
        // Cap to available width.
        let mut total = 0u16;
        hints.retain(|s| {
            let w = s.width() as u16;
            let ok = total.saturating_add(w) <= area.width;
            total = total.saturating_add(w);
            ok
        });

        let line = ratatui::text::Line::from(hints);
        frame.render_widget(
            ratatui::widgets::Paragraph::new(line).style(Styles::help()),
            area,
        );
    }

    fn draw_too_small(&self, frame: &mut ratatui::Frame, area: Rect) {
        let msg = "terminal too small (need 80×24)";
        let block = ratatui::widgets::Block::default()
            .title("Meld")
            .borders(ratatui::widgets::Borders::ALL)
            .border_style(Styles::danger())
            .style(Styles::panel());
        frame.render_widget(block.clone(), area);
        let inner = area.inner(ratatui::layout::Margin {
            vertical: 1,
            horizontal: 1,
        });
        let para = ratatui::widgets::Paragraph::new(msg)
            .style(Styles::danger())
            .alignment(ratatui::layout::Alignment::Center);
        frame.render_widget(para, inner);
    }

    // ----- confirmation dialogs -----

    fn push_confirm(&mut self, title: String, message: String) {
        self.input_mode = InputMode::Confirm;
        self.confirm = Some(ConfirmDialog::new(title, message));
    }

    fn pop_confirm(&mut self) {
        self.input_mode = InputMode::Normal;
        self.confirm = None;
    }

    fn is_connected(&self) -> bool {
        // Screens own their own connection state; the top screen may report it.
        self.current_screen()
            .map(|s| s.is_connected())
            .unwrap_or(false)
    }

    fn current_branch(&self) -> String {
        self.current_screen()
            .and_then(|s| s.branch_name())
            .unwrap_or_else(|| "main".into())
    }

    fn current_head_short(&self) -> String {
        self.current_screen()
            .and_then(|s| s.head_short())
            .unwrap_or_else(|| "–".into())
    }

    fn focus_next(&mut self) {
        if let Some(screen) = self.current_screen_mut() {
            let _ = screen.focus_next();
        }
    }

    fn run_command(&mut self, text: String) -> ScreenResult {
        match crate::command::parse_command(&text) {
            Ok(cmd) => self.dispatch_command(cmd),
            Err(e) => {
                self.set_error(e.to_string());
                Ok(())
            }
        }
    }

    fn attempt_command_completion(&mut self) {
        let text = self.command_buffer.text();
        if let Some(completed) = crate::command::complete_command(&text) {
            self.command_buffer.set_text(&completed);
        }
    }

    fn dispatch_command(&mut self, cmd: crate::command::MeldCommand) -> ScreenResult {
        use crate::command::MeldCommand::*;
        match cmd {
            Help => {
                self.push_screen(ScreenType::Help);
            }
            Connect { profile } => {
                self.set_status(format!("connecting to {profile}"));
            }
            Disconnect => {
                self.set_status("disconnected".to_string());
            }
            Snapshot { message } => {
                self.set_status(format!("snapshot: {message}"));
            }
            History => self.push_screen(ScreenType::History),
            Diff { a, b } => {
                let _ = (a, b);
                self.push_screen(ScreenType::Diff);
            }
            Branches => self.push_screen(ScreenType::Branches),
            Merge { source } => {
                let _ = source;
                self.push_screen(ScreenType::Merge);
            }
            Restore { commit } => {
                let _ = commit;
                self.push_screen(ScreenType::Restore);
            }
            Verify { commit } => {
                let _ = commit;
                self.push_screen(ScreenType::Verify);
            }
            Drift => self.push_screen(ScreenType::Diff),
            Query => self.push_screen(ScreenType::Diff),
        }
        Ok(())
    }
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

fn centered_popup(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = ratatui::layout::Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);
    ratatui::layout::Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}

/// Application entry point.
pub fn run() -> Result<(), io::Error> {
    install_panic_hook();
    let mut guard = TerminalGuard::enter()?;

    let stdout = io::stdout();
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel::<AppEvent>();

    // Forward crossterm events on a dedicated thread so `event::read()` (which
    // blocks) never stalls the async event loop.
    {
        let tx = event_tx.clone();
        std::thread::spawn(move || {
            loop {
                match event::read() {
                    Ok(Event::Key(key)) => {
                        if tx.send(AppEvent::Key(key)).is_err() {
                            break;
                        }
                    }
                    Ok(Event::Resize(cols, rows)) => {
                        if tx.send(AppEvent::Resize(cols, rows)).is_err() {
                            break;
                        }
                    }
                    Ok(_) => {}
                    Err(_) => break,
                }
            }
        });
    }

    let mut app = App::new();
    app.event_tx = Some(event_tx);
    app.init();

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let mut tick = tokio::time::interval(Duration::from_millis(100));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            // Draw.
            let _ = terminal.draw(|frame| app.draw(frame));

            tokio::select! {
                Some(ev) = event_rx.recv() => {
                    match ev {
                        AppEvent::Key(key) => {
                            if app.handle_key(key).is_err() {
                                break;
                            }
                            if app.should_quit {
                                break;
                            }
                        }
                        AppEvent::Resize(cols, rows) => {
                            app.terminal_size = ratatui::layout::Size { width: cols, height: rows };
                            let _ = terminal.autoresize();
                            if let Some(screen) = app.current_screen_mut() {
                                screen.on_resize(ratatui::layout::Size { width: cols, height: rows });
                            }
                        }
                        AppEvent::Tick => {
                            let _ = app.tick();
                        }
                        AppEvent::BackgroundResult { id, ok, message } => {
                            app.busy = app.pending.iter().any(|(pid, _)| *pid != id);
                            app.pending.retain(|(pid, _)| *pid != id);
                            if ok {
                                app.set_status(message);
                            } else {
                                app.set_error(message);
                            }
                        }
                    }
                }
                _ = tick.tick() => {
                    let _ = app.tick();
                }
            }
        }
    });

    // Cleanup via guard drop.
    let _ = guard.restore();
    Ok(())
}
