//! Meld TUI Theme System
//!
//! Centralized color palette, styles, and component styling. All visual
//! design tokens live here; arbitrary color literals are never scattered
//! across screens. Inspired by Dracula theme with adaptations for
//! information density and accessibility.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;

/// Design system color palette (Dracula-inspired).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette;

impl Palette {
    // Base colors
    pub const BG: Color = Color::Rgb(24, 24, 37);
    pub const PANEL: Color = Color::Rgb(33, 33, 48);
    pub const PANEL_ALT: Color = Color::Rgb(42, 42, 60);
    pub const BORDER: Color = Color::Rgb(74, 78, 105);
    pub const BORDER_FOCUS: Color = Color::Rgb(189, 147, 249);

    // Accent colors
    pub const ACCENT: Color = Color::Rgb(189, 147, 249);
    pub const ACCENT_CYAN: Color = Color::Rgb(94, 220, 230);
    pub const ACCENT_SECONDARY: Color = Color::Rgb(139, 233, 253);
    pub const ACCENT_DIM: Color = Color::Rgb(98, 114, 164);

    // Semantic colors
    pub const SUCCESS: Color = Color::Rgb(80, 250, 123);
    pub const WARNING: Color = Color::Rgb(255, 184, 108);
    pub const DANGER: Color = Color::Rgb(255, 85, 85);
    pub const INFO: Color = Color::Rgb(139, 233, 253);

    // Text colors
    pub const TEXT: Color = Color::Rgb(248, 248, 242);
    pub const TEXT_DIM: Color = Color::Rgb(145, 147, 170);
    pub const TEXT_MUTED: Color = Color::Rgb(98, 114, 164);
    pub const TEXT_INVERTED: Color = Color::Rgb(24, 24, 37);

    // Diff colors
    pub const ADD: Color = Color::Rgb(80, 250, 123);
    pub const ADD_BG: Color = Color::Rgb(20, 40, 20);
    pub const REMOVE: Color = Color::Rgb(255, 85, 85);
    pub const REMOVE_BG: Color = Color::Rgb(40, 20, 20);
    pub const MODIFY: Color = Color::Rgb(255, 184, 108);
    pub const MODIFY_BG: Color = Color::Rgb(40, 30, 10);

    // Selection
    pub const SELECTION_BG: Color = Color::Rgb(68, 71, 90);
    pub const SELECTION_FG: Color = Color::Rgb(248, 248, 242);

    // Status bar
    pub const STATUS_BG: Color = Color::Rgb(18, 18, 28);
    pub const STATUS_FG: Color = Color::Rgb(145, 147, 170);
}

/// Semantic style presets.
pub struct Styles;

impl Styles {
    /// Base text style.
    pub fn base() -> Style {
        Style::default().fg(Palette::TEXT).bg(Palette::BG)
    }

    /// Panel background.
    pub fn panel() -> Style {
        Style::default().bg(Palette::PANEL)
    }

    /// Alternate panel background.
    pub fn panel_alt() -> Style {
        Style::default().bg(Palette::PANEL_ALT)
    }

    /// Header/title style.
    pub fn header() -> Style {
        Style::default()
            .fg(Palette::ACCENT)
            .bg(Palette::BG)
            .add_modifier(Modifier::BOLD)
    }

    /// Subtle header.
    pub fn header_dim() -> Style {
        Style::default().fg(Palette::ACCENT_DIM).bg(Palette::BG)
    }

    /// Border style (unfocused).
    pub fn border() -> Style {
        Style::default().fg(Palette::BORDER).bg(Palette::PANEL)
    }

    /// Border style (focused).
    pub fn border_focus() -> Style {
        Style::default()
            .fg(Palette::BORDER_FOCUS)
            .bg(Palette::PANEL)
            .add_modifier(Modifier::BOLD)
    }

    /// Selected/highlighted item.
    pub fn selected() -> Style {
        Style::default()
            .fg(Palette::SELECTION_FG)
            .bg(Palette::SELECTION_BG)
            .add_modifier(Modifier::BOLD)
    }

