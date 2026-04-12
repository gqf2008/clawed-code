//! Crossterm-based input widget for the TUI.
//!
//! Provides a multi-line input with:
//! - UTF-8-aware cursor management
//! - Multi-line editing (Shift+Enter inserts newline, up to MAX_INPUT_ROWS)
//! - Basic editing (Backspace, Delete, Ctrl+A/E/W/U, left/right arrows)
//! - History navigation (Up/Down when at first/last line)
//! - Slash command completion (Tab)
//!
//! Reuses `SLASH_COMMANDS` from `crate::input`.

#![allow(dead_code)]

use crate::input::SLASH_COMMANDS;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use unicode_width::UnicodeWidthStr;

/// Maximum number of visible input rows.
pub const MAX_INPUT_ROWS: usize = 5;

/// Slash command completion state.
struct CompletionState {
    matches: Vec<usize>, // Indices into SLASH_COMMANDS
    selected: usize,
}

pub struct InputWidget {
    buffer: String,
    cursor: usize, // char index into the flat buffer
    history: Vec<String>,
    history_idx: usize,
    history_saved: Option<String>, // Buffer snapshot when navigating history
    completion: Option<CompletionState>,
}

pub enum InputAction {
    None,
    Submit,
    Abort,
    Quit,
    Changed,
}

/// Map a char index to a byte index in the buffer.
fn char_to_byte(s: &str, char_idx: usize) -> usize {
    s.char_indices().nth(char_idx).map_or(s.len(), |(b, _)| b)
}

impl InputWidget {
    pub fn new() -> Self {
        Self {
            buffer: String::new(),
            cursor: 0,
            history: Vec::new(),
            history_idx: 0,
            history_saved: None,
            completion: None,
        }
    }

    pub fn load_history(&mut self, history: Vec<String>) {
        self.history = history;
        self.history_idx = self.history.len();
    }

    pub fn push_history(&mut self, text: String) {
        let trimmed = text.trim().to_string();
        if trimmed.is_empty() {
            return;
        }
        // Deduplicate consecutive entries
        if self.history.last().is_some_and(|h| h == &trimmed) {
            return;
        }
        self.history.push(trimmed);
        self.history_idx = self.history.len();
    }

    // -- Cursor geometry helpers -----------------------------------------------

    /// Split the buffer into lines, returning (line_text, start_char_index) pairs.
    fn line_spans(&self) -> Vec<(&str, usize)> {
        let mut spans = Vec::new();
        let mut char_offset = 0;
        for line in self.buffer.split('\n') {
            spans.push((line, char_offset));
            char_offset += line.chars().count() + 1; // +1 for the newline
        }
        spans
    }

    /// Which logical line the cursor is on, and its column within that line.
    fn cursor_line_col(&self) -> (usize, usize) {
        let spans = self.line_spans();
        for (i, &(line_text, start)) in spans.iter().enumerate() {
            let line_len = line_text.chars().count();
            let end = start + line_len;
            if self.cursor <= end || i == spans.len() - 1 {
                return (i, self.cursor.saturating_sub(start).min(line_len));
            }
        }
        (0, 0)
    }

    /// Number of lines in the buffer.
    pub fn line_count(&self) -> usize {
        self.buffer.split('\n').count().max(1)
    }

    /// Number of visible rows (capped at `MAX_INPUT_ROWS`).
    pub fn visible_rows(&self) -> u16 {
        self.line_count().clamp(1, MAX_INPUT_ROWS) as u16
    }

    /// Cursor row and column for rendering (0-indexed).
    pub fn cursor_position(&self) -> (usize, usize) {
        if self.completion.is_some() {
            let text = self.display_text();
            return (0, text.width());
        }
        let (row, col_char) = self.cursor_line_col();
        let byte = char_to_byte(&self.buffer, self.cursor);
        let line_start_byte = char_to_byte(&self.buffer, self.cursor - col_char.min(self.cursor));
        let col_width = self.buffer[line_start_byte..byte].width();
        (row, col_width)
    }

