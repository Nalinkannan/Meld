//! Screen Trait and Common Screen Types
//!
//! Every screen reads real engine/store state.

use crate::components::InputField;
use crate::event::AppEvent;
use crate::input::TextBuffer;
use crate::theme::{Styles, keybinding_span};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use meld_adapter::ConnectionConfig;
use meld_core::ids::CommitHash;
use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::Modifier,
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};
use std::sync::Arc;
use std::sync::Mutex as StdMutex;

/// Context shared with every screen: the session (behind a mutex so screens
/// can read real engine state and request background work), plus the event
/// sender for posting results back to the app.
#[derive(Clone)]
pub struct ScreenContext {
    pub session: Arc<StdMutex<meld_engine::Session>>,
    pub tx: tokio::sync::mpsc::UnboundedSender<AppEvent>,
}

/// Action requested by a screen from a key press, executed by the app with
/// access to the session.
#[derive(Debug, Clone)]
pub enum ScreenAction {
    Push(ScreenType),
    Pop,
    Replace(ScreenType),
    Quit,
    Connect(String),
    ConnectNew(meld_adapter::ConnectionConfig),
    Disconnect,
    DeleteProfile(String),
    Snapshot(String),
    History,
    Diff(String, String),
    Branches,
    Merge(String),
    Restore(CommitHash),
    Verify(Option<CommitHash>),
    Drift,
}

pub type ScreenResult = Result<(), std::io::Error>;
pub type KeyResult = Result<Option<ScreenAction>, std::io::Error>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScreenType {
    Dashboard,
    Connections,
    NewConnection,
    History,
    Snapshot,
    Branches,
    Diff,
    Merge,
    Restore,
    Verify,
    Settings,
    Help,
    ConfirmQuit,
    Error(String),
}

impl ScreenType {
    pub fn create_screen(self) -> Box<dyn Screen> {
        match self {
            ScreenType::Dashboard => Box::new(DashboardScreen::new()),
            ScreenType::Connections => Box::new(ConnectionsScreen::new()),
            ScreenType::NewConnection => Box::new(NewConnectionScreen::new()),
            ScreenType::History => Box::new(HistoryScreen::new()),
            ScreenType::Snapshot => Box::new(SnapshotScreen::new()),
            ScreenType::Branches => Box::new(BranchesScreen::new()),
            ScreenType::Diff => Box::new(DiffScreen::new()),
            ScreenType::Merge => Box::new(MergeScreen::new()),
            ScreenType::Restore => Box::new(RestoreScreen::new()),
            ScreenType::Verify => Box::new(VerifyScreen::new()),
            ScreenType::Settings => Box::new(SettingsScreen::new()),
            ScreenType::Help => Box::new(HelpScreen::new()),
            ScreenType::ConfirmQuit => Box::new(ConfirmQuitScreen::new()),
            ScreenType::Error(msg) => Box::new(ErrorScreen::new(msg)),
        }
    }

    pub fn is_modal(&self) -> bool {
        matches!(
            self,
            ScreenType::ConfirmQuit | ScreenType::Error(_) | ScreenType::Help
        )
    }
}

pub trait Screen: Send {
    fn draw(&self, frame: &mut Frame, area: Rect);
    fn handle_key(&mut self, _key: crossterm::event::KeyEvent) -> KeyResult {
        Ok(None)
    }
    fn tick(&mut self) -> ScreenResult {
        Ok(())
    }
    fn on_resize(&mut self, _size: ratatui::layout::Size) {}
    fn title(&self) -> &str;
    fn key_hints(&self) -> Vec<ratatui::text::Span<'static>> {
        Vec::new()
    }
    fn take_next_screen(&mut self) -> Option<ScreenType> {
        None
    }
    fn is_modal(&self) -> bool {
        false
    }
    fn is_connected(&self) -> bool {
        false
    }
    fn branch_name(&self) -> Option<String> {
        None
    }
    fn head_short(&self) -> Option<String> {
        None
    }
    fn focus_next(&mut self) -> ScreenResult {
        Ok(())
    }
    fn set_context(&mut self, _ctx: ScreenContext) {}
    fn on_result(&mut self, _id: u64, _ok: bool, _message: String) {}
}

pub struct BaseScreen {
    pub title: String,
    pub selected_index: usize,
    pub scroll_offset: usize,
    next_screen: Option<ScreenType>,
    ctx: Option<ScreenContext>,
}

