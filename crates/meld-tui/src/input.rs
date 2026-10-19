//! Grapheme-aware text input.
//!
//! A single reusable text buffer used by the command bar, connection fields,
//! JSON fields, and the SQL editor (single-line and multiline modes). The
//! cursor is tracked by (line index, grapheme index), never by byte offset,
//! so Unicode editing (e.g. `é`, `日本`) is safe.

use unicode_width::UnicodeWidthChar;

/// A single grapheme cluster with its display width.
#[derive(Clone, Debug)]
struct Grapheme {
    text: String,
    width: u16,
}

impl Grapheme {
    fn new(s: &str) -> Self {
        let width = s
            .chars()
            .map(|c| UnicodeWidthChar::width(c).unwrap_or(0))
            .sum::<usize>() as u16;
        Self {
            text: s.to_string(),
            width,
        }
    }
}

/// A line of text: a sequence of graphemes.
type Line = Vec<Grapheme>;

/// Input mode of the application.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputMode {
    /// Normal navigation mode: global shortcuts are active.
    Normal,
    /// Editing a focused field. Every printable key goes to the buffer.
    Editing,
    /// Command bar is focused (`:`).
    Command,
    /// A confirmation dialog is shown.
    Confirm,
}

/// A grapheme-aware, multiline text buffer.
#[derive(Clone)]
pub struct TextBuffer {
    lines: Vec<Line>,
    cursor_line: usize,
    cursor_grapheme: usize,
    /// Horizontal scroll in display columns (single-line mode).
    hscroll: u16,
    /// Vertical scroll in lines (multiline mode).
    vscroll: usize,
    multiline: bool,
}

impl TextBuffer {
    /// Create an empty single-line buffer.
    pub fn single_line() -> Self {
        Self {
            lines: vec![vec![]],
            cursor_line: 0,
            cursor_grapheme: 0,
            hscroll: 0,
            vscroll: 0,
            multiline: false,
        }
    }

    /// Create an empty multiline buffer.
    pub fn multiline() -> Self {
        Self {
            multiline: true,
            ..Self::single_line()
        }
    }

    /// Replace the entire contents.
    pub fn set_text(&mut self, text: &str) {
        self.lines = text
            .split('\n')
            .map(|seg| graphemes(seg).into_iter().map(Grapheme::new).collect())
            .collect();
        if self.lines.is_empty() {
            self.lines = vec![vec![]];
        }
        self.cursor_line = 0;
        self.cursor_grapheme = 0;
    }