    // -- Key handling ----------------------------------------------------------

    /// Handle a key event. Returns the action to take.
    pub fn handle_key(&mut self, key: KeyEvent) -> InputAction {
        if !matches!(
            key.kind,
            crossterm::event::KeyEventKind::Press | crossterm::event::KeyEventKind::Repeat
        ) {
            return InputAction::None;
        }

        match key.code {
            // Submit (Enter without Shift) or accept completion
            KeyCode::Enter if !key.modifiers.contains(KeyModifiers::SHIFT) => {
                if let Some(ref comp) = self.completion {
                    // Accept completion
                    let idx = comp.matches[comp.selected];
                    self.buffer = SLASH_COMMANDS[idx].to_string();
                    self.cursor = self.buffer.chars().count();
                    self.completion = None;
                    return InputAction::Changed;
                }
                self.completion = None;
                InputAction::Submit
            }

            // Shift+Enter: insert newline (multi-line mode)
            KeyCode::Enter if key.modifiers.contains(KeyModifiers::SHIFT) => {
                if self.line_count() < MAX_INPUT_ROWS {
                    self.completion = None;
                    let byte = char_to_byte(&self.buffer, self.cursor);
                    self.buffer.insert(byte, '\n');
                    self.cursor += 1;
                    InputAction::Changed
                } else {
                    InputAction::None
                }
            }

            // Abort (Ctrl+C)
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                InputAction::Abort
            }

            // Quit (Esc)
            KeyCode::Esc => InputAction::Quit,

            // Tab completion
            KeyCode::Tab => {
                if self.buffer.starts_with('/') {
                    if let Some(ref mut comp) = self.completion {
                        if comp.matches.len() == 1 {
                            let idx = comp.matches[0];
                            self.buffer = SLASH_COMMANDS[idx].to_string();
                            self.cursor = self.buffer.chars().count();
                            self.completion = None;
                        } else {
                            comp.selected = (comp.selected + 1) % comp.matches.len();
                        }
                    } else {
                        self.update_completion();
                        if let Some(ref comp) = self.completion {
                            if comp.matches.len() == 1 {
                                let idx = comp.matches[0];
                                self.buffer = SLASH_COMMANDS[idx].to_string();
                                self.cursor = self.buffer.chars().count();
                                self.completion = None;
                            }
                        }
                    }
                    InputAction::Changed
                } else {
                    InputAction::None
                }
            }

            // BackTab (Shift+Tab): cycle selection upward
            KeyCode::BackTab => {
                if let Some(ref mut comp) = self.completion {
                    if comp.selected > 0 {
                        comp.selected -= 1;
                    } else {
                        comp.selected = comp.matches.len().saturating_sub(1);
                    }
                }
                InputAction::Changed
            }

            KeyCode::Down => {
                if let Some(ref mut comp) = self.completion {
                    if comp.selected + 1 < comp.matches.len() {
                        comp.selected += 1;
                    }
                    return InputAction::Changed;
                }
                // Multi-line: if not on the last line, move cursor down
                let (row, col) = self.cursor_line_col();
                let spans = self.line_spans();
                if row + 1 < spans.len() {
                    let (next_text, next_start) = spans[row + 1];
                    let next_len = next_text.chars().count();
                    self.cursor = next_start + col.min(next_len);
                    InputAction::Changed
                } else {
                    self.handle_history_down()
                }
            }

            KeyCode::Up => {
                if let Some(ref mut comp) = self.completion {
                    if comp.selected > 0 {
                        comp.selected -= 1;
                    }
                    return InputAction::Changed;
                }
                // Multi-line: if not on the first line, move cursor up
                let (row, col) = self.cursor_line_col();
                if row > 0 {
                    let spans = self.line_spans();
                    let (prev_text, prev_start) = spans[row - 1];
                    let prev_len = prev_text.chars().count();
                    self.cursor = prev_start + col.min(prev_len);
                    InputAction::Changed
                } else {
                    self.handle_history_up()
                }
            }

            // Cursor movement
            KeyCode::Left => {
                self.completion = None;
                if self.cursor > 0 {
                    self.cursor -= 1;
                    InputAction::Changed
                } else {
                    InputAction::None
                }
            }

            KeyCode::Right => {
                self.completion = None;
                let char_count = self.buffer.chars().count();
                if self.cursor < char_count {
                    self.cursor += 1;
                    InputAction::Changed
                } else {
                    InputAction::None
                }
            }

            // Home / Ctrl+A → beginning of current line
            KeyCode::Home => {
                self.completion = None;
                let (_, _, start) = self.current_line_info();
                self.cursor = start;
                InputAction::Changed
            }
            KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.completion = None;
                let (_, _, start) = self.current_line_info();
                self.cursor = start;
                InputAction::Changed
            }