impl BaseScreen {
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            selected_index: 0,
            scroll_offset: 0,
            next_screen: None,
            ctx: None,
        }
    }
    pub fn set_context(&mut self, ctx: ScreenContext) {
        self.ctx = Some(ctx);
    }
    pub fn ctx(&self) -> Option<&ScreenContext> {
        self.ctx.as_ref()
    }
    pub fn set_next_screen(&mut self, screen: ScreenType) {
        self.next_screen = Some(screen);
    }
    pub fn take_next_screen(&mut self) -> Option<ScreenType> {
        self.next_screen.take()
    }
    pub fn tick(&mut self) -> ScreenResult {
        Ok(())
    }
    pub fn move_selection_up(&mut self, max: usize) {
        if max == 0 {
            return;
        }
        self.selected_index = self.selected_index.saturating_sub(1).min(max - 1);
    }
    pub fn move_selection_down(&mut self, max: usize) {
        if max == 0 {
            return;
        }
        self.selected_index = (self.selected_index + 1).min(max - 1);
    }
    pub fn handle_list_keys(&mut self, key: crossterm::event::KeyEvent, max_items: usize) -> bool {
        match key.code {
            crossterm::event::KeyCode::Up | crossterm::event::KeyCode::Char('k') => {
                self.move_selection_up(max_items);
                true
            }
            crossterm::event::KeyCode::Down | crossterm::event::KeyCode::Char('j') => {
                self.move_selection_down(max_items);
                true
            }
            crossterm::event::KeyCode::PageUp => {
                self.selected_index = self
                    .selected_index
                    .saturating_sub(10)
                    .min(max_items.saturating_sub(1));
                true
            }
            crossterm::event::KeyCode::PageDown => {
                self.selected_index = (self.selected_index + 10).min(max_items.saturating_sub(1));
                true
            }
            crossterm::event::KeyCode::Home => {
                self.selected_index = 0;
                true
            }
            crossterm::event::KeyCode::End => {
                self.selected_index = max_items.saturating_sub(1);
                true
            }
            _ => false,
        }
    }
}

fn short_hash(hash: &CommitHash) -> String {
    hash.short(12).0
}

fn mask_uri(uri: Option<&str>) -> String {
    match uri {
        Some(u) => {
            if let Some((scheme, rest)) = u.split_once("://")
                && let Some((_creds, hostpart)) = rest.rsplit_once('@')
            {
                return format!("{}://user:****@{}", scheme, hostpart);
            }
            "****".into()
        }
        None => "none".into(),
    }
}

// ===========================================================================
// Dashboard
// ===========================================================================

pub struct DashboardScreen {
    base: BaseScreen,
    connection_status: String,
    connection_masked: String,
    recent_commits: Vec<(CommitHash, String, String)>,
    status_msg: String,
}

impl DashboardScreen {
    pub fn new() -> Self {
        Self {
            base: BaseScreen::new("Dashboard"),
            connection_status: "Disconnected".into(),
            connection_masked: String::new(),
            recent_commits: Vec::new(),
            status_msg: String::new(),
        }
    }
}

impl Default for DashboardScreen {
    fn default() -> Self {
        Self::new()
    }
}

