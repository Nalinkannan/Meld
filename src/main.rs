use crossterm::{
    event::{self, Event, KeyCode},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    widgets::{Block, BorderType, Borders, Paragraph},
};
use std::io;

struct App {
    input: String,
    messages: Vec<String>,
    cursor_visible: bool,
    ticks: u16,
}

impl App {
    fn new() -> Self {
        Self {
            input: String::new(),
            messages: vec!["Welcome to Meld TUI!".to_string()],
            cursor_visible: false,
            ticks: 0,
        }
    }

    fn tick(&mut self) {
        self.ticks = self.ticks.wrapping_add(1);
        if self.ticks % 10 == 0 {
            self.cursor_visible = !self.cursor_visible;
        }
    }

    fn handle_key(&mut self, key: KeyCode) {
        match key {
            KeyCode::Char(c) => {
                self.input.push(c);
            }
            KeyCode::Backspace => {
                self.input.pop();
            }
            KeyCode::Enter => {
                if !self.input.is_empty() {
                    self.messages.push(self.input.clone());
                    self.input.clear();
                }
            }
            KeyCode::Esc => {
                // We handle Esc in the main loop to quit
            }
            _ => {}
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new();

    loop {
        terminal.draw(|f| {
            let size = f.size();

            // Main layout: Top area for messages, bottom area for input
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(1), Constraint::Length(3)])
                .split(size);

            // Main content area
            let main_text = app.messages.join("\n");
            let main_block = Paragraph::new(main_text).block(
                Block::default()
                    .title(" Meld ")
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .style(Style::default().fg(Color::Cyan)),
            );
            f.render_widget(main_block, chunks[0]);

            // Input area
            let mut input_text = app.input.clone();
            if app.cursor_visible {
                input_text.push('┃');
            }

            let input_block = Paragraph::new(input_text.as_str())
                .block(
                    Block::default()
                        .title(" Input ")
                        .borders(Borders::ALL)
                        .border_type(BorderType::Rounded)
                        .style(Style::default().fg(Color::Yellow)),
                )
                .style(Style::default().add_modifier(Modifier::BOLD));
            f.render_widget(input_block, chunks[1]);
        })?;

        app.tick();

        if event::poll(std::time::Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                if key.code == KeyCode::Esc {
                    break;
                }
                app.handle_key(key.code);
            }
        }
    }

    // Restore terminal
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;

    Ok(())
}
