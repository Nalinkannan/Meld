//! Reusable UI components.

use ratatui::{
    Frame,
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph, Wrap},
};

use crate::theme::Styles;

/// A selectable list component.
pub struct SelectableList<'a> {
    pub items: Vec<&'a str>,
    pub selected: usize,
    pub title: Option<&'a str>,
    pub style: Style,
    pub selected_style: Style,
}

impl<'a> SelectableList<'a> {
    pub fn new(items: Vec<&'a str>) -> Self {
        Self {
            items,
            selected: 0,
            title: None,
            style: Styles::item_normal(),
            selected_style: Styles::selected(),
        }
    }

    pub fn with_title(mut self, title: &'a str) -> Self {
        self.title = Some(title);
        self
    }

    pub fn with_selected(mut self, selected: usize) -> Self {
        self.selected = selected;
        self
    }

    pub fn draw(&self, frame: &mut Frame, area: Rect) {
        let list_items: Vec<ListItem> = self
            .items
            .iter()
            .enumerate()
            .map(|(i, item)| {
                let style = if i == self.selected {
                    self.selected_style
                } else {
                    self.style
                };
                ListItem::new(Line::from(Span::styled(*item, style)))
            })
            .collect();

        let mut list = List::new(list_items).highlight_style(self.selected_style);

        if let Some(title) = self.title {
            list = list.block(
                Block::default()
                    .title(title)
                    .borders(Borders::ALL)
                    .border_style(crate::theme::Styles::border()),
            );
        }

        frame.render_widget(list, area);
    }
}

/// A text input field component.
#[derive(Clone)]
pub struct InputField {
    pub label: String,
    pub value: String,
    pub focused: bool,
    pub cursor_position: usize,
    /// When true, the value is drawn as bullets instead of plain text.
    pub masked: bool,
}

impl InputField {
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            value: String::new(),
            focused: false,
            cursor_position: 0,
            masked: false,
        }
    }

    pub fn with_value(mut self, value: impl Into<String>) -> Self {
        self.value = value.into();
        self.cursor_position = self.value.len();
        self
    }

    /// Mask the drawn value (e.g. for passwords).
    pub fn with_masked(mut self, masked: bool) -> Self {
        self.masked = masked;
        self
    }

    pub fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
    }

    pub fn handle_key(&mut self, key: crossterm::event::KeyEvent) -> bool {
        match key.code {
            crossterm::event::KeyCode::Char(c) => {
                self.value.insert(self.cursor_position, c);
                self.cursor_position += 1;
                true
            }
            crossterm::event::KeyCode::Backspace => {
                if self.cursor_position > 0 {
                    self.cursor_position -= 1;
                    self.value.remove(self.cursor_position);
                }
                true
            }
            crossterm::event::KeyCode::Left => {
                if self.cursor_position > 0 {
                    self.cursor_position -= 1;
                }
                true
            }
            crossterm::event::KeyCode::Right => {
                if self.cursor_position < self.value.len() {
                    self.cursor_position += 1;
                }
                true
            }
            crossterm::event::KeyCode::Home => {
                self.cursor_position = 0;
                true
            }
            crossterm::event::KeyCode::End => {
                self.cursor_position = self.value.len();
                true
            }
            _ => false,
        }
    }

    /// The value to draw: bullets when masked, plain text otherwise.
    fn display_value(&self) -> String {
        if self.masked {
            "•".repeat(self.value.chars().count())
        } else {
            self.value.clone()
        }
    }

    pub fn draw(&self, frame: &mut Frame, area: Rect) {
        let style = if self.focused {
            Styles::input_focus()
        } else {
            Styles::input_blur()
        };

        let cursor = if self.focused {
            if self.masked {
                format!("{}_", self.display_value())
            } else {
                format!(
                    "{}_",
                    &self.value[..self.cursor_position.min(self.value.len())]
                )
            }
        } else {
            self.display_value()
        };

        let text = format!("{}: {}", self.label, cursor);
        let mut para = Paragraph::new(text)
            .style(style)
            .wrap(Wrap { trim: true });
        // A bordered block needs at least 3 lines; single-line rows skip it.
        if area.height >= 3 {
            para = para.block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(if self.focused {
                        crate::theme::Styles::border_focus()
                    } else {
                        crate::theme::Styles::border()
                    }),
            );
        }
        frame.render_widget(para, area);
    }
}

/// A confirmation dialog component.
pub struct ConfirmDialog {
    pub title: String,
    pub message: String,
    pub yes_text: String,
    pub no_text: String,
    pub selected_yes: bool,
}

impl ConfirmDialog {
    pub fn new(title: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            message: message.into(),
            yes_text: "Yes".to_string(),
            no_text: "No".to_string(),
            selected_yes: true,
        }
    }

    pub fn handle_key(&mut self, key: crossterm::event::KeyEvent) -> Option<bool> {
        match key.code {
            crossterm::event::KeyCode::Left => {
                self.selected_yes = true;
                Some(false) // Not confirmed yet
            }
            crossterm::event::KeyCode::Right => {
                self.selected_yes = false;
                Some(false)
            }
            crossterm::event::KeyCode::Enter => Some(self.selected_yes),
            crossterm::event::KeyCode::Char('y') | crossterm::event::KeyCode::Char('Y') => {
                Some(true)
            }
            crossterm::event::KeyCode::Char('n') | crossterm::event::KeyCode::Char('N') => {
                Some(false)
            }
            crossterm::event::KeyCode::Esc => Some(false),
            _ => None,
        }
    }

    pub fn draw(&self, frame: &mut Frame, area: Rect) {
        let _border = crate::theme::DEFAULT_BORDER.chars();

        let text = vec![
            Line::from(""),
            Line::from(vec![Span::styled(&self.message, Styles::item_normal())]),
            Line::from(""),
            Line::from(vec![
                Span::styled("  ", Styles::base()),
                Span::styled(
                    if self.selected_yes { "► " } else { "  " },
                    Styles::key_hint(),
                ),
                Span::styled(
                    &self.yes_text,
                    if self.selected_yes {
                        Styles::selected()
                    } else {
                        Styles::item_normal()
                    },
                ),
                Span::styled("    ", Styles::base()),
                Span::styled(
                    if !self.selected_yes { "► " } else { "  " },
                    Styles::key_hint(),
                ),
                Span::styled(
                    &self.no_text,
                    if !self.selected_yes {
                        Styles::selected()
                    } else {
                        Styles::item_normal()
                    },
                ),
            ]),
        ];

        let para = Paragraph::new(text)
            .alignment(ratatui::layout::Alignment::Center)
            .block(
                Block::default()
                    .title(self.title.as_str())
                    .borders(Borders::ALL)
                    .border_style(Styles::border_focus())
                    .style(Styles::panel()),
            )
            .wrap(Wrap { trim: true });

        frame.render_widget(para, area);
    }
}