impl Screen for DashboardScreen {
    fn draw(&self, frame: &mut Frame, area: Rect) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Min(0),
                Constraint::Length(3),
            ])
            .split(area);

        let title = Paragraph::new("Dashboard")
            .style(Styles::header())
            .alignment(Alignment::Center);
        frame.render_widget(title, chunks[0]);

        let conn_lines = vec![
            Line::from(vec![
                Span::styled("Status: ", Styles::muted()),
                Span::styled(
                    &self.connection_status,
                    if self.connection_status == "Connected" {
                        Styles::success()
                    } else {
                        Styles::danger()
                    },
                ),
            ]),
            Line::from(vec![
                Span::styled("Connection: ", Styles::muted()),
                Span::styled(&self.connection_masked, Styles::item_normal()),
            ]),
        ];
        let conn = Paragraph::new(conn_lines).block(
            Block::default()
                .title(" Connection ")
                .borders(Borders::ALL)
                .border_style(Styles::border()),
        );
        frame.render_widget(conn, chunks[1]);

        let items: Vec<ratatui::widgets::ListItem> = self
            .recent_commits
            .iter()
            .map(|(hash, msg, author)| {
                Line::from(vec![
                    Span::styled(short_hash(hash), Styles::key_hint()),
                    Span::styled("  ", Styles::muted()),
                    Span::styled(msg, Styles::item_normal()),
                    Span::styled(format!("  ({author})"), Styles::item_dim()),
                ])
                .into()
            })
            .collect();
        let list = ratatui::widgets::List::new(if items.is_empty() {
            vec![
                Line::from(Span::styled(
                    "No commits yet — create a snapshot.",
                    Styles::item_dim(),
                ))
                .into(),
            ]
        } else {
            items
        })
        .block(
            Block::default()
                .title(" Commits ")
                .borders(Borders::ALL)
                .border_style(Styles::border()),
        );
        frame.render_widget(list, chunks[2]);
    }

    fn handle_key(&mut self, key: KeyEvent) -> KeyResult {
        match key.code {
            KeyCode::Char('s') => return Ok(Some(ScreenAction::Snapshot(String::new()))),
            KeyCode::Char('c') => return Ok(Some(ScreenAction::Push(ScreenType::Connections))),
            KeyCode::Char('v') => return Ok(Some(ScreenAction::Verify(None))),
            _ => {}
        }
        let _ = self.base.handle_list_keys(key, self.recent_commits.len());
        Ok(None)
    }

    fn tick(&mut self) -> ScreenResult {
        self.base.tick()
    }

    fn title(&self) -> &str {
        &self.base.title
    }

    fn key_hints(&self) -> Vec<ratatui::text::Span<'static>> {
        let mut h = Vec::new();
        h.extend(keybinding_span("s", "Snapshot"));
        h.extend(keybinding_span("c", "Connections"));
        h.extend(keybinding_span("v", "Verify"));
        h
    }

    fn set_context(&mut self, ctx: ScreenContext) {
        self.base.set_context(ctx);
    }

    fn on_result(&mut self, _id: u64, ok: bool, message: String) {
        if ok {
            self.status_msg = format!("verify ok: {message}");
        } else {
            self.status_msg = format!("verify failed: {message}");
        }
    }

    fn is_connected(&self) -> bool {
        self.connection_status == "Connected"
    }
}

// ===========================================================================
// Connections
// ===========================================================================

pub struct ConnectionsScreen {
    base: BaseScreen,
    profiles: Vec<ConnectionProfileView>,
    connected_profile: Option<String>,
    status_msg: String,
}

#[derive(Clone)]
struct ConnectionProfileView {
    name: String,
    engine: String,
    masked_uri: String,
}

impl ConnectionsScreen {
    pub fn new() -> Self {
        Self {
            base: BaseScreen::new("Connections"),
            profiles: Vec::new(),
            connected_profile: None,
            status_msg: String::new(),
        }
    }
}

impl Default for ConnectionsScreen {
    fn default() -> Self {
        Self::new()
    }
}

impl Screen for ConnectionsScreen {
    fn draw(&self, frame: &mut Frame, area: Rect) {
        let title = Paragraph::new("Connections")
            .style(Styles::header())
            .alignment(Alignment::Center);
        frame.render_widget(title, area);

        let items: Vec<ratatui::widgets::ListItem> = self
            .profiles
            .iter()
            .map(|p| {
                let connected = self.connected_profile.as_deref() == Some(&p.name);
                let status = if connected { "●" } else { "○" };
                let style = if connected {
                    Styles::success()
                } else {
                    Styles::item_normal()
                };
                Line::from(vec![
                    Span::styled(status, style),
                    Span::styled("  ", Styles::muted()),
                    Span::styled(&p.name, style.add_modifier(Modifier::BOLD)),
                    Span::styled(format!("  ({})", p.engine), Styles::item_dim()),
                    Span::styled(format!("  {}", p.masked_uri), Styles::item_dim()),
                ])
                .into()
            })
            .collect();
        let list = ratatui::widgets::List::new(if items.is_empty() {
            vec![
                Line::from(Span::styled(
                    "No saved profiles. Press 'n' to add one.",
                    Styles::item_dim(),
                ))
                .into(),
            ]
        } else {
            items
        })
        .block(
            Block::default()
                .title(" Profiles ")
                .borders(Borders::ALL)
                .border_style(Styles::border()),
        );
        frame.render_widget(list, area);
    }