    /// The full text content (lines joined with `\n`).
    pub fn text(&self) -> String {
        self.lines
            .iter()
            .map(|line| line.iter().map(|g| g.text.as_str()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Current line index.
    pub fn cursor_line(&self) -> usize {
        self.cursor_line
    }

    /// Current grapheme index within the current line.
    pub fn cursor_grapheme(&self) -> usize {
        self.cursor_grapheme
    }

    /// Number of lines.
    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    /// Number of graphemes on a line.
    pub fn line_len(&self, line: usize) -> usize {
        self.lines.get(line).map(Vec::len).unwrap_or(0)
    }

    /// Display column of the cursor on its line.
    pub fn cursor_column(&self) -> u16 {
        self.lines
            .get(self.cursor_line)
            .map(|line| {
                line.iter()
                    .take(self.cursor_grapheme)
                    .map(|g| g.width)
                    .sum::<u16>()
            })
            .unwrap_or(0)
    }

    /// Insert a grapheme (may be multi-char) at the cursor.
    pub fn insert(&mut self, text: &str) {
        let graphemes = graphemes(text);
        if graphemes.is_empty() {
            return;
        }
        let line = self
            .lines
            .get_mut(self.cursor_line)
            .expect("cursor line exists");
        let new: Vec<Grapheme> = graphemes.into_iter().map(Grapheme::new).collect();
        let split = self.cursor_grapheme;
        let new_len = new.len();
        let mut after = line.split_off(split);
        line.extend(new);
        line.append(&mut after);
        self.cursor_grapheme += new_len;
    }

    /// Delete the grapheme before the cursor. Returns true if something was
    /// deleted. Joins with the previous line when at the start of a line in
    /// multiline mode.
    pub fn delete_back(&mut self) -> bool {
        if self.cursor_grapheme > 0 {
            let line = self
                .lines
                .get_mut(self.cursor_line)
                .expect("cursor line exists");
            line.remove(self.cursor_grapheme - 1);
            self.cursor_grapheme -= 1;
            return true;
        }
        if self.multiline && self.cursor_line > 0 {
            let cur = self.lines.remove(self.cursor_line);
            self.cursor_line -= 1;
            let prev_len = self.lines[self.cursor_line].len();
            self.lines[self.cursor_line].extend(cur);
            self.cursor_grapheme = prev_len;
            return true;
        }
        false
    }

    /// Delete the grapheme after the cursor. Returns true if something was
    /// deleted. Joins with the next line when at the end of a line in
    /// multiline mode.
    pub fn delete_forward(&mut self) -> bool {
        let line_len = self.lines.get(self.cursor_line).map(Vec::len).unwrap_or(0);
        if self.cursor_grapheme < line_len {
            let line = self
                .lines
                .get_mut(self.cursor_line)
                .expect("cursor line exists");
            line.remove(self.cursor_grapheme);
            return true;
        }
        if self.multiline && self.cursor_line + 1 < self.lines.len() {
            let next = self.lines.remove(self.cursor_line + 1);
            let line = self
                .lines
                .get_mut(self.cursor_line)
                .expect("cursor line exists");
            line.extend(next);
            return true;
        }
        false
    }

    /// Move cursor left by one grapheme.
    pub fn move_left(&mut self) {
        if self.cursor_grapheme > 0 {
            self.cursor_grapheme -= 1;
        } else if self.multiline && self.cursor_line > 0 {
            self.cursor_line -= 1;
            self.cursor_grapheme = self.lines[self.cursor_line].len();
        }
    }

    /// Move cursor right by one grapheme.
    pub fn move_right(&mut self) {
        let line_len = self.lines.get(self.cursor_line).map(Vec::len).unwrap_or(0);
        if self.cursor_grapheme < line_len {
            self.cursor_grapheme += 1;
        } else if self.multiline && self.cursor_line + 1 < self.lines.len() {
            self.cursor_line += 1;
            self.cursor_grapheme = 0;
        }
    }

    /// Move cursor to the start of the current line.
    pub fn move_home(&mut self) {
        self.cursor_grapheme = 0;
    }

    /// Move cursor to the end of the current line.
    pub fn move_end(&mut self) {
        self.cursor_grapheme = self.lines.get(self.cursor_line).map(Vec::len).unwrap_or(0);
    }

    /// Move cursor up one line (multiline).
    pub fn move_up(&mut self) {
        if !self.multiline || self.cursor_line == 0 {
            return;
        }
        self.cursor_line -= 1;
        let len = self.lines[self.cursor_line].len();
        self.cursor_grapheme = self.cursor_grapheme.min(len);
    }

    /// Move cursor down one line (multiline).
    pub fn move_down(&mut self) {
        if !self.multiline || self.cursor_line + 1 >= self.lines.len() {
            return;
        }
        self.cursor_line += 1;
        let len = self.lines[self.cursor_line].len();
        self.cursor_grapheme = self.cursor_grapheme.min(len);
    }

    /// Insert a newline, splitting the current line (multiline only).
    pub fn newline(&mut self) {
        if !self.multiline {
            return;
        }
        let after = {
            let line = self
                .lines
                .get_mut(self.cursor_line)
                .expect("cursor line exists");
            line.split_off(self.cursor_grapheme)
        };
        self.lines.insert(self.cursor_line + 1, after);
        self.cursor_line += 1;
        self.cursor_grapheme = 0;
    }

    /// The rendered text of the current line, optionally horizontally scrolled
    /// (single-line mode).
    pub fn current_line_display(&self) -> String {
        let line: &[Grapheme] = match self.lines.get(self.cursor_line) {
            Some(l) => l,
            None => return String::new(),
        };
        let mut out = String::new();
        let mut col = 0u16;
        let mut start = 0usize;
        // Skip leading graphemes until we pass hscroll.
        for (i, g) in line.iter().enumerate() {
            if col + g.width > self.hscroll {
                start = i;
                break;
            }
            col += g.width;
        }
        for g in line.iter().skip(start) {
            out.push_str(&g.text);
        }
        out
    }

    /// Set horizontal scroll so the cursor is visible within `width` columns.
    pub fn ensure_cursor_visible(&mut self, width: u16) {
        if !self.multiline {
            let col = self.cursor_column();
            if col < self.hscroll {
                self.hscroll = col.saturating_sub(1);
            } else if col >= self.hscroll.saturating_add(width.saturating_sub(1)) {
                self.hscroll = col.saturating_sub(width.saturating_sub(2));
            }
        }
    }

    /// Vertical scroll so the cursor line is visible within `height` lines.
    pub fn ensure_line_visible(&mut self, height: usize) {
        if self.cursor_line < self.vscroll {
            self.vscroll = self.cursor_line;
        } else if self.cursor_line >= self.vscroll.saturating_add(height) {
            self.vscroll = self.cursor_line.saturating_sub(height - 1);
        }
    }

    /// Horizontal scroll in display columns.
    pub fn hscroll(&self) -> u16 {
        self.hscroll
    }
}

impl Default for TextBuffer {
    fn default() -> Self {
        Self::single_line()
    }
}

/// Split a string into grapheme clusters.
fn graphemes(s: &str) -> Vec<&str> {
    unicode_segmentation::UnicodeSegmentation::graphemes(s, true).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grapheme_cursor_unicode_insert() {
        let mut b = TextBuffer::single_line();
        b.insert("ab");
        b.insert("é"); // 2 bytes, 1 grapheme, width 1
        b.insert("日"); // 3 bytes, width 2
        assert_eq!(b.text(), "abé日");
        // Cursor is after all four graphemes: a, b, é, 日.
        assert_eq!(b.cursor_grapheme(), 4);
        // Display width: a(1)+b(1)+é(1)+日(2) = 5.
        assert_eq!(b.cursor_column(), 5);
    }

    #[test]
    fn delete_back_removes_grapheme_not_byte() {
        let mut b = TextBuffer::single_line();
        b.insert("é日");
        b.move_left(); // cursor is before 日, after é
        b.delete_back(); // removes the grapheme before the cursor: é
        assert_eq!(b.text(), "日");
    }

    #[test]
    fn multiline_newline_and_delete_back_join() {
        let mut b = TextBuffer::multiline();
        b.insert("hello");
        b.newline();
        b.insert("world");
        assert_eq!(b.text(), "hello\nworld");
        b.move_home();
        b.move_up(); // stays on line 0
        b.delete_back(); // start of line 0, multiline -> no-op (no previous)
        assert_eq!(b.text(), "hello\nworld");
        b.move_end();
        b.move_down(); // line 1 end
        b.delete_back(); // remove 'd'
        assert_eq!(b.text(), "hello\nworl");
    }
}
