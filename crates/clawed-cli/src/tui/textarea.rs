//! Editable text buffer with word wrapping and cursor management.
//!
//! Adapted from codex-rs `textarea.rs`. Provides:
//! - UTF-8 / grapheme-aware cursor movement
//! - Word-level editing (Ctrl+W, Alt+B/F, etc.)
//! - Kill buffer (Ctrl+K / Ctrl+Y)
//! - Cached word-wrap for visual line navigation
//! - C0 control character handling for terminals without modifier reporting

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::widgets::Widget;
use std::cell::Ref;
use std::cell::RefCell;
use std::ops::Range;
use textwrap::Options;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

const WORD_SEPARATORS: &str = "`~!@#$%^&*()-=+[{]}\\|;:'\",.<>/?";

fn is_word_separator(ch: char) -> bool {
    WORD_SEPARATORS.contains(ch)
}

fn split_word_pieces(run: &str) -> Vec<(usize, &str)> {
    let mut pieces = Vec::new();
    for (segment_start, segment) in run.split_word_bound_indices() {
        let mut piece_start = 0;
        let mut chars = segment.char_indices();
        let Some((_, first_char)) = chars.next() else {
            continue;
        };
        let mut in_separator = is_word_separator(first_char);

        for (idx, ch) in chars {
            let is_separator = is_word_separator(ch);
            if is_separator == in_separator {
                continue;
            }
            pieces.push((segment_start + piece_start, &segment[piece_start..idx]));
            piece_start = idx;
            in_separator = is_separator;
        }

        pieces.push((segment_start + piece_start, &segment[piece_start..]));
    }

    pieces
}

/// Returns byte-ranges for each wrapped line with a +1 sentinel byte at end.
/// Used by cursor position logic.
fn wrap_ranges(text: &str, opts: Options<'_>) -> Vec<Range<usize>> {
    let mut lines: Vec<Range<usize>> = Vec::new();
    let mut cursor = 0usize;
    for line in textwrap::wrap(text, &opts) {
        match line {
            std::borrow::Cow::Borrowed(slice) => {
                // Calculate byte offset of `slice` within `text` safely.
                // Both slices share the same backing allocation, so we can
                // use the pointer addresses to find the offset without unsafe.
                let start = slice.as_ptr() as usize - text.as_ptr() as usize;
                let end = start + slice.len();
                let trailing_spaces = text[end..].chars().take_while(|c| *c == ' ').count();
                let range_end = (end + trailing_spaces + 1).min(text.len());
                lines.push(start..range_end);
                cursor = end + trailing_spaces;
            }
            std::borrow::Cow::Owned(slice) => {
                let mapped = map_owned_line(text, cursor, &slice);
                let trailing_spaces =
                    text[mapped.end..].chars().take_while(|c| *c == ' ').count();
                let range_end = (mapped.end + trailing_spaces + 1).min(text.len());
                lines.push(mapped.start..range_end);
                cursor = mapped.end + trailing_spaces;
            }
        }
    }
    lines
}

/// Map an owned (materialized) wrapped line back to a byte range in `text`.
fn map_owned_line(text: &str, cursor: usize, wrapped: &str) -> Range<usize> {
    let mut start = cursor;
    while start < text.len() && !wrapped.starts_with(' ') {
        let Some(ch) = text[start..].chars().next() else {
            break;
        };
        if ch != ' ' {
            break;
        }
        start += ch.len_utf8();
    }

    let mut end = start;
    let mut chars = wrapped.chars().peekable();
    while let Some(ch) = chars.next() {
        if end < text.len() {
            let Some(src) = text[end..].chars().next() else {
                break;
            };
            if ch == src {
                end += src.len_utf8();
                continue;
            }
        }
        // Skip trailing penalty chars (e.g. hyphen from textwrap)
        if ch == '-' && chars.peek().is_none() {
            continue;
        }
        break;
    }
    start..end
}

#[derive(Debug, Clone)]
struct WrapCache {
    width: u16,
    lines: Vec<Range<usize>>,
}

