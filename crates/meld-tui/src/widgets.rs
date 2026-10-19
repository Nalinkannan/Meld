//! Custom Ratatui widgets.

use ratatui::{
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph, Widget},
};

use crate::theme::Styles;

/// A diff view widget showing additions, removals, and modifications.
pub struct DiffView<'a> {
    pub lines: Vec<DiffLine<'a>>,
    pub selected: usize,
}

#[derive(Debug, Clone)]
pub struct DiffLine<'a> {
    pub kind: DiffKind,
    pub content: &'a str,
    pub line_number: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffKind {
    Context,
    Add,
    Remove,
    Modify,
}

impl<'a> Widget for DiffView<'a> {
    fn render(self, area: Rect, buf: &mut ratatui::buffer::Buffer) {
        let items: Vec<ListItem> = self
            .lines
            .iter()
            .enumerate()
            .map(|(i, line)| {
                let style = match line.kind {
                    DiffKind::Context => Styles::item_dim(),
                    DiffKind::Add => Styles::diff_add(),
                    DiffKind::Remove => Styles::diff_remove(),
                    DiffKind::Modify => Styles::diff_modify(),
                };

                let prefix = match line.kind {
                    DiffKind::Context => "  ",
                    DiffKind::Add => "+ ",
                    DiffKind::Remove => "- ",
                    DiffKind::Modify => "~ ",
                };

                let line_num = line
                    .line_number
                    .map(|n| format!("{:4} ", n))
                    .unwrap_or_else(|| "     ".to_string());

                let content = format!("{}{}{}", line_num, prefix, line.content);
                let list_item = ListItem::new(Line::from(Span::styled(content, style)));

                if i == self.selected {
                    list_item.style(Styles::selected())
                } else {
                    list_item
                }
            })
            .collect();

        let list = List::new(items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Styles::border())
                    .style(Styles::panel()),
            )
            .highlight_style(Styles::selected());
        Widget::render(&list, area, buf);
    }
}

/// A progress bar widget.
pub struct ProgressBar {
    pub progress: f64, // 0.0 to 1.0
    pub label: Option<String>,
    pub style: Style,
    pub filled_style: Style,
}

impl ProgressBar {
    pub fn new(progress: f64) -> Self {
        Self {
            progress: progress.clamp(0.0, 1.0),
            label: None,
            style: Styles::muted(),
            filled_style: Styles::success(),
        }
    }

    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }
}

impl Widget for &ProgressBar {
    fn render(self, area: Rect, buf: &mut ratatui::buffer::Buffer) {
        if area.width < 10 {
            return;
        }

        // Extract all fields upfront to avoid lifetime issues
        let style = self.style;
        let filled_style = self.filled_style;
        let progress = self.progress;
        let label = self.label.clone();

        let filled_width = ((area.width as f64 - 2.0) * progress) as u16;

        // Background
        for x in area.x..area.x + area.width {
            for y in area.y..area.y + area.height {
                buf[(x, y)].set_style(style);
            }
        }

        // Filled portion
        for x in area.x + 1..area.x + 1 + filled_width {
            for y in area.y + 1..area.y + area.height.saturating_sub(1) {
                buf[(x, y)].set_style(filled_style);
            }
        }

        // Label
        if let Some(label) = label {
            let label_x = area.x + (area.width.saturating_sub(label.len() as u16)) / 2;
            let label_y = area.y + area.height / 2;
            for (i, ch) in label.chars().enumerate() {
                if label_x + (i as u16) < area.x + area.width {
                    buf[(label_x + i as u16, label_y)]
                        .set_char(ch)
                        .set_style(Styles::base());
                }
            }
        }
    }
}

/// A table widget for displaying structured data.
pub struct DataTable<'a> {
    pub headers: Vec<&'a str>,
    pub rows: Vec<Vec<&'a str>>,
    pub selected_row: usize,
    pub column_widths: Vec<u16>,
}

impl<'a> DataTable<'a> {
    pub fn new(headers: Vec<&'a str>, rows: Vec<Vec<&'a str>>) -> Self {
        let column_widths = headers.iter().map(|h| h.len() as u16 + 2).collect();
        Self {
            headers,
            rows,
            selected_row: 0,
            column_widths,
        }
    }
}

