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
use std::path::PathBuf;
use std::time::{Duration, Instant};
use std::thread;

use crate::components::ConfirmDialog;
use crate::event::{AppEvent, BackgroundKind};
use crate::input::{InputMode, TextBuffer};
use crate::screens::{Screen, ScreenAction, ScreenContext, ScreenResult, ScreenType};
use crate::terminal::{TerminalGuard, install_panic_hook};
use crate::theme::Styles;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers, KeyEventKind};
use meld_adapter::{ConnectionConfig, DatabaseAdapter, MongoConfig, PostgresConfig, QueryRequest};
use meld_adapter::registry::AdapterRegistry;
use meld_core::ids::{CommitHash, Hash};
use meld_core::state::EngineKind;
use meld_engine::{checkout, diff_cmd, drift, snapshot, verify_cmd};
use meld_store::connection_store;
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Rect},
    style::Modifier,
    widgets::{Block, Borders, Paragraph},
    Frame,
};
use serde_json::json;
use tracing::{debug, warn};

/// A password prompt shown when reconnecting a saved profile. Passwords are
/// never persisted; they are held only in memory for the duration of the
/// connect attempt.
pub struct PasswordPrompt {
    profile: String,
    buffer: TextBuffer,
}

/// Minimum supported terminal size.
const MIN_WIDTH: u16 = 80;
const MIN_HEIGHT: u16 = 24;

/// Result of a background task posted back to the event loop.
type BackgroundResult = Result<String, String>;

