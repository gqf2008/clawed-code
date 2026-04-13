//! Crossterm-based input widget for the TUI.
//!
//! Provides a multi-line input with:
//! - UTF-8 / grapheme-aware cursor management (via `TextArea`)
//! - Multi-line editing (Ctrl+J/M, Shift+Enter, Enter with modifier)
//! - Word-level editing (Ctrl+W, Alt+B/F, Ctrl+K/Y)
//! - History navigation (Up/Down when at first/last line)
//! - Slash command completion (Tab)
//!
//! Reuses `SLASH_COMMANDS` from `crate::input`.

#![allow(dead_code)]

use crate::input::SLASH_COMMANDS;
use super::textarea::TextArea;
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use unicode_width::UnicodeWidthStr;

/// Maximum number of visible input rows.
pub const MAX_INPUT_ROWS: usize = 5;

/// Slash command completion state.
struct CompletionState {
    matches: Vec<usize>, // Indices into SLASH_COMMANDS
    selected: usize,
}

pub struct InputWidget {
    textarea: TextArea,
    scroll_offset: usize, // first visible line index (viewport top)
    history: Vec<String>,
    history_idx: usize,
    history_saved: Option<String>, // Buffer snapshot when navigating history
    completion: Option<CompletionState>,
}

pub enum InputAction {
    None,
    Submit,
    Abort,
    Changed,
}

impl InputWidget {
    pub fn new() -> Self {
        Self {
            textarea: TextArea::new(),
            scroll_offset: 0,
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
        if self.history.last().is_some_and(|h| h == &trimmed) {
            return;
        }
        self.history.push(trimmed);
        self.history_idx = self.history.len();
    }

    /// Number of lines in the buffer.
    pub fn line_count(&self) -> usize {
        self.textarea.line_count()
    }

    /// Number of visible rows (capped at `MAX_INPUT_ROWS`).
    pub fn visible_rows(&self) -> u16 {
        self.line_count().clamp(1, MAX_INPUT_ROWS) as u16
    }

    /// Cursor row and column for rendering (0-indexed, viewport-relative).
    pub fn cursor_position(&self) -> (usize, usize) {
        if self.completion.is_some() {
            let text = self.display_text();
            return (0, text.width());
        }
        let (row, col) = self.cursor_line_col();
        (row.saturating_sub(self.scroll_offset), col)
    }

    // -- Key handling ----------------------------------------------------------

    /// Handle a key event. Returns the action to take.
    pub fn handle_key(&mut self, key: KeyEvent) -> InputAction {
        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return InputAction::None;
        }

        let action = match key.code {
            // Submit on plain Enter (no modifier), or accept completion.
            KeyCode::Enter if key.modifiers == KeyModifiers::NONE => {
                if let Some(ref comp) = self.completion {
                    let idx = comp.matches[comp.selected];
                    self.textarea.set_text(SLASH_COMMANDS[idx]);
                    self.textarea.set_cursor(self.textarea.text().len());
                    self.completion = None;
                    self.scroll_offset = 0;
                    InputAction::Changed
                } else {
                    self.completion = None;
                    InputAction::Submit
                }
            }

            // Abort (Ctrl+C)
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                InputAction::Abort
            }

            // Esc: dismiss completion → clear input → no-op (never quits)
            KeyCode::Esc => {
                if self.completion.is_some() {
                    self.completion = None;
                    InputAction::Changed
                } else if !self.textarea.is_empty() {
                    self.textarea.set_text("");
                    self.scroll_offset = 0;
                    InputAction::Changed
                } else {
                    InputAction::None
                }
            }

            // Tab: accept the currently selected completion (fill into input box).
            // Up/Down arrows handle navigation within the list.
            KeyCode::Tab => {
                if self.textarea.text().starts_with('/') {
                    if let Some(ref comp) = self.completion {
                        // Accept the highlighted item
                        let idx = comp.matches[comp.selected];
                        self.textarea.set_text(SLASH_COMMANDS[idx]);
                        self.textarea.set_cursor(self.textarea.text().len());
                        self.completion = None;
                    } else {
                        // No menu yet — trigger completion
                        self.update_completion();
                        // If only one match, auto-accept it immediately
                        if let Some(ref comp) = self.completion {
                            if comp.matches.len() == 1 {
                                let idx = comp.matches[0];
                                self.textarea.set_text(SLASH_COMMANDS[idx]);
                                self.textarea.set_cursor(self.textarea.text().len());
                                self.completion = None;
                            }
                        }
                    }
                    InputAction::Changed
                } else {
                    InputAction::None
                }
            }

            // Shift+Tab: cycle selection upward (backward navigation)
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

            // Up arrow: completion navigation → cursor up → history
            KeyCode::Up => {
                if let Some(ref mut comp) = self.completion {
                    if comp.selected > 0 {
                        comp.selected -= 1;
                    }
                    InputAction::Changed
                } else if !self.textarea.is_at_first_line() {
                    self.textarea.move_cursor_up();
                    InputAction::Changed
                } else {
                    self.handle_history_up()
                }
            }

            // Down arrow: completion navigation → cursor down → history
            KeyCode::Down => {
                if let Some(ref mut comp) = self.completion {
                    if comp.selected + 1 < comp.matches.len() {
                        comp.selected += 1;
                    }
                    InputAction::Changed
                } else if !self.textarea.is_at_last_line() {
                    self.textarea.move_cursor_down();
                    InputAction::Changed
                } else {
                    self.handle_history_down()
                }
            }

            // Key debug toggle (Ctrl+D) is handled in mod.rs, not here.
            // Everything else goes to TextArea for editing.
            _ => {
                let old_text = self.textarea.text().to_string();
                let old_cursor = self.textarea.cursor();
                self.textarea.input(key);

                let changed = self.textarea.text() != old_text
                    || self.textarea.cursor() != old_cursor;

                if changed {
                    // Update completion on slash commands
                    if self.textarea.text().starts_with('/') {
                        self.update_completion();
                    } else {
                        self.completion = None;
                    }
                    InputAction::Changed
                } else {
                    InputAction::None
                }
            }
        };