    /// Normal list item.
    pub fn item_normal() -> Style {
        Style::default().fg(Palette::TEXT).bg(Palette::PANEL)
    }

    /// Dim list item.
    pub fn item_dim() -> Style {
        Style::default().fg(Palette::TEXT_DIM).bg(Palette::PANEL)
    }

    /// Success style.
    pub fn success() -> Style {
        Style::default()
            .fg(Palette::SUCCESS)
            .add_modifier(Modifier::BOLD)
    }

    /// Warning style.
    pub fn warning() -> Style {
        Style::default()
            .fg(Palette::WARNING)
            .add_modifier(Modifier::BOLD)
    }

    /// Danger/error style.
    pub fn danger() -> Style {
        Style::default()
            .fg(Palette::DANGER)
            .add_modifier(Modifier::BOLD)
    }

    /// Info style.
    pub fn info() -> Style {
        Style::default()
            .fg(Palette::INFO)
            .add_modifier(Modifier::BOLD)
    }

    /// Muted/disabled text.
    pub fn muted() -> Style {
        Style::default().fg(Palette::TEXT_MUTED)
    }

    /// Diff addition.
    pub fn diff_add() -> Style {
        Style::default().fg(Palette::ADD).bg(Palette::ADD_BG)
    }

    /// Diff removal.
    pub fn diff_remove() -> Style {
        Style::default().fg(Palette::REMOVE).bg(Palette::REMOVE_BG)
    }

    /// Diff modification.
    pub fn diff_modify() -> Style {
        Style::default().fg(Palette::MODIFY).bg(Palette::MODIFY_BG)
    }

    /// Status bar style.
    pub fn status_bar() -> Style {
        Style::default()
            .fg(Palette::STATUS_FG)
            .bg(Palette::STATUS_BG)
    }

    /// Focused input field.
    pub fn input_focus() -> Style {
        Style::default().fg(Palette::TEXT).bg(Palette::PANEL_ALT)
    }

    /// Unfocused input field.
    pub fn input_blur() -> Style {
        Style::default().fg(Palette::TEXT_DIM).bg(Palette::PANEL)
    }

    /// Help text.
    pub fn help() -> Style {
        Style::default()
            .fg(Palette::TEXT_MUTED)
            .bg(Palette::STATUS_BG)
    }

    /// Keybinding hint.
    pub fn key_hint() -> Style {
        Style::default()
            .fg(Palette::ACCENT_SECONDARY)
            .bg(Palette::STATUS_BG)
            .add_modifier(Modifier::BOLD)
    }

    /// Keybinding description.
    pub fn key_desc() -> Style {
        Style::default()
            .fg(Palette::TEXT_DIM)
            .bg(Palette::STATUS_BG)
    }
}

/// Unicode box-drawing characters for borders.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BorderStyle {
    /// Single line (light).
    Single,
    /// Double line.
    Double,
    /// Rounded corners.
    Rounded,
    /// Heavy/thick.
    Heavy,
    /// ASCII fallback.
    Ascii,
}

impl BorderStyle {
    pub const fn chars(self) -> BorderChars {
        match self {
            BorderStyle::Single => BorderChars {
                top_left: "┌",
                top_right: "┐",
                bottom_left: "└",
                bottom_right: "┘",
                horizontal: "─",
                vertical: "│",
                tee_up: "┴",
                tee_down: "┬",
                tee_left: "┤",
                tee_right: "├",
                cross: "┼",
            },
            BorderStyle::Double => BorderChars {
                top_left: "╔",
                top_right: "╗",
                bottom_left: "╚",
                bottom_right: "╝",
                horizontal: "═",
                vertical: "║",
                tee_up: "╩",
                tee_down: "╦",
                tee_left: "╣",
                tee_right: "╠",
                cross: "╬",
            },
            BorderStyle::Rounded => BorderChars {
                top_left: "╭",
                top_right: "╮",
                bottom_left: "╰",
                bottom_right: "╯",
                horizontal: "─",
                vertical: "│",
                tee_up: "┴",
                tee_down: "┬",
                tee_left: "┤",
                tee_right: "├",
                cross: "┼",
            },
            BorderStyle::Heavy => BorderChars {
                top_left: "┏",
                top_right: "┓",
                bottom_left: "┗",
                bottom_right: "┛",
                horizontal: "━",
                vertical: "┃",
                tee_up: "┻",
                tee_down: "┳",
                tee_left: "┫",
                tee_right: "┣",
                cross: "╋",
            },
            BorderStyle::Ascii => BorderChars {
                top_left: "+",
                top_right: "+",
                bottom_left: "+",
                bottom_right: "+",
                horizontal: "-",
                vertical: "|",
                tee_up: "+",
                tee_down: "+",
                tee_left: "+",
                tee_right: "+",
                cross: "+",
            },
        }
    }
}