    fn handle_key(&mut self, key: KeyEvent) -> KeyResult {
        match key.code {
            KeyCode::Char('n') => return Ok(Some(ScreenAction::Push(ScreenType::NewConnection))),
            KeyCode::Enter => {
                if let Some(p) = self.selected_profile() {
                    if self.connected_profile.as_deref() == Some(&p) {
                        return Ok(Some(ScreenAction::Disconnect));
                    }
                    return Ok(Some(ScreenAction::Connect(p)));
                }
            }
            KeyCode::Char('d') => {
                if let Some(p) = self.selected_profile() {
                    if self.connected_profile.as_deref() == Some(&p) {
                        return Ok(Some(ScreenAction::Disconnect));
                    }
                    return Ok(Some(ScreenAction::Connect(p)));
                }
            }
            KeyCode::Char('D') => {
                if let Some(p) = self.selected_profile() {
                    return Ok(Some(ScreenAction::DeleteProfile(p)));
                }
            }
            _ => {}
        }
        let _ = self.base.handle_list_keys(key, self.profiles.len());
        Ok(None)
    }

    fn tick(&mut self) -> ScreenResult {
        self.base.tick()
    }

    fn title(&self) -> &str {
        &self.base.title
    }

    fn key_hints(&self) -> Vec<ratatui::text::Span<'static>> {
        let mut h = Vec::new();
        h.extend(keybinding_span("n", "New"));
        h.extend(keybinding_span("c", "Connect"));
        h.extend(keybinding_span("d", "Disconnect"));
        h
    }

    fn set_context(&mut self, _ctx: ScreenContext) {
        self.base.set_context(_ctx);
        let Some(ctx) = &self.base.ctx else { return };
        if let Ok(session) = ctx.session.lock() {
            let profiles = meld_store::connection_store::list_profiles(session.store.db())
                .unwrap_or_default()
                .into_iter()
                .map(|p| ConnectionProfileView {
                    name: p.name.clone(),
                    engine: format!("{:?}", p.engine),
                    masked_uri: mask_uri(None),
                })
                .collect();
            self.profiles = profiles;
        }
    }

    fn on_result(&mut self, _id: u64, ok: bool, message: String) {
        if ok {
            self.status_msg = format!("connected: {message}");
            if let Some(p) = self.selected_profile() {
                self.connected_profile = Some(p);
            }
        } else {
            self.status_msg = format!("connection error: {message}");
        }
    }

    fn is_connected(&self) -> bool {
        self.connected_profile.is_some()
    }

    fn branch_name(&self) -> Option<String> {
        None
    }

    fn head_short(&self) -> Option<String> {
        None
    }
}

impl ConnectionsScreen {
    fn selected_profile(&self) -> Option<String> {
        if self.profiles.is_empty() {
            return None;
        }
        let idx = self.base.selected_index.min(self.profiles.len() - 1);
        Some(self.profiles[idx].name.clone())
    }
}

// ===========================================================================
// New connection
// ===========================================================================

pub struct NewConnectionScreen {
    base: BaseScreen,
    name: InputField,
    engine: InputField,
    host: InputField,
    port: InputField,
    database: InputField,
    user: InputField,
    password: InputField,
    focus: usize,
    status_msg: String,
}

impl NewConnectionScreen {
    pub fn new() -> Self {
        Self {
            base: BaseScreen::new("New Connection"),
            name: InputField::new("name"),
            engine: InputField::new("engine (postgres|mongodb)"),
            host: InputField::new("host"),
            port: InputField::new("port"),
            database: InputField::new("database"),
            user: InputField::new("user"),
            password: InputField::new("password"),
            focus: 0,
            status_msg: String::new(),
        }
    }
}

impl Default for NewConnectionScreen {
    fn default() -> Self {
        Self::new()
    }
}