            // End / Ctrl+E → end of current line
            KeyCode::End => {
                self.completion = None;
                let (line_text, _, start) = self.current_line_info();
                self.cursor = start + line_text.chars().count();
                InputAction::Changed
            }
            KeyCode::Char('e') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.completion = None;
                let (line_text, _, start) = self.current_line_info();
                self.cursor = start + line_text.chars().count();
                InputAction::Changed
            }

            // Deletion
            KeyCode::Backspace => {
                self.completion = None;
                if self.cursor > 0 {
                    let start = char_to_byte(&self.buffer, self.cursor - 1);
                    let end = char_to_byte(&self.buffer, self.cursor);
                    self.buffer.drain(start..end);
                    self.cursor -= 1;
                    InputAction::Changed
                } else {
                    InputAction::None
                }
            }

            KeyCode::Delete => {
                self.completion = None;
                let char_count = self.buffer.chars().count();
                if self.cursor < char_count {
                    let start = char_to_byte(&self.buffer, self.cursor);
                    let end = char_to_byte(&self.buffer, self.cursor + 1);
                    self.buffer.drain(start..end);
                    InputAction::Changed
                } else {
                    InputAction::None
                }
            }

            KeyCode::Char('w') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.completion = None;
                self.delete_word_backward();
                InputAction::Changed
            }

            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.completion = None;
                self.buffer.clear();
                self.cursor = 0;
                InputAction::Changed
            }

            // Character insertion
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.completion = None;
                let byte = char_to_byte(&self.buffer, self.cursor);
                self.buffer.insert(byte, c);
                self.cursor += 1;
                InputAction::Changed
            }

            _ => InputAction::None,
        }
    }

    /// Get info about the line the cursor is on: (line_text, row, start_char).
    fn current_line_info(&self) -> (String, usize, usize) {
        let spans = self.line_spans();
        let (row, _) = self.cursor_line_col();
        let (line_text, start) = spans[row];
        (line_text.to_string(), row, start)
    }

    fn handle_history_up(&mut self) -> InputAction {
        if self.history.is_empty() || self.history_idx == 0 {
            return InputAction::None;
        }
        if self.history_saved.is_none() {
            self.history_saved = Some(self.buffer.clone());
        }
        self.history_idx -= 1;
        self.buffer = self.history[self.history_idx].clone();
        self.cursor = self.buffer.chars().count();
        InputAction::Changed
    }

    fn handle_history_down(&mut self) -> InputAction {
        if self.history_idx >= self.history.len() {
            return InputAction::None;
        }
        self.history_idx += 1;
        if self.history_idx == self.history.len() {
            self.buffer = self.history_saved.take().unwrap_or_default();
        } else {
            self.buffer = self.history[self.history_idx].clone();
        }
        self.cursor = self.buffer.chars().count();
        InputAction::Changed
    }

    fn delete_word_backward(&mut self) {
        let chars: Vec<char> = self.buffer.chars().collect();
        let mut new_cursor = self.cursor;
        // Skip trailing spaces
        while new_cursor > 0 && chars[new_cursor - 1] == ' ' {
            new_cursor -= 1;
        }
        // Skip the word
        while new_cursor > 0 && chars[new_cursor - 1] != ' ' {
            new_cursor -= 1;
        }
        let start = char_to_byte(&self.buffer, new_cursor);
        let end = char_to_byte(&self.buffer, self.cursor);
        self.buffer.drain(start..end);
        self.cursor = new_cursor;
    }

    fn update_completion(&mut self) {
        if !self.buffer.starts_with('/') {
            self.completion = None;
            return;
        }
        let prefix = &self.buffer;
        let matches: Vec<usize> = SLASH_COMMANDS
            .iter()
            .enumerate()
            .filter_map(|(i, cmd)| cmd.starts_with(prefix).then_some(i))
            .collect();
        if matches.is_empty() {
            self.completion = None;
        } else {
            self.completion = Some(CompletionState {
                matches,
                selected: 0,
            });
        }
    }

    /// Take the current buffer text for submission.
    pub fn take_text(&mut self) -> String {
        let text = self.buffer.clone();
        if !text.trim().is_empty() {
            self.push_history(text.clone());
        }
        self.buffer.clear();
        self.cursor = 0;
        self.history_saved = None;
        self.completion = None;
        text
    }

    /// Check if input is in completion mode.
    pub fn in_completion(&self) -> bool {
        self.completion.is_some()
    }

    /// Get a reference to the history entries (for persistence).
    pub fn history(&self) -> &[String] {
        &self.history
    }

    /// Get completion matches and selected index.
    pub fn completion_matches(&self) -> Vec<&str> {
        self.completion
            .iter()
            .flat_map(|comp| comp.matches.iter().map(|&i| SLASH_COMMANDS[i]))
            .collect()
    }

    pub fn completion_selected(&self) -> usize {
        self.completion.as_ref().map(|c| c.selected).unwrap_or(0)
    }

    /// Get current buffer for display.
    pub fn buffer(&self) -> &str {
        &self.buffer
    }

    /// Get cursor position (char index).
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Get the visual column width of the display text (for cursor positioning).
    pub fn display_width(&self) -> usize {
        if let Some(ref comp) = self.completion {
            let idx = comp.matches[comp.selected];
            SLASH_COMMANDS[idx].width()
        } else {
            self.buffer.width()
        }
    }

    /// Get the text to display (may show completion suggestion).
    pub fn display_text(&self) -> &str {
        if let Some(ref comp) = self.completion {
            SLASH_COMMANDS[comp.matches[comp.selected]]
        } else {
            &self.buffer
        }
    }

    /// Get cursor column for the display text (single-line compat).
    pub fn display_cursor_col(&self) -> usize {
        self.cursor_position().1
    }

    /// Get display lines for multi-line rendering.
    pub fn display_lines(&self) -> Vec<&str> {
        if self.completion.is_some() {
            return vec![self.display_text()];
        }
        self.buffer.split('\n').collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn shift_enter() -> KeyEvent {
        KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT)
    }

    #[test]
    fn test_type_text() {
        let mut w = InputWidget::new();
        assert!(matches!(
            w.handle_key(key(KeyCode::Char('h'))),
            InputAction::Changed
        ));
        assert!(matches!(
            w.handle_key(key(KeyCode::Char('i'))),
            InputAction::Changed
        ));
        assert_eq!(w.buffer(), "hi");
        assert_eq!(w.cursor(), 2);
    }

    #[test]
    fn test_backspace() {
        let mut w = InputWidget::new();
        w.handle_key(key(KeyCode::Char('h')));
        w.handle_key(key(KeyCode::Char('i')));
        w.handle_key(key(KeyCode::Backspace));
        assert_eq!(w.buffer(), "h");
        assert_eq!(w.cursor(), 1);
    }

    #[test]
    fn test_cursor_movement() {
        let mut w = InputWidget::new();
        w.handle_key(key(KeyCode::Char('a')));
        w.handle_key(key(KeyCode::Char('b')));
        w.handle_key(key(KeyCode::Char('c')));
        assert_eq!(w.cursor(), 3);
        w.handle_key(key(KeyCode::Left));
        assert_eq!(w.cursor(), 2);
        w.handle_key(key(KeyCode::Left));
        assert_eq!(w.cursor(), 1);
        w.handle_key(key(KeyCode::Right));
        assert_eq!(w.cursor(), 2);
    }

    #[test]
    fn test_ctrl_a_e() {
        let mut w = InputWidget::new();
        w.handle_key(key(KeyCode::Char('h')));
        w.handle_key(key(KeyCode::Char('i')));
        w.handle_key(ctrl('a'));
        assert_eq!(w.cursor(), 0);
        w.handle_key(ctrl('e'));
        assert_eq!(w.cursor(), 2);
    }

    #[test]
    fn test_ctrl_w_delete_word() {
        let mut w = InputWidget::new();
        for c in "hello world".chars() {
            w.handle_key(key(KeyCode::Char(c)));
        }
        w.handle_key(ctrl('w'));
        assert_eq!(w.buffer(), "hello ");
        assert_eq!(w.cursor(), 6);
    }

    #[test]
    fn test_ctrl_u_clear() {
        let mut w = InputWidget::new();
        w.handle_key(key(KeyCode::Char('h')));
        w.handle_key(key(KeyCode::Char('i')));
        w.handle_key(ctrl('u'));
        assert_eq!(w.buffer(), "");
        assert_eq!(w.cursor(), 0);
    }

    #[test]
    fn test_enter_submit() {
        let mut w = InputWidget::new();
        w.handle_key(key(KeyCode::Char('h')));
        assert!(matches!(
            w.handle_key(key(KeyCode::Enter)),
            InputAction::Submit
        ));
        assert_eq!(w.buffer(), "h");
        assert_eq!(w.take_text(), "h");
        assert_eq!(w.buffer(), "");
    }

    #[test]
    fn test_escape_quit() {
        let mut w = InputWidget::new();
        assert!(matches!(
            w.handle_key(key(KeyCode::Esc)),
            InputAction::Quit
        ));
    }

    #[test]
    fn test_history_navigation() {
        let mut w = InputWidget::new();
        w.push_history("hello".to_string());
        w.push_history("world".to_string());
        w.handle_key(key(KeyCode::Char('t')));
        assert!(matches!(
            w.handle_key(key(KeyCode::Up)),
            InputAction::Changed
        ));
        assert_eq!(w.buffer(), "world");
        assert!(matches!(
            w.handle_key(key(KeyCode::Up)),
            InputAction::Changed
        ));
        assert_eq!(w.buffer(), "hello");
        assert!(matches!(
            w.handle_key(key(KeyCode::Down)),
            InputAction::Changed
        ));
        assert_eq!(w.buffer(), "world");
        assert!(matches!(
            w.handle_key(key(KeyCode::Down)),
            InputAction::Changed
        ));
        assert_eq!(w.buffer(), "t"); // Restored typed text
    }

    #[test]
    fn test_home_end_without_ctrl() {
        let mut w = InputWidget::new();
        for c in "hello".chars() {
            w.handle_key(key(KeyCode::Char(c)));
        }
        assert_eq!(w.cursor(), 5);
        w.handle_key(key(KeyCode::Home));
        assert_eq!(w.cursor(), 0);
        w.handle_key(key(KeyCode::End));
        assert_eq!(w.cursor(), 5);
    }

    #[test]
    fn test_ctrl_c_abort() {
        let mut w = InputWidget::new();
        assert!(matches!(w.handle_key(ctrl('c')), InputAction::Abort));
    }

    #[test]
    fn test_tab_cycles_completions() {
        let mut w = InputWidget::new();
        w.handle_key(key(KeyCode::Char('/')));
        w.handle_key(key(KeyCode::Tab));
        assert!(w.in_completion());
        assert_eq!(w.completion_selected(), 0);
        w.handle_key(key(KeyCode::Tab));
        assert!(w.in_completion());
        assert_eq!(w.completion_selected(), 1);
    }

    #[test]
    fn test_history_accessor() {
        let mut w = InputWidget::new();
        w.push_history("first".to_string());
        w.push_history("second".to_string());
        assert_eq!(w.history(), &["first", "second"]);
    }

    // -- Multi-line tests --

    #[test]
    fn test_shift_enter_newline() {
        let mut w = InputWidget::new();
        for c in "line1".chars() {
            w.handle_key(key(KeyCode::Char(c)));
        }
        assert!(matches!(w.handle_key(shift_enter()), InputAction::Changed));
        for c in "line2".chars() {
            w.handle_key(key(KeyCode::Char(c)));
        }
        assert_eq!(w.buffer(), "line1\nline2");
        assert_eq!(w.line_count(), 2);
        assert_eq!(w.visible_rows(), 2);
    }

    #[test]
    fn test_multiline_cursor_navigation() {
        let mut w = InputWidget::new();
        // Type "abc\ndef"
        for c in "abc".chars() {
            w.handle_key(key(KeyCode::Char(c)));
        }
        w.handle_key(shift_enter());
        for c in "def".chars() {
            w.handle_key(key(KeyCode::Char(c)));
        }
        // Cursor should be at end of "def" (row=1, col=3)
        let (row, col) = w.cursor_position();
        assert_eq!(row, 1);
        assert_eq!(col, 3);

        // Up arrow should move to row 0
        w.handle_key(key(KeyCode::Up));
        let (row, _) = w.cursor_position();
        assert_eq!(row, 0);

        // Down should go back to row 1
        w.handle_key(key(KeyCode::Down));
        let (row, _) = w.cursor_position();
        assert_eq!(row, 1);
    }

    #[test]
    fn test_multiline_home_end() {
        let mut w = InputWidget::new();
        for c in "abc".chars() {
            w.handle_key(key(KeyCode::Char(c)));
        }
        w.handle_key(shift_enter());
        for c in "defgh".chars() {
            w.handle_key(key(KeyCode::Char(c)));
        }
        // Home should go to start of current line (line 2), not start of buffer
        w.handle_key(key(KeyCode::Home));
        assert_eq!(w.cursor(), 4); // char index 4 = start of "defgh" (after "abc\n")
        // End should go to end of current line
        w.handle_key(key(KeyCode::End));
        assert_eq!(w.cursor(), 9); // 3 + 1 + 5 = 9
    }

    #[test]
    fn test_max_input_rows_limit() {
        let mut w = InputWidget::new();
        // Insert MAX_INPUT_ROWS - 1 newlines (should succeed)
        for _ in 0..MAX_INPUT_ROWS - 1 {
            w.handle_key(key(KeyCode::Char('x')));
            w.handle_key(shift_enter());
        }
        assert_eq!(w.line_count(), MAX_INPUT_ROWS);
        // One more should be blocked
        assert!(matches!(w.handle_key(shift_enter()), InputAction::None));
        assert_eq!(w.line_count(), MAX_INPUT_ROWS);
    }

    #[test]
    fn test_up_on_first_line_goes_to_history() {
        let mut w = InputWidget::new();
        w.push_history("old".to_string());
        for c in "new".chars() {
            w.handle_key(key(KeyCode::Char(c)));
        }
        // On single line, Up = history
        assert!(matches!(w.handle_key(key(KeyCode::Up)), InputAction::Changed));
        assert_eq!(w.buffer(), "old");
    }

    #[test]
    fn test_display_lines() {
        let mut w = InputWidget::new();
        for c in "a".chars() {
            w.handle_key(key(KeyCode::Char(c)));
        }
        w.handle_key(shift_enter());
        for c in "b".chars() {
            w.handle_key(key(KeyCode::Char(c)));
        }
        let lines = w.display_lines();
        assert_eq!(lines, vec!["a", "b"]);
    }
}