/// Result of taking session parts: store path, adapter, and connection config.
type SessionParts = (
    PathBuf,
    Option<Box<dyn DatabaseAdapter>>,
    Option<ConnectionConfig>,
);

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
    /// Time the error was first displayed, so the tick can auto-clear it
    /// after a few seconds instead of clearing it on every key press.
    error_at: Option<Instant>,
    /// Cached name of the currently connected profile. Updated in the
    /// `BackgroundResult` arm for Connect/Disconnect so that `is_connected`
    /// and the draw path never need to block on the session mutex.
    connection_name: Option<String>,
    /// Active confirmation dialog, if any.
    pub confirm: Option<ConfirmDialog>,
    /// Profile name awaiting deletion confirmation.
    pub confirm_delete: Option<String>,
    /// Config for the in-flight connect, used to persist a profile on success.
    pub connect_config: Option<ConnectionConfig>,
    /// Password prompt for reconnecting a saved profile (password not persisted).
    pub password_prompt: Option<PasswordPrompt>,
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
            error_at: None,
            connection_name: None,
            confirm: None,
            confirm_delete: None,
            connect_config: None,
            password_prompt: None,
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
        self.pending.push((id, label.into()));
        self.busy = true;
        let tx = self
            .event_tx
            .clone()
            .expect("event channel not initialized");
        tokio::task::spawn_blocking(move || {
            let result = f();
            let (ok, message) = match &result {
                Ok(m) => (true, m.clone()),
                Err(e) => (false, e.clone()),
            };
            let _ = tx.send(AppEvent::BackgroundResult {
                id,
                ok,
                message,
                kind: BackgroundKind::Other,
            });
        });
        id
    }

    /// Request an async background operation. Returns a request id used to
    /// match the result when it is posted back.
    pub fn request_background_async<F>(&mut self, label: impl Into<String>, fut: F) -> u64
    where
        F: std::future::Future<Output = BackgroundResult> + Send + 'static,
    {
        let id = {
            self.next_request_id += 1;
            self.next_request_id
        };
        self.pending.push((id, label.into()));
        self.busy = true;
        let tx = self
            .event_tx
            .clone()
            .expect("event channel not initialized");
        tokio::spawn(async move {
            let result = fut.await;
            let (ok, message) = match &result {
                Ok(m) => (true, m.clone()),
                Err(e) => (false, e.clone()),
            };
            let _ = tx.send(AppEvent::BackgroundResult {
                id,
                ok,
                message,
                kind: BackgroundKind::Other,
            });
        });
        id
    }

    /// Synchronously takes the store path, adapter, and connection config out
    /// of the session, releasing the `MutexGuard` before any async work. This
    /// avoids holding a `std::sync::MutexGuard` across an `.await` point,
    /// which is not `Send` when `Session: !Send`.
    fn take_session_parts(ctx: &ScreenContext) -> io::Result<SessionParts> {
        let mut session = ctx.session.lock().map_err(|e| io::Error::other(e.to_string()))?;
        let path = session.store.path().to_path_buf();
        let adapter = session.take_adapter();
        let config = session.connection.clone();
        Ok((path, adapter, config))
    }

    /// Record a status message.
    pub fn set_status(&mut self, msg: impl Into<String>) {
        self.status = Some(msg.into());
    }

    /// Record an error message.
    pub fn set_error(&mut self, msg: impl Into<String>) {
        self.error = Some(msg.into());
        self.error_at = Some(Instant::now());
    }

    /// Handle a key event. Key routing is decided by input mode in one place.
    pub fn handle_key(&mut self, key: KeyEvent) -> ScreenResult {
        // Ctrl-C always quits.
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.should_quit = true;
            return Ok(());
        }
        // Clear a displayed error only when the user starts a new action
        // (Enter, ':'); it otherwise persists until the tick clears it, so a
        // failed connect attempt keeps its error visible.
        if key.code == KeyCode::Enter || key.code == KeyCode::Char(':') {
            self.error = None;
            self.error_at = None;
        }

        // A password prompt intercepts all input until dismissed.
        if self.password_prompt.is_some() {
            return self.handle_key_password_prompt(key);
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
        debug!(action = ?action, "apply_action: dispatching screen action");
        match action {
            Push(screen) => {
                debug!(screen = ?screen, "apply_action: Push screen");
                self.push_screen(screen)
            }
            Pop => {
                debug!("apply_action: Pop screen");
                self.pop_screen()
            }
            Replace(screen) => {
                debug!(screen = ?screen, "apply_action: Replace screen");
                self.replace_screen(screen)
            }
            Quit => {
                debug!("apply_action: Quit");
                self.should_quit = true
            }
            Connect(profile) => {
                debug!(profile = %profile, "apply_action: Connect(profile)");
                self.connect_profile(profile)?
            }
            ConnectNew(config) => {
                let name = match &config {
                    ConnectionConfig::PostgreSql(pg) => &pg.name,
                    ConnectionConfig::MongoDb(mongo) => &mongo.name,
                };
                debug!(name = %name, "apply_action: ConnectNew(config)");
                self.start_connect(config);
            }
            Disconnect => {
                debug!("apply_action: Disconnect");
                let Some(ctx) = self.ctx.clone() else {
                    self.set_error("Session is not initialized.".to_string());
                    return Ok(());
                };
                let tx = match self.event_tx.clone() {
                    Some(tx) => tx,
                    None => {
                        self.set_error("Event channel is not initialized.".to_string());
                        return Ok(());
                    }
                };
                debug!("apply_action: Disconnect acquiring session lock");
                let adapter = {
                    let mut session = ctx
                        .session
                        .lock()
                        .expect("session lock poisoned");
                    debug!("apply_action: Disconnect session lock acquired");
                    session.take_adapter()
                };
                debug!("apply_action: Disconnect session lock released");
                self.next_request_id += 1;
                let id = self.next_request_id;
                self.pending.push((id, "Disconnect".to_string()));
                self.busy = true;
                self.set_status("Disconnecting...");
                tokio::spawn(async move {
                    let result = (async {
                        if let Some(mut adapter) = adapter {
                            adapter
                                .disconnect()
                                .await
                                .map_err(|e| e.to_string())?;
                        }
                        Ok::<_, String>("Disconnected".into())
                    }).await;
                    let (ok, message) = match result {
                        Ok(message) => (true, message),
                        Err(error) => (false, format!("Disconnect failed: {error}")),
                    };
                    let send_result = tx.send(AppEvent::BackgroundResult {
                        id,
                        ok,
                        message,
                        kind: BackgroundKind::Disconnect,
                    });
                    if send_result.is_err() {
                        warn!(id, "apply_action: Disconnect tx.send(BackgroundResult) failed - channel closed");
                    }
                });
            }
            DeleteProfile(name) => {
                debug!(name = %name, "apply_action: DeleteProfile");
                self.confirm_delete = Some(name.clone());
                self.push_confirm(
                    format!("Delete profile {name}?"),
                    "This cannot be undone.".into(),
                );
            }
            Snapshot(message) => {
                debug!(message = %message, "apply_action: Snapshot");
                let ctx = match self.ctx.clone() {
                    Some(c) => c,
                    None => {
                        self.set_error("Session is not initialized.".to_string());
                        return Ok(());
                    }
                };
                let (store_path, adapter, config) = Self::take_session_parts(&ctx)?;
                self.request_background_async(format!("snapshot: {message}"), async move {
                    let mut session = meld_engine::Session::open(store_path).map_err(|e| e.to_string())?;
                    if let Some(adapter) = adapter {
                        let cfg = config.ok_or_else(|| "no connection config".to_string())?;
                        session.install_adapter(adapter, cfg);
                    }
                    let result = snapshot::snapshot(&mut session, &message, "meld").await.map_err(|e| e.to_string())?;
                    Ok(format!("committed {} (root {})", result.commit_hash.short(12), result.root_hash.short(12)))
                });
            }
            History => {
                debug!("apply_action: History");
                self.push_screen(ScreenType::History)
            }
            Diff(a, b) => {
                debug!(a = %a, b = %b, "apply_action: Diff");
                let from = CommitHash(Hash::from_hex(&a).map_err(|e| io::Error::other(e.to_string()))?);
                let to = CommitHash(Hash::from_hex(&b).map_err(|e| io::Error::other(e.to_string()))?);
                let ctx = match self.ctx.clone() {
                    Some(c) => c,
                    None => {
                        self.set_error("Session is not initialized.".to_string());
                        return Ok(());
                    }
                };
                let store_path = {
                    let session = ctx.session.lock().map_err(|e| io::Error::other(format!("session lock failed: {e}")))?;
                    session.store.path().to_path_buf()
                };
                self.request_background_async(format!("diff {a} {b}"), async move {
                    let session = meld_engine::Session::open(store_path).map_err(|e| e.to_string())?;
                    let diff = diff_cmd::diff(&session.store, &from, &to).await.map_err(|e| e.to_string())?;
                    Ok(format!("diff summary: {} changes", diff.summary.total_changes()))
                });
                self.push_screen(ScreenType::Diff);
            }
            Branches => {
                debug!("apply_action: Branches");
                self.push_screen(ScreenType::Branches)
            }
            BranchCreate { name, target } => {
                debug!(name = %name, ?target, "apply_action: BranchCreate");
                let ctx = match self.ctx.clone() {
                    Some(c) => c,
                    None => {
                        self.set_error("Session is not initialized.".to_string());
                        return Ok(());
                    }
                };
                let target = match target {
                    Some(t) => Some(CommitHash(Hash::from_hex(&t).map_err(|e| io::Error::other(e.to_string()))?)),
                    None => None,
                };
                let store_path = {
                    let session = ctx.session.lock().map_err(|e| io::Error::other(format!("session lock failed: {e}")))?;
                    session.store.path().to_path_buf()
                };
                self.request_background_async(format!("branch create {name}"), async move {
                    let mut session = meld_engine::Session::open(store_path).map_err(|e| e.to_string())?;
                    session.create_branch(&name, target).await.map_err(|e| e.to_string())?;
                    Ok(format!("branch '{name}' created"))
                });
            }
            BranchSwitch { name } => {
                debug!(name = %name, "apply_action: BranchSwitch");
                let ctx = match self.ctx.clone() {
                    Some(c) => c,
                    None => {
                        self.set_error("Session is not initialized.".to_string());
                        return Ok(());
                    }
                };
                let store_path = {
                    let session = ctx.session.lock().map_err(|e| io::Error::other(format!("session lock failed: {e}")))?;
                    session.store.path().to_path_buf()
                };
                self.request_background_async(format!("branch switch {name}"), async move {
                    let mut session = meld_engine::Session::open(store_path).map_err(|e| e.to_string())?;
                    session.switch_branch(&name).await.map_err(|e| e.to_string())?;
                    Ok(format!("switched to branch '{name}'"))
                });
            }
            Merge(source) => {
                debug!(source = %source, "apply_action: Merge");
                self.push_screen(ScreenType::Merge)
            }
            Restore(commit) => {
                debug!(commit = ?commit, "apply_action: Restore");
                self.push_screen(ScreenType::Restore);
                let ctx = match self.ctx.clone() {
                    Some(c) => c,
                    None => {
                        self.set_error("Session is not initialized.".to_string());
                        return Ok(());
                    }
                };
                let (store_path, adapter, config) = Self::take_session_parts(&ctx)?;
                self.request_background_async(format!("restore {commit}"), async move {
                    let mut session = meld_engine::Session::open(store_path).map_err(|e| e.to_string())?;
                    if let Some(adapter) = adapter {
                        let cfg = config.ok_or_else(|| "no connection config".to_string())?;
                        session.install_adapter(adapter, cfg);
                    }
                    checkout::checkout(&mut session, &commit).await.map_err(|e| e.to_string())?;
                    Ok(format!("restored to {commit}"))
                });
            }
            Verify(commit_opt) => {
                debug!(commit = ?commit_opt, "apply_action: Verify");
                let commit = match commit_opt {
                    Some(c) => c,
                    None => match self.ctx.as_ref()
                        .and_then(|c| c.session.lock().ok())
                        .and_then(|s| s.store.get_head().ok())
                        .flatten()
                    {
                        Some(h) => h,
                        None => {
                            self.set_error("HEAD is empty".to_string());
                            return Ok(());
                        }
                    },
                };
                let ctx = match self.ctx.clone() {
                    Some(c) => c,
                    None => {
                        self.set_error("Session is not initialized.".to_string());
                        return Ok(());
                    }
                };
                let (store_path, adapter, config) = Self::take_session_parts(&ctx)?;
                self.request_background_async(format!("verify {commit}"), async move {
                    let mut session = meld_engine::Session::open(store_path).map_err(|e| e.to_string())?;
                    if let Some(adapter) = adapter {
                        let cfg = config.ok_or_else(|| "no connection config".to_string())?;
                        session.install_adapter(adapter, cfg);
                    }
                    let adapter = session.adapter().map_err(|e| e.to_string())?;
                    let report = verify_cmd::verify(adapter, &session.store, &commit).await.map_err(|e| e.to_string())?;
                    if report.passed {
                        Ok(format!("verify passed for {commit}"))
                    } else {
                        Err(format!("verify failed for {commit}: {:?}", report.mismatches))
                    }
                });
                self.push_screen(ScreenType::Verify);
            }
            Drift => {
                debug!("apply_action: Drift");
                let ctx = match self.ctx.clone() {
                    Some(c) => c,
                    None => {
                        self.set_error("Session is not initialized.".to_string());
                        return Ok(());
                    }
                };
                self.request_background_async("drift", async move {
                    let (store_path, adapter, config) = Self::take_session_parts(&ctx).map_err(|e| e.to_string())?;
                    let mut session = meld_engine::Session::open(store_path).map_err(|e| e.to_string())?;
                    if let Some(adapter) = adapter {
                        let cfg = config.ok_or_else(|| "no connection config".to_string())?;
                        session.install_adapter(adapter, cfg);
                    }
                    let diff = drift::drift_head(&session).await.map_err(|e| e.to_string())?;
                    Ok(format!("drift: {} changes", diff.summary.total_changes()))
                });
                self.push_screen(ScreenType::Diff);
            }
            Query { sql } => {
                debug!(sql = %sql, "apply_action: Query");
                let request = QueryRequest::PostgreSql { sql: sql.clone(), read_only: true };
                let ctx = self.ctx.clone().ok_or_else(|| io::Error::other("no session"))?;
                let (store_path, adapter, config) = Self::take_session_parts(&ctx)?;
                self.request_background_async("query", async move {
                    let mut session = meld_engine::Session::open(store_path).map_err(|e| e.to_string())?;
                    if let Some(adapter) = adapter {
                        let cfg = config.ok_or_else(|| "no connection config".to_string())?;
                        session.install_adapter(adapter, cfg);
                    }
                    let result = session.execute_query(request).await.map_err(|e| e.to_string())?;
                    Ok(format!("query returned {} rows in {}ms", result.row_count, result.duration_ms))
                });
                self.push_screen(ScreenType::Diff);
            }
        }
        Ok(())
    }

    /// Begin a connect attempt on the main runtime, mirroring the Fix 1 path.
    /// The result is posted back as an `AppEvent::BackgroundResult` of kind
    /// `Connect`. `config` is remembered so a successful new connection can be
    /// persisted as a profile.
    fn start_connect(&mut self, config: ConnectionConfig) {
        let Some(ctx) = self.ctx.clone() else {
            self.set_error("Session is not initialized.".to_string());
            return;
        };
        let tx = match self.event_tx.clone() {
            Some(tx) => tx,
            None => {
                self.set_error("Event channel is not initialized.".to_string());
                return;
            }
        };

        self.next_request_id += 1;
        let id = self.next_request_id;
        self.pending.push((id, "Connect to database".to_string()));
        self.busy = true;
        self.set_status("Connecting to database...");
        self.connect_config = Some(config.clone());

        let name = match &config {
            ConnectionConfig::PostgreSql(pg) => pg.name.clone(),
            ConnectionConfig::MongoDb(mongo) => mongo.name.clone(),
        };
        let host = match &config {
            ConnectionConfig::PostgreSql(pg) => pg.host.clone(),
            ConnectionConfig::MongoDb(mongo) => mongo.host.clone().unwrap_or_default(),
        };
        let port = match &config {
            ConnectionConfig::PostgreSql(pg) => pg.port,
            ConnectionConfig::MongoDb(mongo) => mongo.port.unwrap_or(0),
        };
        let database = match &config {
            ConnectionConfig::PostgreSql(pg) => pg.database.clone(),
            ConnectionConfig::MongoDb(mongo) => mongo.database.clone(),
        };
        debug!(
            id,
            name = %name,
            host = %host,
            port,
            database = %database,
            "start_connect: spawn task on main runtime"
        );
        let thread_id = thread::current().id();
        let in_runtime = tokio::runtime::Handle::try_current().is_ok();
        debug!(
            id,
            thread_id = ?thread_id,
            in_runtime_handle = in_runtime,
            "start_connect: spawn context"
        );

        let config = config.clone();
        tokio::spawn(async move {
            debug!(id, "spawned connect task: beginning connect flow");
            let result: Result<String, String> = (async {
                let before_connect = Instant::now();
                debug!(id, "AdapterRegistry::connect: before call");
                let connect_result = tokio::time::timeout(
                    Duration::from_secs(15),
                    AdapterRegistry::new().connect(&config),
                )
                .await;
                let elapsed = before_connect.elapsed();
                match &connect_result {
                    Ok(Ok(_)) => debug!(id, elapsed_ms = elapsed.as_millis(), "AdapterRegistry::connect: success"),
                    Ok(Err(e)) => debug!(id, elapsed_ms = elapsed.as_millis(), error = %e, "AdapterRegistry::connect: adapter error"),
                    Err(_) => debug!(id, elapsed_ms = elapsed.as_millis(), "AdapterRegistry::connect: TIMEOUT after 15s"),
                }
                let (adapter, info) = connect_result
                    .map_err(|_| "connection timed out after 15s".to_string())?
                    .map_err(|e| e.to_string())?;

                let before_lock = Instant::now();
                debug!(id, "install_adapter: before session lock");
                let old = {
                    let mut session = ctx
                        .session
                        .lock()
                        .map_err(|e| format!("Session lock failed: {e}"))?;
                    let lock_elapsed = before_lock.elapsed();
                    if lock_elapsed.as_millis() > 50 {
                        warn!(id, lock_ms = lock_elapsed.as_millis(), "install_adapter: session lock held >50ms");
                    }
                    debug!(id, lock_ms = lock_elapsed.as_millis(), "install_adapter: session lock acquired");
                    session.install_adapter(adapter, config)
                };
                let lock_elapsed = before_lock.elapsed();
                debug!(id, lock_ms = lock_elapsed.as_millis(), "install_adapter: session lock released");

                // Disconnect the previous adapter outside the session lock.
                if let Some((mut old_adapter, _old_config)) = old
                    && let Err(e) = old_adapter.disconnect().await
                {
                    tracing::warn!(error = %e, "failed to disconnect previous adapter");
                }

                Ok(format!(
                    "Connected to {} ({})",
                    info.database_name, info.version
                ))
            }).await;

            let (ok, message) = match result {
                Ok(message) => (true, message),
                Err(error) => (false, format!("Connection failed: {error}")),
            };

            debug!(id, ok, message = %message, "start_connect: sending BackgroundResult to event loop");
            let send_result = tx.send(AppEvent::BackgroundResult {
                id,
                ok,
                message,
                kind: BackgroundKind::Connect,
            });
            if send_result.is_err() {
                warn!(id, "start_connect: tx.send(BackgroundResult) failed - channel closed");
            }
        });
    }

    /// Reconnect a saved profile: the password is prompted separately and is
    /// never persisted.
    fn connect_profile(&mut self, profile: String) -> ScreenResult {
        debug!(profile = %profile, "connect_profile: attempting to reconnect saved profile");
        let Some(ctx) = self.ctx.clone() else {
            self.set_error("Session is not initialized.".to_string());
            return Ok(());
        };
        let saved = {
            debug!(profile = %profile, "connect_profile: acquiring session lock");
            let Ok(session) = ctx.session.lock() else {
                self.set_error("Session lock failed.".to_string());
                return Ok(());
            };
            debug!(profile = %profile, "connect_profile: session lock acquired");
            match connection_store::get_profile(session.store.db(), &profile) {
                Ok(Some(saved)) => saved,
                _ => {
                    self.set_error(format!("profile {profile} not found"));
                    return Ok(());
                }
            }
        }; // session lock released here
        debug!(profile = %profile, "connect_profile: profile found in store; session lock released");
        if let Err(e) = Self::profile_to_config(&saved) {
            self.set_error(e);
            return Ok(());
        }
        debug!(profile = %profile, "connect_profile: showing password modal");
        self.password_prompt = Some(PasswordPrompt {
            profile: saved.name.clone(),
            buffer: TextBuffer::single_line(),
        });
        Ok(())
    }

    /// Build a `ConnectionConfig` from a saved profile's params. Passwords are
    /// never stored, so they start as `None` and must be prompted.
    fn profile_to_config(
        profile: &connection_store::ConnectionProfile,
    ) -> Result<ConnectionConfig, String> {
        let params = &profile.params;
        let get_str = |key: &str| {
            params
                .get(key)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string()
        };
        let get_opt_str = |key: &str| {
            params
                .get(key)
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
        };
        let host = get_str("host");
        // Default the port per engine: MongoDB 27017, PostgreSQL 5432.
        let port: u16 = params
            .get("port")
            .and_then(|v| v.as_u64())
            .map(|v| v as u16)
            .unwrap_or_else(|| match profile.engine {
                EngineKind::MongoDb => 27017,
                EngineKind::PostgreSql => 5432,
            });
        let database = get_str("database");
        // Both engines store the username under "user".
        let user = get_str("user");
        let uri = get_opt_str("uri");
        let auth_source = get_opt_str("auth_source");
        match profile.engine {
            EngineKind::PostgreSql => Ok(ConnectionConfig::PostgreSql(PostgresConfig {
                name: profile.name.clone(),
                host,
                port,
                user,
                password: None,
                database,
                uri,
            })),
            EngineKind::MongoDb => Ok(ConnectionConfig::MongoDb(MongoConfig {
                name: profile.name.clone(),
                uri,
                host: if host.is_empty() { None } else { Some(host) },
                port: if port == 0 { None } else { Some(port) },
                database,
                username: if user.is_empty() { None } else { Some(user) },
                password: None,
                auth_source,
            })),
        }
    }

    /// Persist a newly connected profile (without its password) and refresh
    /// the connections list. The session mutex is held only in tight, scoped
    /// sections and never while calling screen methods, so this can never
    /// deadlock the event loop.
    fn save_new_connection(&mut self, config: &ConnectionConfig) {
        debug!("save_new_connection: start");
        let ctx = match self.ctx.clone() {
            Some(c) => c,
            None => return,
        };
        let engine = match config {
            ConnectionConfig::PostgreSql(_) => EngineKind::PostgreSql,
            ConnectionConfig::MongoDb(_) => EngineKind::MongoDb,
        };
        let name = match config {
            ConnectionConfig::PostgreSql(pg) => pg.name.clone(),
            ConnectionConfig::MongoDb(mongo) => mongo.name.clone(),
        };
        let mut params = serde_json::Map::new();
        match config {
            ConnectionConfig::PostgreSql(pg) => {
                params.insert("host".into(), json!(pg.host));
                params.insert("port".into(), json!(pg.port));
                params.insert("user".into(), json!(pg.user));
                params.insert("database".into(), json!(pg.database));
                if let Some(u) = &pg.uri {
                    params.insert("uri".into(), json!(u));
                }
            }
            ConnectionConfig::MongoDb(mongo) => {
                if let Some(h) = &mongo.host {
                    params.insert("host".into(), json!(h));
                }
                if let Some(p) = mongo.port {
                    params.insert("port".into(), json!(p));
                }
                params.insert("database".into(), json!(mongo.database));
                if let Some(u) = &mongo.username {
                    params.insert("user".into(), json!(u));
                }
                if let Some(u) = &mongo.uri {
                    params.insert("uri".into(), json!(u));
                }
                if let Some(a) = &mongo.auth_source {
                    params.insert("auth_source".into(), json!(a));
                }
            }
        }
        params.insert("engine".into(), json!(engine));

        // Preserve created_at when re-saving an existing profile; only
        // updated_at is refreshed.
        let created_at = {
            debug!("save_new_connection: acquiring session lock (created_at lookup)");
            let Ok(session) = ctx.session.lock() else {
                self.set_error("Session lock failed.".to_string());
                return;
            };
            debug!("save_new_connection: session lock acquired (created_at lookup)");
            connection_store::get_profile(session.store.db(), &name)
                .ok()
                .flatten()
                .map(|p| p.created_at)
                .unwrap_or_else(chrono::Utc::now)
        }; // guard released here
        debug!("save_new_connection: session lock released (created_at lookup)");

        let profile = connection_store::ConnectionProfile {
            name,
            engine,
            params: serde_json::Value::Object(params),
            created_at,
            updated_at: chrono::Utc::now(),
        };

        // Tight scope: lock, save, release the guard before touching any
        // screen (which may lock the session again).
        let save_result = {
            debug!("save_new_connection: acquiring session lock (save_profile)");
            let session = ctx.session.lock().expect("session lock poisoned");
            debug!("save_new_connection: session lock acquired (save_profile)");
            let result = connection_store::save_profile(session.store.db(), &profile);
            debug!("save_new_connection: releasing session lock (save_profile)");
            result
        }; // guard dropped here
        debug!("save_new_connection: session lock released (save_profile)");

        match save_result {
            Ok(()) => {
                debug!("save_new_connection: profile saved; refreshing connections screen (lock NOT held)");
                self.refresh_connections_screen();
                debug!("save_new_connection: refresh done");
            }
            Err(e) => self.set_error(format!("failed to save profile: {e}")),
        }
        debug!("save_new_connection: end");
    }

    /// Reload the profiles on any ConnectionsScreen on the stack.
    fn refresh_connections_screen(&mut self) {
        debug!("refresh_connections_screen: start");
        let ctx = match self.ctx.clone() {
            Some(c) => c,
            None => return,
        };
        for screen in &mut self.screen_stack {
            if screen.title() == "Connections" {
                debug!("refresh_connections_screen: calling ConnectionsScreen::set_context");
                screen.set_context(ctx.clone());
            }
        }
        debug!("refresh_connections_screen: end");
    }

    /// Handle keys while a password prompt is active.
    fn handle_key_password_prompt(&mut self, key: KeyEvent) -> ScreenResult {
        match key.code {
            KeyCode::Enter => {
                let Some(prompt) = self.password_prompt.take() else {
                    return Ok(());
                };
                self.input_mode = InputMode::Normal;
                let profile_name = prompt.profile.clone();
                let password = if prompt.buffer.text().trim().is_empty() {
                    None
                } else {
                    Some(prompt.buffer.text().trim().to_string())
                };
                let Some(ctx) = self.ctx.clone() else { return Ok(()); };
                let saved = {
                    debug!(profile = %profile_name, "handle_key_password_prompt: acquiring session lock");
                    let Ok(session) = ctx.session.lock() else {
                        self.set_error("Session lock failed.".to_string());
                        return Ok(());
                    };
                    debug!(profile = %profile_name, "handle_key_password_prompt: session lock acquired");
                    match connection_store::get_profile(session.store.db(), &profile_name) {
                        Ok(Some(saved)) => saved,
                        _ => {
                            self.set_error(format!("profile {profile_name} not found"));
                            return Ok(());
                        }
                    }
                }; // session lock released here
                debug!(profile = %profile_name, "handle_key_password_prompt: profile found; session lock released");
                let mut config = match Self::profile_to_config(&saved) {
                    Ok(c) => c,
                    Err(e) => {
                        self.set_error(e);
                        return Ok(());
                    }
                };
                match &mut config {
                    ConnectionConfig::PostgreSql(pg) => pg.password = password,
                    ConnectionConfig::MongoDb(mongo) => mongo.password = password,
                }
                self.start_connect(config);
                Ok(())
            }
            KeyCode::Esc => {
                self.password_prompt = None;
                self.input_mode = InputMode::Normal;
                Ok(())
            }
            KeyCode::Char(c)
                if !key.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                if let Some(prompt) = self.password_prompt.as_mut() {
                    prompt.buffer.insert(&c.to_string());
                }
                Ok(())
            }
            KeyCode::Backspace => {
                if let Some(prompt) = self.password_prompt.as_mut() {
                    prompt.buffer.delete_back();
                }
                Ok(())
            }
            KeyCode::Delete => {
                if let Some(prompt) = self.password_prompt.as_mut() {
                    prompt.buffer.delete_forward();
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn draw_password_prompt(&self, frame: &mut Frame, area: Rect, prompt: &PasswordPrompt) {
        let popup = centered_popup(50, 12, area);
        frame.render_widget(ratatui::widgets::Clear, popup);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Styles::border_focus())
            .title("Enter password");
        frame.render_widget(block.clone(), popup);
        let inner = block.inner(popup);
        let chunks = ratatui::layout::Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Length(3),
                Constraint::Length(3),
            ])
            .split(inner);
        let masked = "•".repeat(prompt.buffer.text().chars().count());
        let profile = Paragraph::new(format!("Profile: {}", prompt.profile))
            .style(Styles::muted());
        frame.render_widget(profile, chunks[0]);
        let input = Paragraph::new(format!("password: {masked}")).style(Styles::input_focus());
        frame.render_widget(input, chunks[1]);
        let hints = Paragraph::new("Enter: connect    Esc: cancel")
            .style(Styles::help());
        frame.render_widget(hints, chunks[2]);
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
        // Auto-clear a persistent error after a few seconds.
        if self.error.is_some()
            && self.error_at
                .map(|t| t.elapsed() > Duration::from_secs(5))
                .unwrap_or(false)
        {
            self.error = None;
            self.error_at = None;
        }
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
                Constraint::Length(3), // bordered top bar: top + content + bottom
                Constraint::Min(8),    // main workspace always retains useful space
                Constraint::Length(3), // bordered command bar: top + content + bottom
                Constraint::Length(1), // status line
                Constraint::Length(1), // key hints
            ])
            .split(area);

        // Top bar: app, engine, connection, database, branch, HEAD.
        self.draw_top_bar(frame, chunks[0]);

        // Screens are opaque views. Draw only the active screen.
        if let Some(screen) = self.screen_stack.last() {
            screen.draw(frame, chunks[1]);
        }

        // Command bar (persistent).
        self.draw_command_bar(frame, chunks[2]);

        // Status line: last status message or error (danger style).
        self.draw_status(frame, chunks[3]);

        // Key hints.
        self.draw_key_hints(frame, chunks[4]);

        // Confirmation dialog overlay (covers the whole screen).
        if let Some(dialog) = &self.confirm {
            let popup = centered_popup(50, 22, area);
            frame.render_widget(ratatui::widgets::Clear, popup);
            dialog.draw(frame, popup);
        }

        // Password prompt overlay (covers the whole screen).
        if let Some(prompt) = &self.password_prompt {
            self.draw_password_prompt(frame, area, prompt);
        }
    }

    fn draw_top_bar(&mut self, frame: &mut ratatui::Frame, area: Rect) {
        let block = ratatui::widgets::Block::default()
            .borders(ratatui::widgets::Borders::ALL)
            .border_style(Styles::border())
            .style(Styles::panel());
        frame.render_widget(block.clone(), area);
        let inner = area.inner(ratatui::layout::Margin {
            vertical: 1,
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
            vertical: 1,
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
        let prompt_width = prompt.chars().count() as u16;
        let cursor_col = self.command_buffer.cursor_column().saturating_sub(hscroll);

        let mut text = format!("{prompt}{visible}");
        while (text.len() as u16) <= cursor_col.saturating_add(1) {
            text.push(' ');
        }
        let para = ratatui::widgets::Paragraph::new(text).style(style);
        frame.render_widget(para, inner);

        // Cursor position.
        if focused {
            let cursor_x = inner.x
                + prompt_width
                + cursor_col.min(inner.width.saturating_sub(prompt_width + 1));
            frame.set_cursor_position((cursor_x, inner.y));
        } else {
            frame.set_cursor_position((inner.x, inner.y));
        }
    }

    /// Render the last status message or error on the status line.
    fn draw_status(&mut self, frame: &mut ratatui::Frame, area: Rect) {
        match (&self.error, &self.status) {
            (Some(error), _) => {
                let para = ratatui::widgets::Paragraph::new(format!("Error: {error}"))
                    .style(Styles::danger());
                frame.render_widget(para, area);
            }
            (None, Some(status)) => {
                let para = ratatui::widgets::Paragraph::new(status.clone()).style(Styles::info());
                frame.render_widget(para, area);
            }
            (None, None) => {}
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
        if self.connection_name.is_some() {
            return true;
        }
        // Fall back to the live session without ever blocking: a slow or
        // contended lock must never freeze rendering.
        self.ctx
            .as_ref()
            .and_then(|c| c.session.try_lock().ok())
            .map(|session| session.adapter.is_some())
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
        let action = match cmd {
            Help => {
                self.push_screen(ScreenType::Help);
                return Ok(());
            }
            Connect { profile } => return self.connect_profile(profile),
            Disconnect => ScreenAction::Disconnect,
            Snapshot { message } => ScreenAction::Snapshot(message),
            History => ScreenAction::History,
            Diff { a, b } => ScreenAction::Diff(a, b),
            Branches => ScreenAction::Branches,
            BranchCreate { name, target } => ScreenAction::BranchCreate { name, target },
            BranchSwitch { name } => ScreenAction::BranchSwitch { name },
            Merge { source } => ScreenAction::Merge(source),
            Restore { commit } => ScreenAction::Restore(CommitHash(
                Hash::from_hex(&commit).map_err(|e| io::Error::other(e.to_string()))?,
            )),
            Verify { commit } => match commit {
                Some(c) => ScreenAction::Verify(Some(CommitHash(
                    Hash::from_hex(&c).map_err(|e| io::Error::other(e.to_string()))?,
                ))),
                None => ScreenAction::Verify(None),
            },
            Drift => ScreenAction::Drift,
            Query { sql } => ScreenAction::Query { sql },
        };
        self.apply_action(action)
    }
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::screens::ScreenType;
    use meld_adapter::{ConnectionConfig, PostgresConfig};
    use std::sync::mpsc;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    use tempfile::TempDir;

    fn test_app() -> (App, TempDir) {
        let dir = TempDir::new().unwrap();
        let session = meld_engine::Session::open(dir.path().join(".meld")).unwrap();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<AppEvent>();
        let ctx = ScreenContext {
            session: Arc::new(Mutex::new(session)),
            tx,
        };
        let mut app = App::new();
        app.set_context(ctx);
        app.push_screen(ScreenType::Dashboard);
        app.push_screen(ScreenType::Connections);
        (app, dir)
    }

    #[test]
    fn save_new_connection_completes_without_deadlock() {
        let (mut app, _dir) = test_app();
        let config = ConnectionConfig::PostgreSql(PostgresConfig {
            name: "regression-test".into(),
            host: "localhost".into(),
            port: 5432,
            user: "meld".into(),
            password: None,
            database: "meld".into(),
            uri: None,
        });

        let (done_tx, done_rx) = mpsc::channel();
        let handle = std::thread::spawn(move || {
            app.save_new_connection(&config);
            let _ = done_tx.send(());
        });

        assert!(
            done_rx.recv_timeout(Duration::from_secs(2)).is_ok(),
            "save_new_connection deadlocked on the session mutex"
        );
        handle.join().unwrap();
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
                        // Only forward Press (and Repeat) events: key-release
                        // events (e.g. on Windows) must not double-submit or
                        // clear errors.
                        if !matches!(
                            key.kind,
                            KeyEventKind::Press | KeyEventKind::Repeat
                        ) {
                            continue;
                        }
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
    app.event_tx = Some(event_tx.clone());

    let repo_path = std::env::current_dir()?.join(".meld");
    let session = meld_engine::Session::open(repo_path.join("meld.store"))
        .map_err(|e| io::Error::other(format!("Failed to open Meld repository: {e}")))?;

    app.set_context(ScreenContext {
        session: std::sync::Arc::new(std::sync::Mutex::new(session)),
        tx: event_tx,
    });

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
                        AppEvent::BackgroundResult { id, ok, message, kind } => {
                            debug!(id, ok, ?kind, message = %message, "BackgroundResult: start");
                            app.pending.retain(|(pid, _)| *pid != id);
                            app.busy = !app.pending.is_empty();
                            match kind {
                                BackgroundKind::Connect if ok => {
                                    let name = app
                                        .connect_config
                                        .as_ref()
                                        .map(|config| match config {
                                            ConnectionConfig::PostgreSql(pg) => pg.name.clone(),
                                            ConnectionConfig::MongoDb(mongo) => mongo.name.clone(),
                                        });
                                    if let Some(config) = app.connect_config.take() {
                                        app.save_new_connection(&config);
                                    }
                                    app.pop_screen();
                                    app.connection_name = name;
                                    app.set_status(message.clone());
                                }
                                BackgroundKind::Connect => {
                                    // The attempt ended: never leave a stale
                                    // "Connecting to database..." status line.
                                    app.status = None;
                                    app.set_error(message.clone());
                                }
                                BackgroundKind::Disconnect => {
                                    if ok {
                                        app.connection_name = None;
                                    }
                                    app.set_status(message.clone());
                                }
                                BackgroundKind::Other => {
                                    if ok {
                                        app.set_status(message.clone());
                                    } else {
                                        app.set_error(message.clone());
                                    }
                                }
                            }
                            // Route the result to every screen in the stack.
                            for screen in &mut app.screen_stack {
                                screen.on_result(id, ok, message.clone());
                            }
                            debug!(id, ok, ?kind, "BackgroundResult: end");
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