impl Screen for NewConnectionScreen {
    fn draw(&self, frame: &mut Frame, area: Rect) {
        let title = Paragraph::new("New Connection")
            .style(Styles::header())
            .alignment(Alignment::Center);
        frame.render_widget(title, area);

        let fields = [
            (&self.name, self.focus == 0),
            (&self.engine, self.focus == 1),
            (&self.host, self.focus == 2),
            (&self.port, self.focus == 3),
            (&self.database, self.focus == 4),
            (&self.user, self.focus == 5),
            (&self.password, self.focus == 6),
        ];
        let chunks: Vec<Rect> = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(3); 7])
            .split(area)
            .iter()
            .cloned()
            .collect();
        for i in 0..fields.len() {
            let (field, focused) = fields[i];
            let mut f = field.clone();
            f.set_focused(focused);
            f.draw(frame, chunks[i]);
        }
        if !self.status_msg.is_empty() {
            let msg = Paragraph::new(self.status_msg.clone()).style(Styles::danger());
            let rect = Rect {
                x: 0,
                y: 21,
                width: area.width,
                height: 1,
            };
            frame.render_widget(msg, rect);
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> KeyResult {
        match key.code {
            KeyCode::Tab => {
                self.focus = (self.focus + 1) % 7;
                return Ok(None);
            }
            KeyCode::Enter => {
                return self.submit();
            }
            KeyCode::Esc => {
                self.base.set_next_screen(ScreenType::Connections);
                return Ok(None);
            }
            _ => {}
        }
        // Delegate character/navigation editing to the focused field.
        let _handled = self.field_mut().handle_key(key);
        Ok(None)
    }

    fn tick(&mut self) -> ScreenResult {
        self.base.tick()
    }

    fn title(&self) -> &str {
        &self.base.title
    }

    fn key_hints(&self) -> Vec<ratatui::text::Span<'static>> {
        let mut h = Vec::new();
        h.extend(keybinding_span("Tab", "Next field"));
        h.extend(keybinding_span("Enter", "Connect"));
        h.extend(keybinding_span("Esc", "Back"));
        h
    }

    fn set_context(&mut self, ctx: ScreenContext) {
        self.base.set_context(ctx);
    }
}

impl NewConnectionScreen {
    fn field_mut(&mut self) -> &mut InputField {
        match self.focus {
            0 => &mut self.name,
            1 => &mut self.engine,
            2 => &mut self.host,
            3 => &mut self.port,
            4 => &mut self.database,
            5 => &mut self.user,
            _ => &mut self.password,
        }
    }

    fn submit(&mut self) -> KeyResult {
        let engine = self.engine.value.trim().to_lowercase();
        let port: u16 = self.port.value.trim().parse().unwrap_or(5432);
        let config = if engine == "mongodb" {
            ConnectionConfig::MongoDb(meld_adapter::MongoConfig {
                name: self.name.value.clone(),
                uri: None,
                host: Some(self.host.value.clone()),
                port: Some(port),
                database: self.database.value.clone(),
                username: Some(self.user.value.clone()),
                password: if self.password.value.is_empty() {
                    None
                } else {
                    Some(self.password.value.clone())
                },
                auth_source: None,
            })
        } else {
            ConnectionConfig::PostgreSql(meld_adapter::PostgresConfig {
                name: self.name.value.clone(),
                host: self.host.value.clone(),
                port,
                user: self.user.value.clone(),
                password: if self.password.value.is_empty() {
                    None
                } else {
                    Some(self.password.value.clone())
                },
                database: self.database.value.clone(),
                uri: None,
            })
        };
        Ok(Some(ScreenAction::ConnectNew(config)))
    }
}

// ===========================================================================
// History
// ===========================================================================

pub struct HistoryScreen {
    base: BaseScreen,
    commits: Vec<(CommitHash, String, String, String)>,
}

impl HistoryScreen {
    pub fn new() -> Self {
        Self {
            base: BaseScreen::new("History"),
            commits: Vec::new(),
        }
    }
}

impl Default for HistoryScreen {
    fn default() -> Self {
        Self::new()
    }
}

impl Screen for HistoryScreen {
    fn draw(&self, frame: &mut Frame, area: Rect) {
        let title = Paragraph::new("History")
            .style(Styles::header())
            .alignment(Alignment::Center);
        frame.render_widget(title, area);

        let items: Vec<ratatui::widgets::ListItem> = self
            .commits
            .iter()
            .map(|(hash, msg, author, time)| {
                Line::from(vec![
                    Span::styled(short_hash(hash), Styles::key_hint()),
                    Span::styled("  ", Styles::muted()),
                    Span::styled(msg, Styles::item_normal()),
                    Span::styled(format!("  {author}  {time}"), Styles::item_dim()),
                ])
                .into()
            })
            .collect();
        let list = ratatui::widgets::List::new(if items.is_empty() {
            vec![Line::from(Span::styled("No commits in history.", Styles::item_dim())).into()]
        } else {
            items
        })
        .block(
            Block::default()
                .title(" Commits ")
                .borders(Borders::ALL)
                .border_style(Styles::border()),
        );
        frame.render_widget(list, area);
    }

    fn handle_key(&mut self, key: KeyEvent) -> KeyResult {
        if let KeyCode::Char('h') = key.code {
            self.base.set_next_screen(ScreenType::History);
        }
        let _ = self.base.handle_list_keys(key, self.commits.len());
        Ok(None)
    }