impl<'a> Widget for DataTable<'a> {
    fn render(self, area: Rect, buf: &mut ratatui::buffer::Buffer) {
        if self.headers.is_empty() || area.height < 3 {
            return;
        }

        // Render headers
        let mut x = area.x;
        for (i, header) in self.headers.iter().enumerate() {
            let width = self
                .column_widths
                .get(i)
                .copied()
                .unwrap_or(10)
                .min(area.width);
            let header_para = Paragraph::new(*header)
                .style(Styles::header())
                .alignment(ratatui::layout::Alignment::Left);
            let header_rect = Rect {
                x,
                y: area.y,
                width: width.min(area.width.saturating_sub(x.saturating_sub(area.x))),
                height: 1,
            };
            header_para.render(header_rect, buf);
            x += width;
        }

        // Separator
        let sep_y = area.y + 1;
        for x in area.x..area.x + area.width {
            buf[(x, sep_y)].set_char('─').set_style(Styles::border());
        }

        // Render rows
        let row_start = area.y + 2;
        let max_rows = area.height.saturating_sub(2) as usize;

        for (row_idx, row) in self.rows.iter().enumerate().take(max_rows) {
            let y = row_start + row_idx as u16;
            if y >= area.y + area.height {
                break;
            }

            let style = if row_idx == self.selected_row {
                Styles::selected()
            } else {
                Styles::item_normal()
            };

            let mut x = area.x;
            for (col_idx, cell) in row.iter().enumerate() {
                let width = self.column_widths.get(col_idx).copied().unwrap_or(10);
                let cell_rect = Rect {
                    x,
                    y,
                    width: width.min(area.width.saturating_sub(x.saturating_sub(area.x))),
                    height: 1,
                };
                let cell_para = Paragraph::new(*cell).style(style);
                cell_para.render(cell_rect, buf);
                x += width;
            }
        }
    }
}

/// A status indicator widget.
pub struct StatusIndicator {
    pub status: Status,
    pub label: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Connected,
    Disconnected,
    Connecting,
    Error,
    Warning,
}

impl StatusIndicator {
    pub fn new(status: Status, label: impl Into<String>) -> Self {
        Self {
            status,
            label: label.into(),
        }
    }
}

impl Widget for StatusIndicator {
    fn render(self, area: Rect, buf: &mut ratatui::buffer::Buffer) {
        let (icon, style) = match self.status {
            Status::Connected => ("●", Styles::success()),
            Status::Disconnected => ("○", Styles::danger()),
            Status::Connecting => ("◐", Styles::warning()),
            Status::Error => ("✗", Styles::danger()),
            Status::Warning => ("⚠", Styles::warning()),
        };

        let text = format!("{} {}", icon, self.label);
        let para = Paragraph::new(text).style(style);
        para.render(area, buf);
    }
}

/// A keybinding hint bar widget.
pub struct KeyHintBar {
    pub hints: Vec<(String, String)>, // (key, description)
}

impl KeyHintBar {
    pub fn new(hints: Vec<(String, String)>) -> Self {
        Self { hints }
    }
}

impl Widget for KeyHintBar {
    fn render(self, area: Rect, buf: &mut ratatui::buffer::Buffer) {
        if area.height < 1 || self.hints.is_empty() {
            return;
        }

        let mut x = area.x;
        for (key, desc) in self.hints {
            let key_text = format!(" {} ", key);
            let desc_text = format!(" {}  ", desc);

            let key_width = key_text.len() as u16;
            let desc_width = desc_text.len() as u16;

            if x + key_width + desc_width > area.x + area.width {
                break;
            }

            // Key
            for (i, ch) in key_text.chars().enumerate() {
                buf[(x + i as u16, area.y)]
                    .set_char(ch)
                    .set_style(Styles::key_hint());
            }
            x += key_width;

            // Description
            for (i, ch) in desc_text.chars().enumerate() {
                buf[(x + i as u16, area.y)]
                    .set_char(ch)
                    .set_style(Styles::key_desc());
            }
            x += desc_width
        }
    }
}