        if matches!(action, InputAction::Changed) {
            self.ensure_cursor_visible();
        }
        action
    }

    // -- History ---------------------------------------------------------------

    fn handle_history_up(&mut self) -> InputAction {
        if self.history.is_empty() || self.history_idx == 0 {
            return InputAction::None;
        }
        if self.history_saved.is_none() {
            self.history_saved = Some(self.textarea.text().to_string());
        }
        self.history_idx -= 1;
        let text = self.history[self.history_idx].clone();
        self.textarea.set_text(&text);
        self.textarea.set_cursor(self.textarea.text().len());
        InputAction::Changed
    }

    fn handle_history_down(&mut self) -> InputAction {
        if self.history_idx >= self.history.len() {
            return InputAction::None;
        }
        self.history_idx += 1;
        if self.history_idx == self.history.len() {
            let saved = self.history_saved.take().unwrap_or_default();
            self.textarea.set_text(&saved);
        } else {
            let text = self.history[self.history_idx].clone();
            self.textarea.set_text(&text);
        }
        self.textarea.set_cursor(self.textarea.text().len());
        InputAction::Changed
    }

    // -- Completion ------------------------------------------------------------

    fn update_completion(&mut self) {
        if !self.textarea.text().starts_with('/') {
            self.completion = None;
            return;
        }
        let prefix = self.textarea.text();
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

    // -- Public API ------------------------------------------------------------

    /// Insert a block of text at the cursor position (e.g. from a paste event).
    pub fn insert_text(&mut self, text: &str) {
        self.completion = None;
        // Normalize CR to LF
        let normalized = text.replace('\r', "\n");
        self.textarea.insert_str(&normalized);
        self.ensure_cursor_visible();
    }

    /// Take the current buffer text for submission.
    pub fn take_text(&mut self) -> String {
        let text = self.textarea.text().to_string();
        if !text.trim().is_empty() {
            self.push_history(text.clone());
        }
        self.textarea.set_text("");
        self.scroll_offset = 0;
        self.history_saved = None;
        self.completion = None;
        text
    }

    pub fn in_completion(&self) -> bool {
        self.completion.is_some()
    }

    pub fn history(&self) -> &[String] {
        &self.history
    }

    pub fn completion_matches(&self) -> Vec<&str> {
        self.completion
            .iter()
            .flat_map(|comp| comp.matches.iter().map(|&i| SLASH_COMMANDS[i]))
            .collect()
    }

    pub fn completion_selected(&self) -> usize {
        self.completion.as_ref().map(|c| c.selected).unwrap_or(0)
    }

    pub fn buffer(&self) -> &str {
        self.textarea.text()
    }

    pub fn cursor(&self) -> usize {
        // Return char-index for compatibility with existing tests
        self.textarea.text()[..self.textarea.cursor()].chars().count()
    }

    pub fn display_width(&self) -> usize {
        if let Some(ref comp) = self.completion {
            let idx = comp.matches[comp.selected];
            SLASH_COMMANDS[idx].width()
        } else {
            self.textarea.text().width()
        }
    }

    pub fn display_text(&self) -> &str {
        if let Some(ref comp) = self.completion {
            SLASH_COMMANDS[comp.matches[comp.selected]]
        } else {
            self.textarea.text()
        }
    }

    pub fn display_cursor_col(&self) -> usize {
        self.cursor_position().1
    }

    /// Get display lines for multi-line rendering (viewport-windowed).
    pub fn display_lines(&self) -> Vec<&str> {
        if self.completion.is_some() {
            return vec![self.display_text()];
        }
        let all_lines: Vec<&str> = self.textarea.text().split('\n').collect();
        let end = (self.scroll_offset + MAX_INPUT_ROWS).min(all_lines.len());
        all_lines[self.scroll_offset..end].to_vec()
    }

    /// Returns (has_lines_above, has_lines_below) for scroll indicators.
    pub fn scroll_indicators(&self) -> (bool, bool) {
        let total = self.line_count();
        let above = self.scroll_offset > 0;
        let below = self.scroll_offset + MAX_INPUT_ROWS < total;
        (above, below)
    }

    // -- Internal helpers ------------------------------------------------------

    /// Current logical line and display-width column.
    fn cursor_line_col(&self) -> (usize, usize) {
        let text = self.textarea.text();
        let byte_pos = self.textarea.cursor();
        let mut row = 0;
        let mut line_start = 0;
        for (i, ch) in text.char_indices() {
            if i == byte_pos {
                let col = text[line_start..byte_pos].width();
                return (row, col);
            }
            if ch == '\n' {
                row += 1;
                line_start = i + 1;
            }
        }
        // Cursor at end of text
        let col = text[line_start..byte_pos.min(text.len())].width();
        (row, col)
    }

    fn ensure_cursor_visible(&mut self) {
        let (cursor_row, _) = self.cursor_line_col();
        if cursor_row < self.scroll_offset {
            self.scroll_offset = cursor_row;
        } else if cursor_row >= self.scroll_offset + MAX_INPUT_ROWS {
            self.scroll_offset = cursor_row + 1 - MAX_INPUT_ROWS;
        }
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

    fn alt_enter() -> KeyEvent {
        KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT)
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
        // TextArea kills backward word (skips trailing whitespace, then the word)
        assert_eq!(w.buffer(), "hello ");
    }

    #[test]
    fn test_ctrl_u_kills_to_bol() {
        let mut w = InputWidget::new();
        w.handle_key(key(KeyCode::Char('h')));
        w.handle_key(key(KeyCode::Char('i')));
        w.handle_key(ctrl('u'));
        // TextArea Ctrl+U kills from beginning of line to cursor
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
    fn test_escape_clears_input_not_quit() {
        let mut w = InputWidget::new();
        // ESC with empty buffer → no-op
        assert!(matches!(
            w.handle_key(key(KeyCode::Esc)),
            InputAction::None
        ));

        // Type something, then ESC → clears buffer
        w.handle_key(key(KeyCode::Char('h')));
        w.handle_key(key(KeyCode::Char('i')));
        assert_eq!(w.buffer(), "hi");
        assert!(matches!(
            w.handle_key(key(KeyCode::Esc)),
            InputAction::Changed
        ));
        assert_eq!(w.buffer(), "");
    }

    #[test]
    fn test_escape_dismisses_completion_first() {
        let mut w = InputWidget::new();
        // Type "/" to trigger completion popup
        w.handle_key(key(KeyCode::Char('/')));
        assert!(w.in_completion());

        // ESC should dismiss completion but keep buffer
        assert!(matches!(
            w.handle_key(key(KeyCode::Esc)),
            InputAction::Changed
        ));
        assert!(!w.in_completion());
        assert_eq!(w.buffer(), "/");

        // Second ESC clears the buffer
        assert!(matches!(
            w.handle_key(key(KeyCode::Esc)),
            InputAction::Changed
        ));
        assert_eq!(w.buffer(), "");
    }

    #[test]
    fn test_ctrl_j_inserts_newline() {
        let mut w = InputWidget::new();
        w.handle_key(key(KeyCode::Char('a')));
        // Ctrl+J: crossterm decodes byte 0x0A as Char('j') + CONTROL in raw mode
        assert!(matches!(w.handle_key(ctrl('j')), InputAction::Changed));
        w.handle_key(key(KeyCode::Char('b')));
        assert_eq!(w.buffer(), "a\nb");
        assert_eq!(w.cursor(), 3);
    }

    #[test]
    fn test_ctrl_n_moves_cursor_down() {
        // In codex-rs TextArea, Ctrl+N moves cursor down (Emacs binding)
        let mut w = InputWidget::new();
        w.insert_text("line1\nline2");
        // Move cursor to line 1
        w.handle_key(key(KeyCode::Up));
        assert_eq!(w.cursor_position().0, 0); // row 0
        // Ctrl+N moves down
        w.handle_key(ctrl('n'));
        assert_eq!(w.cursor_position().0, 1); // row 1
    }

    #[test]
    fn test_shift_enter_inserts_newline() {
        let mut w = InputWidget::new();
        w.handle_key(key(KeyCode::Char('a')));
        assert!(matches!(
            w.handle_key(shift_enter()),
            InputAction::Changed
        ));
        w.handle_key(key(KeyCode::Char('b')));
        assert_eq!(w.buffer(), "a\nb");
    }

    #[test]
    fn test_alt_enter_inserts_newline() {
        let mut w = InputWidget::new();
        w.handle_key(key(KeyCode::Char('a')));
        assert!(matches!(
            w.handle_key(alt_enter()),
            InputAction::Changed
        ));
        w.handle_key(key(KeyCode::Char('b')));
        assert_eq!(w.buffer(), "a\nb");
    }

    #[test]
    fn test_literal_newline_char_inserts_newline() {
        let mut w = InputWidget::new();
        w.handle_key(key(KeyCode::Char('a')));
        // Edge case: Char('\n') without modifiers
        assert!(matches!(
            w.handle_key(key(KeyCode::Char('\n'))),
            InputAction::Changed
        ));
        w.handle_key(key(KeyCode::Char('b')));
        assert_eq!(w.buffer(), "a\nb");
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
    fn test_tab_accepts_completion() {
        let mut w = InputWidget::new();
        // Type enough to get a single match
        for c in "/hel".chars() { w.handle_key(key(KeyCode::Char(c))); }
        assert!(w.in_completion());
        // Tab should accept the currently highlighted item
        w.handle_key(key(KeyCode::Tab));
        assert!(!w.in_completion(), "Tab should dismiss completion menu");
        assert!(w.buffer().starts_with('/'), "Tab should fill in the selected command");
    }

    #[test]
    fn test_tab_on_slash_alone_shows_menu() {
        let mut w = InputWidget::new();
        w.handle_key(key(KeyCode::Char('/')));
        assert!(w.in_completion());
        // Tab with multiple matches should accept the currently selected (first) item
        w.handle_key(key(KeyCode::Tab));
        assert!(!w.in_completion(), "Tab should accept and dismiss menu");
        assert!(w.buffer().starts_with('/'));
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
    fn test_alt_enter_newline() {
        let mut w = InputWidget::new();
        w.insert_text("line1");
        assert!(matches!(w.handle_key(alt_enter()), InputAction::Changed));
        w.insert_text("line2");
        assert_eq!(w.buffer(), "line1\nline2");
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
    fn test_unlimited_lines_with_viewport_scroll() {
        let mut w = InputWidget::new();
        // Insert more than MAX_INPUT_ROWS lines
        for _ in 0..MAX_INPUT_ROWS + 3 {
            w.handle_key(key(KeyCode::Char('x')));
            w.handle_key(shift_enter());
        }
        // Buffer has all lines (no cap)
        assert_eq!(w.line_count(), MAX_INPUT_ROWS + 4); // +3 newlines + final empty line
        // Visible rows capped at MAX_INPUT_ROWS
        assert_eq!(w.visible_rows(), MAX_INPUT_ROWS as u16);
        // Scroll indicators: cursor is at bottom, so has_above=true
        let (above, _) = w.scroll_indicators();
        assert!(above);
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

    #[test]
    fn test_insert_text_single_line() {
        let mut w = InputWidget::new();
        w.insert_text("hello world");
        assert_eq!(w.buffer(), "hello world");
        assert_eq!(w.cursor(), 11);
    }

    #[test]
    fn test_insert_text_multiline() {
        let mut w = InputWidget::new();
        w.insert_text("line1\nline2\nline3");
        assert_eq!(w.buffer(), "line1\nline2\nline3");
        assert_eq!(w.line_count(), 3);
    }

    #[test]
    fn test_insert_text_cr_treated_as_newline() {
        let mut w = InputWidget::new();
        w.insert_text("a\rb\rc");
        assert_eq!(w.buffer(), "a\nb\nc");
        assert_eq!(w.line_count(), 3);
    }

    #[test]
    fn test_insert_text_unlimited_lines() {
        let mut w = InputWidget::new();
        // Insert 7 lines — all stored, viewport scrolls
        w.insert_text("1\n2\n3\n4\n5\n6\n7");
        assert_eq!(w.line_count(), 7);
        // Only MAX_INPUT_ROWS visible
        assert_eq!(w.display_lines().len(), MAX_INPUT_ROWS);
        // Scroll indicator: lines above viewport
        let (above, _) = w.scroll_indicators();
        assert!(above);
    }

    #[test]
    fn test_insert_text_at_cursor_position() {
        let mut w = InputWidget::new();
        w.insert_text("hello");
        // Move cursor left 2 positions → cursor before "lo"
        w.handle_key(key(KeyCode::Left));
        w.handle_key(key(KeyCode::Left));
        w.insert_text("XX");
        assert_eq!(w.buffer(), "helXXlo");
    }
}