    fn tick(&mut self) -> ScreenResult {
        self.base.tick()
    }

    fn title(&self) -> &str {
        &self.base.title
    }

    fn set_context(&mut self, _ctx: ScreenContext) {
        self.base.set_context(_ctx);
        let Some(ctx) = &self.base.ctx else { return };
        if let Ok(session) = ctx.session.lock()
            && let Ok(hashes) = session.store.list_all_commit_hashes()
        {
            let mut commits = Vec::new();
            for h in hashes {
                if let Ok(c) = session.store.get_commit(&h) {
                    commits.push((
                        h,
                        c.message,
                        c.author.clone(),
                        c.timestamp.format("%Y-%m-%d %H:%M").to_string(),
                    ));
                }
            }
            commits.sort_by(|a, b| b.3.cmp(&a.3));
            self.commits = commits;
        }
    }
}

// ===========================================================================
// Snapshot
// ===========================================================================

pub struct SnapshotScreen {
    base: BaseScreen,
    message: TextBuffer,
    submitting: bool,
    result: Option<(bool, String)>,
}

impl SnapshotScreen {
    pub fn new() -> Self {
        Self {
            base: BaseScreen::new("Create Snapshot"),
            message: TextBuffer::single_line(),
            submitting: false,
            result: None,
        }
    }
}

impl Default for SnapshotScreen {
    fn default() -> Self {
        Self::new()
    }
}

impl Screen for SnapshotScreen {
    fn draw(&self, frame: &mut Frame, area: Rect) {
        let title = Paragraph::new("Create Snapshot")
            .style(Styles::header())
            .alignment(Alignment::Center);
        frame.render_widget(title, area);

        // Render the message buffer as a single line.
        let buf = self.message.current_line_display();
        let msg_para = Paragraph::new(buf).style(Styles::item_normal()).block(
            Block::default()
                .title(" Message ")
                .borders(Borders::ALL)
                .border_style(Styles::border()),
        );
        frame.render_widget(
            msg_para,
            Rect {
                x: 2,
                y: 3,
                width: area.width.saturating_sub(4),
                height: 3,
            },
        );

        let hint = if self.submitting {
            "Snapshot in progress..."
        } else if let Some((_, m)) = &self.result {
            m.as_str()
        } else {
            "Enter a message and press Enter to snapshot"
        };
        let hint_para = Paragraph::new(hint)
            .style(if self.submitting {
                Styles::warning()
            } else {
                Styles::muted()
            })
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Styles::border()),
            );
        frame.render_widget(
            hint_para,
            Rect {
                x: 2,
                y: 7,
                width: area.width.saturating_sub(4),
                height: 3,
            },
        );
    }

    fn handle_key(&mut self, key: KeyEvent) -> KeyResult {
        if self.submitting {
            return Ok(None);
        }
        match key.code {
            KeyCode::Enter => {
                let msg = self.message.text().trim().to_string();
                if msg.is_empty() {
                    self.result = Some((false, "message required".into()));
                    return Ok(None);
                }
                self.submitting = true;
                self.result = None;
                return Ok(Some(ScreenAction::Snapshot(msg)));
            }
            KeyCode::Esc => {
                self.base.set_next_screen(ScreenType::Dashboard);
                return Ok(None);
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.message.insert(&c.to_string());
            }
            KeyCode::Backspace => {
                self.message.delete_back();
            }
            KeyCode::Delete => {
                self.message.delete_forward();
            }
            KeyCode::Left => self.message.move_left(),
            KeyCode::Right => self.message.move_right(),
            KeyCode::Home => self.message.move_home(),
            KeyCode::End => self.message.move_end(),
            _ => {}
        }
        Ok(None)
    }

    fn tick(&mut self) -> ScreenResult {
        self.base.tick()
    }

    fn title(&self) -> &str {
        &self.base.title
    }

    fn key_hints(&self) -> Vec<ratatui::text::Span<'static>> {
        let mut h = Vec::new();
        h.extend(keybinding_span("Enter", "Snapshot"));
        h.extend(keybinding_span("Esc", "Back"));
        h
    }

    fn set_context(&mut self, ctx: ScreenContext) {
        self.base.set_context(ctx);
    }

    fn on_result(&mut self, _id: u64, ok: bool, message: String) {
        self.submitting = false;
        self.result = Some((ok, message));
    }
}

// ===========================================================================
// Branches
// ===========================================================================