/// Border character set.
#[derive(Debug, Clone, Copy)]
pub struct BorderChars {
    pub top_left: &'static str,
    pub top_right: &'static str,
    pub bottom_left: &'static str,
    pub bottom_right: &'static str,
    pub horizontal: &'static str,
    pub vertical: &'static str,
    pub tee_up: &'static str,
    pub tee_down: &'static str,
    pub tee_left: &'static str,
    pub tee_right: &'static str,
    pub cross: &'static str,
}

/// Default border style for the application.
pub const DEFAULT_BORDER: BorderStyle = BorderStyle::Rounded;

/// Create a styled span for a keybinding hint (e.g., "q" in "q: quit").
pub fn key_span(key: &str) -> Span<'static> {
    Span::styled(key.to_string(), Styles::key_hint())
}

/// Create a styled span for a keybinding description.
pub fn desc_span(desc: &str) -> Span<'static> {
    Span::styled(desc.to_string(), Styles::key_desc())
}

/// Create a styled span for a keybinding with description.
pub fn keybinding_span(key: &str, desc: &str) -> Vec<Span<'static>> {
    vec![
        key_span(key),
        Span::styled(" ", Styles::help()),
        desc_span(desc),
    ]
}

/// Truncate a string to fit within a given width, adding ellipsis if needed.
pub fn truncate(s: &str, width: usize) -> String {
    if s.len() <= width {
        s.to_string()
    } else if width <= 3 {
        "...".to_string()
    } else {
        format!("{}...", &s[..width - 3])
    }
}

/// Pad a string to a given width.
pub fn pad_right(s: &str, width: usize) -> String {
    if s.len() >= width {
        truncate(s, width)
    } else {
        format!("{}{}", s, " ".repeat(width - s.len()))
    }
}

/// Pad a string on the left.
pub fn pad_left(s: &str, width: usize) -> String {
    if s.len() >= width {
        truncate(s, width)
    } else {
        format!("{}{}", " ".repeat(width - s.len()), s)
    }
}

/// Center a string within a given width.
pub fn center(s: &str, width: usize) -> String {
    if s.len() >= width {
        truncate(s, width)
    } else {
        let padding = width - s.len();
        let left = padding / 2;
        let right = padding - left;
        format!("{}{}{}", " ".repeat(left), s, " ".repeat(right))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_truncate() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("hello world", 8), "hello...");
        assert_eq!(truncate("hi", 2), "hi");
        assert_eq!(truncate("hi", 1), "...");
    }

    #[test]
    fn test_pad_right() {
        assert_eq!(pad_right("hi", 5), "hi   ");
        // When width <= 3, truncate uses ellipsis
        assert_eq!(pad_right("hello", 3), "...");
    }

    #[test]
    fn test_pad_left() {
        assert_eq!(pad_left("hi", 5), "   hi");
        // When width <= 3, truncate uses ellipsis
        assert_eq!(pad_left("hello", 3), "...");
    }

    #[test]
    fn test_center() {
        assert_eq!(center("hi", 5), " hi  ");
        // center("hello", 9) has padding=4, left=2, right=2 => "  hello  "
        assert_eq!(center("hello", 9), "  hello  ");
    }
}