/// `TextArea` is the editable buffer behind the TUI input.
///
/// Provides grapheme-aware editing, cached word-wrap, and a single-entry kill buffer.
#[derive(Debug)]
pub struct TextArea {
    text: String,
    cursor_pos: usize,
    wrap_cache: RefCell<Option<WrapCache>>,
    preferred_col: Option<usize>,
    kill_buffer: String,
}

impl TextArea {
    pub fn new() -> Self {
        Self {
            text: String::new(),
            cursor_pos: 0,
            wrap_cache: RefCell::new(None),
            preferred_col: None,
            kill_buffer: String::new(),
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    pub fn cursor(&self) -> usize {
        self.cursor_pos
    }

    pub fn set_cursor(&mut self, pos: usize) {
        self.cursor_pos = pos.clamp(0, self.text.len());
        self.preferred_col = None;
    }

    /// Replace the entire buffer text, clamping the cursor.
    pub fn set_text(&mut self, text: &str) {
        self.text = text.to_string();
        self.cursor_pos = self.cursor_pos.clamp(0, self.text.len());
        self.clamp_cursor_to_boundary();
        self.wrap_cache.replace(None);
        self.preferred_col = None;
    }

    pub fn insert_str(&mut self, text: &str) {
        self.insert_str_at(self.cursor_pos, text);
    }

    pub fn insert_str_at(&mut self, pos: usize, text: &str) {
        let pos = self.clamp_to_char_boundary(pos);
        self.text.insert_str(pos, text);
        self.wrap_cache.replace(None);
        if pos <= self.cursor_pos {
            self.cursor_pos += text.len();
        }
        self.preferred_col = None;
    }

    pub fn replace_range(&mut self, range: Range<usize>, text: &str) {
        let start = range.start.clamp(0, self.text.len());
        let end = range.end.clamp(0, self.text.len());
        if start > end {
            return;
        }
        let removed_len = end - start;
        let inserted_len = text.len();
        let diff = inserted_len as isize - removed_len as isize;

        self.text.replace_range(start..end, text);
        self.wrap_cache.replace(None);
        self.preferred_col = None;

        self.cursor_pos = if self.cursor_pos < start {
            self.cursor_pos
        } else if self.cursor_pos <= end {
            start + inserted_len
        } else {
            ((self.cursor_pos as isize) + diff) as usize
        }
        .min(self.text.len());
    }

    /// Desired visual height (in wrapped lines) for a given width.
    pub fn desired_height(&self, width: u16) -> u16 {
        self.wrapped_lines(width).len() as u16
    }

    /// Compute cursor (x, y) relative to `area`, accounting for wrapping.
    pub fn cursor_pos_in(&self, area: Rect) -> Option<(u16, u16)> {
        let lines = self.wrapped_lines(area.width);
        let i = Self::line_index_for(&lines, self.cursor_pos)?;
        let ls = &lines[i];
        let col = self.text[ls.start..self.cursor_pos].width() as u16;
        let row = i.min(area.height as usize);
        Some((area.x + col, area.y + row as u16))
    }

    // -- Key input dispatch ----------------------------------------------------

    pub fn input(&mut self, event: KeyEvent) {
        if !matches!(event.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return;
        }
        match event {
            // C0 control chars without CONTROL modifier (some terminals)
            KeyEvent { code: KeyCode::Char('\u{0002}'), modifiers: KeyModifiers::NONE, .. } => {
                self.move_cursor_left();
            }
            KeyEvent { code: KeyCode::Char('\u{0006}'), modifiers: KeyModifiers::NONE, .. } => {
                self.move_cursor_right();
            }
            KeyEvent { code: KeyCode::Char('\u{0010}'), modifiers: KeyModifiers::NONE, .. } => {
                self.move_cursor_up();
            }
            KeyEvent { code: KeyCode::Char('\u{000e}'), modifiers: KeyModifiers::NONE, .. } => {
                self.move_cursor_down();
            }

            // Plain character insertion (no CONTROL/ALT)
            KeyEvent {
                code: KeyCode::Char(c),
                modifiers: KeyModifiers::NONE | KeyModifiers::SHIFT,
                ..
            } => self.insert_str(&c.to_string()),

            // Newline: Ctrl+J, Ctrl+M, or Enter (any modifier)
            KeyEvent {
                code: KeyCode::Char('j' | 'm'),
                modifiers: KeyModifiers::CONTROL,
                ..
            }
            | KeyEvent {
                code: KeyCode::Enter,
                ..
            } => self.insert_str("\n"),

            // Ctrl+Alt+H → delete backward word
            KeyEvent {
                code: KeyCode::Char('h'),
                modifiers,
                ..
            } if modifiers == (KeyModifiers::CONTROL | KeyModifiers::ALT) => {
                self.delete_backward_word();
            }
            // Alt+Backspace → delete backward word
            KeyEvent {
                code: KeyCode::Backspace,
                modifiers: KeyModifiers::ALT,
                ..
            } => self.delete_backward_word(),
            // Backspace / Ctrl+H
            KeyEvent {
                code: KeyCode::Backspace,
                ..
            }
            | KeyEvent {
                code: KeyCode::Char('h'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => self.delete_backward(1),

            // Alt+Delete / Alt+D → delete forward word
            KeyEvent {
                code: KeyCode::Delete,
                modifiers: KeyModifiers::ALT,
                ..
            }
            | KeyEvent {
                code: KeyCode::Char('d'),
                modifiers: KeyModifiers::ALT,
                ..
            } => self.delete_forward_word(),
            // Delete / Ctrl+D
            KeyEvent {
                code: KeyCode::Delete,
                ..
            }
            | KeyEvent {
                code: KeyCode::Char('d'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => self.delete_forward(1),

            // Ctrl+W → delete backward word
            KeyEvent {
                code: KeyCode::Char('w'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => self.delete_backward_word(),

            // Alt+B → word left, Alt+F → word right
            KeyEvent {
                code: KeyCode::Char('b'),
                modifiers: KeyModifiers::ALT,
                ..
            } => self.set_cursor(self.beginning_of_previous_word()),
            KeyEvent {
                code: KeyCode::Char('f'),
                modifiers: KeyModifiers::ALT,
                ..
            } => self.set_cursor(self.end_of_next_word()),

            // Ctrl+U → kill to beginning of line
            KeyEvent {
                code: KeyCode::Char('u'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => self.kill_to_beginning_of_line(),

            // Ctrl+K → kill to end of line
            KeyEvent {
                code: KeyCode::Char('k'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => self.kill_to_end_of_line(),

            // Ctrl+Y → yank
            KeyEvent {
                code: KeyCode::Char('y'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => self.yank(),

            // Arrow keys
            KeyEvent { code: KeyCode::Left, modifiers: KeyModifiers::NONE, .. } => {
                self.move_cursor_left();
            }
            KeyEvent { code: KeyCode::Right, modifiers: KeyModifiers::NONE, .. } => {
                self.move_cursor_right();
            }
            KeyEvent { code: KeyCode::Char('b'), modifiers: KeyModifiers::CONTROL, .. } => {
                self.move_cursor_left();
            }
            KeyEvent { code: KeyCode::Char('f'), modifiers: KeyModifiers::CONTROL, .. } => {
                self.move_cursor_right();
            }
            KeyEvent { code: KeyCode::Char('p'), modifiers: KeyModifiers::CONTROL, .. } => {
                self.move_cursor_up();
            }
            KeyEvent { code: KeyCode::Char('n'), modifiers: KeyModifiers::CONTROL, .. } => {
                self.move_cursor_down();
            }

            // Alt+Arrow / Ctrl+Arrow → word movement
            KeyEvent { code: KeyCode::Left, modifiers: KeyModifiers::ALT | KeyModifiers::CONTROL, .. } => {
                self.set_cursor(self.beginning_of_previous_word());
            }
            KeyEvent { code: KeyCode::Right, modifiers: KeyModifiers::ALT | KeyModifiers::CONTROL, .. } => {
                self.set_cursor(self.end_of_next_word());
            }

            // Up / Down
            KeyEvent { code: KeyCode::Up, .. } => self.move_cursor_up(),
            KeyEvent { code: KeyCode::Down, .. } => self.move_cursor_down(),

            // Home / Ctrl+A → beginning of line
            KeyEvent { code: KeyCode::Home, .. } => {
                self.move_cursor_to_beginning_of_line(false);
            }
            KeyEvent { code: KeyCode::Char('a'), modifiers: KeyModifiers::CONTROL, .. } => {
                self.move_cursor_to_beginning_of_line(true);
            }

            // End / Ctrl+E → end of line
            KeyEvent { code: KeyCode::End, .. } => {
                self.move_cursor_to_end_of_line(false);
            }
            KeyEvent { code: KeyCode::Char('e'), modifiers: KeyModifiers::CONTROL, .. } => {
                self.move_cursor_to_end_of_line(true);
            }

            _ => {}
        }
    }

    // -- Editing operations ---------------------------------------------------

    pub fn delete_backward(&mut self, n: usize) {
        if n == 0 || self.cursor_pos == 0 {
            return;
        }
        let mut target = self.cursor_pos;
        for _ in 0..n {
            target = self.prev_grapheme(target);
            if target == 0 {
                break;
            }
        }
        self.replace_range(target..self.cursor_pos, "");
    }

    pub fn delete_forward(&mut self, n: usize) {
        if n == 0 || self.cursor_pos >= self.text.len() {
            return;
        }
        let mut target = self.cursor_pos;
        for _ in 0..n {
            target = self.next_grapheme(target);
            if target >= self.text.len() {
                break;
            }
        }
        self.replace_range(self.cursor_pos..target, "");
    }

    pub fn delete_backward_word(&mut self) {
        let start = self.beginning_of_previous_word();
        self.kill_range(start..self.cursor_pos);
    }

    pub fn delete_forward_word(&mut self) {
        let end = self.end_of_next_word();
        if end > self.cursor_pos {
            self.kill_range(self.cursor_pos..end);
        }
    }

    pub fn kill_to_end_of_line(&mut self) {
        let eol = self.end_of_current_line();
        let range = if self.cursor_pos == eol {
            if eol < self.text.len() {
                Some(self.cursor_pos..eol + 1)
            } else {
                None
            }
        } else {
            Some(self.cursor_pos..eol)
        };
        if let Some(range) = range {
            self.kill_range(range);
        }
    }

    pub fn kill_to_beginning_of_line(&mut self) {
        let bol = self.beginning_of_current_line();
        let range = if self.cursor_pos == bol {
            if bol > 0 { Some(bol - 1..bol) } else { None }
        } else {
            Some(bol..self.cursor_pos)
        };
        if let Some(range) = range {
            self.kill_range(range);
        }
    }

    pub fn yank(&mut self) {
        if self.kill_buffer.is_empty() {
            return;
        }
        let text = self.kill_buffer.clone();
        self.insert_str(&text);
    }

    fn kill_range(&mut self, range: Range<usize>) {
        if range.start >= range.end || range.start > self.text.len() {
            return;
        }
        let end = range.end.min(self.text.len());
        let removed = self.text[range.start..end].to_string();
        if removed.is_empty() {
            return;
        }
        self.kill_buffer = removed;
        self.replace_range(range.start..end, "");
    }

    // -- Cursor movement ------------------------------------------------------

    pub fn move_cursor_left(&mut self) {
        self.cursor_pos = self.prev_grapheme(self.cursor_pos);
        self.preferred_col = None;
    }

    pub fn move_cursor_right(&mut self) {
        self.cursor_pos = self.next_grapheme(self.cursor_pos);
        self.preferred_col = None;
    }

    pub fn move_cursor_up(&mut self) {
        if let Some((target_col, maybe_line)) = {
            let cache_ref = self.wrap_cache.borrow();
            if let Some(cache) = cache_ref.as_ref() {
                let lines = &cache.lines;
                if let Some(idx) = Self::line_index_for(lines, self.cursor_pos) {
                    let cur_range = &lines[idx];
                    let target_col = self
                        .preferred_col
                        .unwrap_or_else(|| self.text[cur_range.start..self.cursor_pos].width());
                    if idx > 0 {
                        let prev = &lines[idx - 1];
                        Some((target_col, Some((prev.start, prev.end.saturating_sub(1)))))
                    } else {
                        Some((target_col, None))
                    }
                } else {
                    None
                }
            } else {
                None
            }
        } {
            match maybe_line {
                Some((line_start, line_end)) => {
                    if self.preferred_col.is_none() {
                        self.preferred_col = Some(target_col);
                    }
                    self.move_to_display_col(line_start, line_end, target_col);
                    return;
                }
                None => {
                    self.cursor_pos = 0;
                    self.preferred_col = None;
                    return;
                }
            }
        }

        // Fallback to logical line navigation
        if let Some(prev_nl) = self.text[..self.cursor_pos].rfind('\n') {
            let target_col = match self.preferred_col {
                Some(c) => c,
                None => {
                    let c = self.current_display_col();
                    self.preferred_col = Some(c);
                    c
                }
            };
            let prev_line_start = self.text[..prev_nl].rfind('\n').map(|i| i + 1).unwrap_or(0);
            self.move_to_display_col(prev_line_start, prev_nl, target_col);
        } else {
            self.cursor_pos = 0;
            self.preferred_col = None;
        }
    }

    pub fn move_cursor_down(&mut self) {
        if let Some((target_col, next_line)) = {
            let cache_ref = self.wrap_cache.borrow();
            if let Some(cache) = cache_ref.as_ref() {
                let lines = &cache.lines;
                if let Some(idx) = Self::line_index_for(lines, self.cursor_pos) {
                    let cur_range = &lines[idx];
                    let target_col = self
                        .preferred_col
                        .unwrap_or_else(|| self.text[cur_range.start..self.cursor_pos].width());
                    if idx + 1 < lines.len() {
                        let next = &lines[idx + 1];
                        Some((target_col, Some((next.start, next.end.saturating_sub(1)))))
                    } else {
                        Some((target_col, None))
                    }
                } else {
                    None
                }
            } else {
                None
            }
        } {
            match next_line {
                Some((line_start, line_end)) => {
                    if self.preferred_col.is_none() {
                        self.preferred_col = Some(target_col);
                    }
                    self.move_to_display_col(line_start, line_end, target_col);
                    return;
                }
                None => {
                    self.cursor_pos = self.text.len();
                    self.preferred_col = None;
                    return;
                }
            }
        }

        // Fallback to logical line navigation
        let target_col = match self.preferred_col {
            Some(c) => c,
            None => {
                let c = self.current_display_col();
                self.preferred_col = Some(c);
                c
            }
        };
        if let Some(next_nl) = self.text[self.cursor_pos..]
            .find('\n')
            .map(|i| i + self.cursor_pos)
        {
            let next_line_start = next_nl + 1;
            let next_line_end = self.text[next_line_start..]
                .find('\n')
                .map(|i| i + next_line_start)
                .unwrap_or(self.text.len());
            self.move_to_display_col(next_line_start, next_line_end, target_col);
        } else {
            self.cursor_pos = self.text.len();
            self.preferred_col = None;
        }
    }

    /// Returns true if cursor is on the first visual line.
    pub fn is_at_first_line(&self) -> bool {
        let cache_ref = self.wrap_cache.borrow();
        if let Some(cache) = cache_ref.as_ref() {
            Self::line_index_for(&cache.lines, self.cursor_pos)
                .is_none_or(|idx| idx == 0)
        } else {
            !self.text[..self.cursor_pos].contains('\n')
        }
    }

    /// Returns true if cursor is on the last visual line.
    pub fn is_at_last_line(&self) -> bool {
        let cache_ref = self.wrap_cache.borrow();
        if let Some(cache) = cache_ref.as_ref() {
            let lines = &cache.lines;
            Self::line_index_for(lines, self.cursor_pos)
                .is_none_or(|idx| idx + 1 >= lines.len())
        } else {
            !self.text[self.cursor_pos..].contains('\n')
        }
    }

    fn move_cursor_to_beginning_of_line(&mut self, move_up_at_bol: bool) {
        let bol = self.beginning_of_current_line();
        if move_up_at_bol && self.cursor_pos == bol {
            self.set_cursor(self.beginning_of_line(self.cursor_pos.saturating_sub(1)));
        } else {
            self.set_cursor(bol);
        }
        self.preferred_col = None;
    }

    fn move_cursor_to_end_of_line(&mut self, move_down_at_eol: bool) {
        let eol = self.end_of_current_line();
        if move_down_at_eol && self.cursor_pos == eol {
            let next_pos = (self.cursor_pos.saturating_add(1)).min(self.text.len());
            self.set_cursor(self.end_of_line(next_pos));
        } else {
            self.set_cursor(eol);
        }
    }

    // -- Internal helpers -----------------------------------------------------

    fn current_display_col(&self) -> usize {
        let bol = self.beginning_of_current_line();
        self.text[bol..self.cursor_pos].width()
    }

    fn beginning_of_line(&self, pos: usize) -> usize {
        self.text[..pos].rfind('\n').map(|i| i + 1).unwrap_or(0)
    }

    fn beginning_of_current_line(&self) -> usize {
        self.beginning_of_line(self.cursor_pos)
    }

    fn end_of_line(&self, pos: usize) -> usize {
        self.text[pos..]
            .find('\n')
            .map(|i| i + pos)
            .unwrap_or(self.text.len())
    }

    fn end_of_current_line(&self) -> usize {
        self.end_of_line(self.cursor_pos)
    }

    fn line_index_for(lines: &[Range<usize>], pos: usize) -> Option<usize> {
        let idx = lines.partition_point(|r| r.start <= pos);
        if idx == 0 { None } else { Some(idx - 1) }
    }

    fn move_to_display_col(&mut self, line_start: usize, line_end: usize, target_col: usize) {
        let mut width_so_far = 0usize;
        for (i, g) in self.text[line_start..line_end].grapheme_indices(true) {
            width_so_far += g.width();
            if width_so_far > target_col {
                self.cursor_pos = line_start + i;
                return;
            }
        }
        self.cursor_pos = line_end;
    }

    fn clamp_to_char_boundary(&self, pos: usize) -> usize {
        let pos = pos.min(self.text.len());
        if self.text.is_char_boundary(pos) {
            return pos;
        }
        let mut p = pos;
        while p > 0 && !self.text.is_char_boundary(p) {
            p -= 1;
        }
        p
    }

    fn clamp_cursor_to_boundary(&mut self) {
        self.cursor_pos = self.clamp_to_char_boundary(self.cursor_pos);
    }

    fn prev_grapheme(&self, pos: usize) -> usize {
        if pos == 0 {
            return 0;
        }
        let mut gc = unicode_segmentation::GraphemeCursor::new(pos, self.text.len(), false);
        match gc.prev_boundary(&self.text, 0) {
            Ok(Some(b)) => b,
            Ok(None) => 0,
            Err(_) => self.clamp_to_char_boundary(pos.saturating_sub(1)),
        }
    }

    fn next_grapheme(&self, pos: usize) -> usize {
        if pos >= self.text.len() {
            return self.text.len();
        }
        let mut gc = unicode_segmentation::GraphemeCursor::new(pos, self.text.len(), false);
        match gc.next_boundary(&self.text, 0) {
            Ok(Some(b)) => b,
            Ok(None) => self.text.len(),
            Err(_) => self.clamp_to_char_boundary(pos.saturating_add(1)),
        }
    }

    fn beginning_of_previous_word(&self) -> usize {
        let prefix = &self.text[..self.cursor_pos];
        let Some((first_non_ws_idx, ch)) = prefix
            .char_indices()
            .rev()
            .find(|&(_, ch)| !ch.is_whitespace())
        else {
            return 0;
        };
        let run_start = prefix[..first_non_ws_idx]
            .char_indices()
            .rev()
            .find(|&(_, ch)| ch.is_whitespace())
            .map_or(0, |(idx, ch)| idx + ch.len_utf8());
        let run_end = first_non_ws_idx + ch.len_utf8();
        let pieces = split_word_pieces(&prefix[run_start..run_end]);
        let mut pieces = pieces.into_iter().rev().peekable();
        let Some((piece_start, piece)) = pieces.next() else {
            return run_start;
        };
        let mut start = run_start + piece_start;

        if piece.chars().all(is_word_separator) {
            while let Some((idx, piece)) = pieces.peek() {
                if !piece.chars().all(is_word_separator) {
                    break;
                }
                start = run_start + *idx;
                pieces.next();
            }
        }

        start
    }

    fn end_of_next_word(&self) -> usize {
        let suffix = &self.text[self.cursor_pos..];
        let Some(first_non_ws) = suffix.find(|ch: char| !ch.is_whitespace()) else {
            return self.text.len();
        };
        let run = &suffix[first_non_ws..];
        let run = &run[..run.find(char::is_whitespace).unwrap_or(run.len())];
        let mut pieces = split_word_pieces(run).into_iter().peekable();
        let Some((start, piece)) = pieces.next() else {
            return self.cursor_pos + first_non_ws;
        };
        let word_start = self.cursor_pos + first_non_ws + start;
        let mut end = word_start + piece.len();
        if piece.chars().all(is_word_separator) {
            while let Some((idx, piece)) = pieces.peek() {
                if !piece.chars().all(is_word_separator) {
                    break;
                }
                end = self.cursor_pos + first_non_ws + *idx + piece.len();
                pieces.next();
            }
        }
        end
    }

    #[expect(clippy::unwrap_used)]
    fn wrapped_lines(&self, width: u16) -> Ref<'_, Vec<Range<usize>>> {
        {
            let mut cache = self.wrap_cache.borrow_mut();
            let needs_recalc = match cache.as_ref() {
                Some(c) => c.width != width,
                None => true,
            };
            if needs_recalc {
                let lines = wrap_ranges(
                    &self.text,
                    Options::new(width as usize)
                        .wrap_algorithm(textwrap::WrapAlgorithm::FirstFit),
                );
                *cache = Some(WrapCache { width, lines });
            }
        }
        let cache = self.wrap_cache.borrow();
        Ref::map(cache, |c| &c.as_ref().unwrap().lines)
    }

    /// Number of logical (newline-separated) lines.
    pub fn line_count(&self) -> usize {
        self.text.split('\n').count().max(1)
    }

    /// Get display lines (newline-split, for rendering without wrapping).
    pub fn display_lines(&self) -> Vec<&str> {
        self.text.split('\n').collect()
    }
}

impl Widget for &TextArea {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let lines = self.wrapped_lines(area.width);
        let end = lines.len().min(area.height as usize);
        for (row, idx) in (0..end).enumerate() {
            let r = &lines[idx];
            let y = area.y + row as u16;
            let line_end = r.end.saturating_sub(1).min(self.text.len());
            let line_start = r.start.min(line_end);
            buf.set_string(area.x, y, &self.text[line_start..line_end], Style::default());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ta_with(text: &str) -> TextArea {
        let mut t = TextArea::new();
        t.insert_str(text);
        t
    }

    fn key_event(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new_with_kind(code, modifiers, KeyEventKind::Press)
    }

    #[test]
    fn insert_and_cursor() {
        let mut t = ta_with("hello");
        assert_eq!(t.text(), "hello");
        assert_eq!(t.cursor(), 5);
        t.insert_str("!");
        assert_eq!(t.text(), "hello!");
        assert_eq!(t.cursor(), 6);
    }

    #[test]
    fn delete_backward_forward() {
        let mut t = ta_with("abc");
        t.set_cursor(1);
        t.delete_backward(1);
        assert_eq!(t.text(), "bc");
        assert_eq!(t.cursor(), 0);

        t.set_cursor(0);
        t.delete_forward(1);
        assert_eq!(t.text(), "c");
        assert_eq!(t.cursor(), 0);
    }

    #[test]
    fn kill_and_yank() {
        let mut t = ta_with("hello world");
        t.set_cursor(5);
        t.kill_to_end_of_line();
        assert_eq!(t.text(), "hello");
        t.yank();
        assert_eq!(t.text(), "hello world");
    }

    #[test]
    fn ctrl_j_inserts_newline() {
        let mut t = TextArea::new();
        t.insert_str("line1");
        t.input(key_event(KeyCode::Char('j'), KeyModifiers::CONTROL));
        assert_eq!(t.text(), "line1\n");
        t.insert_str("line2");
        assert_eq!(t.text(), "line1\nline2");
    }

    #[test]
    fn ctrl_m_inserts_newline() {
        let mut t = TextArea::new();
        t.insert_str("a");
        t.input(key_event(KeyCode::Char('m'), KeyModifiers::CONTROL));
        assert_eq!(t.text(), "a\n");
    }

    #[test]
    fn enter_inserts_newline() {
        let mut t = TextArea::new();
        t.insert_str("x");
        t.input(key_event(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(t.text(), "x\n");
    }

    #[test]
    fn shift_enter_inserts_newline() {
        let mut t = TextArea::new();
        t.insert_str("x");
        t.input(key_event(KeyCode::Enter, KeyModifiers::SHIFT));
        assert_eq!(t.text(), "x\n");
    }

    #[test]
    fn word_movement() {
        let mut t = ta_with("hello world foo");
        t.set_cursor(0);
        let end = t.end_of_next_word();
        assert_eq!(end, 5); // "hello"
        t.set_cursor(end);
        let end2 = t.end_of_next_word();
        assert_eq!(end2, 11); // "world"
    }

    #[test]
    fn home_end_keys() {
        let mut t = ta_with("abc\ndef");
        t.set_cursor(5); // in "def"
        t.input(key_event(KeyCode::Home, KeyModifiers::NONE));
        assert_eq!(t.cursor(), 4); // beginning of "def"
        t.input(key_event(KeyCode::End, KeyModifiers::NONE));
        assert_eq!(t.cursor(), 7); // end of "def"
    }

    #[test]
    fn replace_range_updates_cursor() {
        let mut t = ta_with("abcd");
        t.set_cursor(4);
        t.replace_range(0..1, "AA");
        assert_eq!(t.text(), "AAbcd");
        assert_eq!(t.cursor(), 5);
    }

    #[test]
    fn set_text_clears() {
        let mut t = ta_with("hello");
        t.set_text("new");
        assert_eq!(t.text(), "new");
        assert_eq!(t.cursor(), 3); // clamped to end
    }

    #[test]
    fn first_last_line_detection() {
        let mut t = ta_with("line1\nline2\nline3");
        t.set_cursor(0);
        assert!(t.is_at_first_line());
        assert!(!t.is_at_last_line());

        t.set_cursor(t.text().len());
        assert!(!t.is_at_first_line());
        assert!(t.is_at_last_line());
    }

    #[test]
    fn chinese_text_editing() {
        let mut t = TextArea::new();
        t.insert_str("你好世界");
        assert_eq!(t.cursor(), "你好世界".len());
        t.move_cursor_left();
        assert_eq!(t.cursor(), "你好世".len());
        t.delete_backward(1);
        assert_eq!(t.text(), "你好界");
    }

    #[test]
    fn c0_control_char_movement() {
        let mut t = ta_with("abc");
        // ^B (0x02) should move left
        t.input(key_event(KeyCode::Char('\u{0002}'), KeyModifiers::NONE));
        assert_eq!(t.cursor(), 2);
        // ^F (0x06) should move right
        t.input(key_event(KeyCode::Char('\u{0006}'), KeyModifiers::NONE));
        assert_eq!(t.cursor(), 3);
    }

    #[test]
    fn desired_height() {
        let t = ta_with("short");
        assert_eq!(t.desired_height(80), 1);

        let t = ta_with("line1\nline2\nline3");
        assert_eq!(t.desired_height(80), 3);
    }
}