pub struct BranchesScreen {
    base: BaseScreen,
    branches: Vec<String>,
    current: String,
}

impl BranchesScreen {
    pub fn new() -> Self {
        Self {
            base: BaseScreen::new("Branches"),
            branches: Vec::new(),
            current: "main".into(),
        }
    }
}

impl Default for BranchesScreen {
    fn default() -> Self {
        Self::new()
    }
}

impl Screen for BranchesScreen {
    fn draw(&self, frame: &mut Frame, area: Rect) {
        let title = Paragraph::new("Branches")
            .style(Styles::header())
            .alignment(Alignment::Center);
        frame.render_widget(title, area);

        let items: Vec<ratatui::widgets::ListItem> = self
            .branches
            .iter()
            .map(|b| {
                let cur = if self.current == *b { "●" } else { "○" };
                Line::from(vec![
                    Span::styled(cur, Styles::success()),
                    Span::styled("  ", Styles::muted()),
                    Span::styled(b, Styles::item_normal()),
                ])
                .into()
            })
            .collect();
        let list = ratatui::widgets::List::new(if items.is_empty() {
            vec![Line::from(Span::styled("No branches.", Styles::item_dim())).into()]
        } else {
            items
        })
        .block(
            Block::default()
                .title(" Branches ")
                .borders(Borders::ALL)
                .border_style(Styles::border()),
        );
        frame.render_widget(list, area);
    }

    fn handle_key(&mut self, key: KeyEvent) -> KeyResult {
        if let KeyCode::Char('c') = key.code {
            return Ok(Some(ScreenAction::Push(ScreenType::NewConnection)));
        }
        let _ = self.base.handle_list_keys(key, self.branches.len());
        Ok(None)
    }

    fn tick(&mut self) -> ScreenResult {
        self.base.tick()
    }

    fn title(&self) -> &str {
        &self.base.title
    }

    fn set_context(&mut self, _ctx: ScreenContext) {
        self.base.set_context(_ctx);
        let Some(ctx) = &self.base.ctx else { return };
        if let Ok(session) = ctx.session.lock()
            && let Ok(branches) = session.store.list_branches()
        {
            self.branches = branches;
            self.current = session.current_branch.clone();
        }
    }

    fn is_connected(&self) -> bool {
        self.base.ctx.is_some()
    }

    fn branch_name(&self) -> Option<String> {
        Some(self.current.clone())
    }
}

// ===========================================================================
// Diff / Merge / Restore / Verify (placeholders wired to actions)
// ===========================================================================

pub struct DiffScreen {
    base: BaseScreen,
    result: Option<(bool, String)>,
}

impl DiffScreen {
    pub fn new() -> Self {
        Self {
            base: BaseScreen::new("Diff"),
            result: None,
        }
    }
}

impl Default for DiffScreen {
    fn default() -> Self {
        Self::new()
    }
}

impl Screen for DiffScreen {
    fn draw(&self, frame: &mut Frame, area: Rect) {
        let title = Paragraph::new("Diff")
            .style(Styles::header())
            .alignment(Alignment::Center);
        frame.render_widget(title, area);
        let msg = self.result.as_ref().map(|(_, m)| m.as_str()).unwrap_or("");
        let para = Paragraph::new(msg).style(Styles::muted());
        frame.render_widget(para, area);
    }
    fn title(&self) -> &str {
        &self.base.title
    }
    fn on_result(&mut self, _id: u64, ok: bool, message: String) {
        self.result = Some((ok, message));
    }
}

pub struct MergeScreen {
    base: BaseScreen,
    result: Option<(bool, String)>,
}

impl MergeScreen {
    pub fn new() -> Self {
        Self {
            base: BaseScreen::new("Merge"),
            result: None,
        }
    }
}

impl Default for MergeScreen {
    fn default() -> Self {
        Self::new()
    }
}

impl Screen for MergeScreen {
    fn draw(&self, frame: &mut Frame, area: Rect) {
        let title = Paragraph::new("Merge")
            .style(Styles::header())
            .alignment(Alignment::Center);
        frame.render_widget(title, area);
        let msg = self.result.as_ref().map(|(_, m)| m.as_str()).unwrap_or("");
        let para = Paragraph::new(msg).style(Styles::muted());
        frame.render_widget(para, area);
    }
    fn title(&self) -> &str {
        &self.base.title
    }
    fn on_result(&mut self, _id: u64, ok: bool, message: String) {
        self.result = Some((ok, message));
    }
}

pub struct RestoreScreen {
    base: BaseScreen,
    result: Option<(bool, String)>,
}

impl RestoreScreen {
    pub fn new() -> Self {
        Self {
            base: BaseScreen::new("Restore"),
            result: None,
        }
    }
}

impl Default for RestoreScreen {
    fn default() -> Self {
        Self::new()
    }
}

impl Screen for RestoreScreen {
    fn draw(&self, frame: &mut Frame, area: Rect) {
        let title = Paragraph::new("Restore")
            .style(Styles::header())
            .alignment(Alignment::Center);
        frame.render_widget(title, area);
        let msg = self.result.as_ref().map(|(_, m)| m.as_str()).unwrap_or("");
        let para = Paragraph::new(msg).style(Styles::muted());
        frame.render_widget(para, area);
    }
    fn title(&self) -> &str {
        &self.base.title
    }
    fn on_result(&mut self, _id: u64, ok: bool, message: String) {
        self.result = Some((ok, message));
    }
}

pub struct VerifyScreen {
    base: BaseScreen,
    result: Option<(bool, String)>,
}

impl VerifyScreen {
    pub fn new() -> Self {
        Self {
            base: BaseScreen::new("Verify"),
            result: None,
        }
    }
}

impl Default for VerifyScreen {
    fn default() -> Self {
        Self::new()
    }
}

impl Screen for VerifyScreen {
    fn draw(&self, frame: &mut Frame, area: Rect) {
        let title = Paragraph::new("Verify")
            .style(Styles::header())
            .alignment(Alignment::Center);
        frame.render_widget(title, area);
        let msg = self.result.as_ref().map(|(_, m)| m.as_str()).unwrap_or("");
        let para = Paragraph::new(msg).style(Styles::muted());
        frame.render_widget(para, area);
    }
    fn title(&self) -> &str {
        &self.base.title
    }
    fn on_result(&mut self, _id: u64, ok: bool, message: String) {
        self.result = Some((ok, message));
    }
}

// ===========================================================================
// Settings / Help / Confirm quit / Error
// ===========================================================================

pub struct SettingsScreen;
impl SettingsScreen {
    pub fn new() -> Self {
        Self
    }
}
impl Default for SettingsScreen {
    fn default() -> Self {
        Self::new()
    }
}
impl Screen for SettingsScreen {
    fn draw(&self, frame: &mut Frame, area: Rect) {
        let t = Paragraph::new("Settings")
            .style(Styles::header())
            .alignment(Alignment::Center);
        frame.render_widget(t, area);
    }
    fn title(&self) -> &str {
        "Settings"
    }
}

pub struct HelpScreen;
impl HelpScreen {
    pub fn new() -> Self {
        Self
    }
}
impl Default for HelpScreen {
    fn default() -> Self {
        Self::new()
    }
}
impl Screen for HelpScreen {
    fn draw(&self, frame: &mut Frame, area: Rect) {
        let t = Paragraph::new("Help — press ?")
            .style(Styles::header())
            .alignment(Alignment::Center);
        frame.render_widget(t, area);
    }
    fn title(&self) -> &str {
        "Help"
    }
    fn is_modal(&self) -> bool {
        true
    }
}

pub struct ConfirmQuitScreen;
impl ConfirmQuitScreen {
    pub fn new() -> Self {
        Self
    }
}
impl Default for ConfirmQuitScreen {
    fn default() -> Self {
        Self::new()
    }
}
impl Screen for ConfirmQuitScreen {
    fn draw(&self, frame: &mut Frame, area: Rect) {
        let t = Paragraph::new("Confirm quit")
            .style(Styles::header())
            .alignment(Alignment::Center);
        frame.render_widget(t, area);
    }
    fn title(&self) -> &str {
        "Confirm Quit"
    }
    fn is_modal(&self) -> bool {
        true
    }
}

#[derive(Default)]
pub struct ErrorScreen {
    message: String,
}
impl ErrorScreen {
    pub fn new(message: String) -> Self {
        Self { message }
    }
}
impl Screen for ErrorScreen {
    fn draw(&self, frame: &mut Frame, area: Rect) {
        let t = Paragraph::new(self.message.clone())
            .style(Styles::danger())
            .alignment(Alignment::Center);
        frame.render_widget(t, area);
    }
    fn title(&self) -> &str {
        "Error"
    }
    fn is_modal(&self) -> bool {
        true
    }
}
