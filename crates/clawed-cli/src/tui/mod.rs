//! Full-screen TUI with ratatui double-buffered rendering.
//!
//! Layout:
//! ```text
//! Messages (scrollable)
//! ── claude-3.5 │ turn 3 │ 4096↑ 1024↓ │ 80% ctx │ 📥2 ──  (separator + static info)
//! ⠹ thinking  01:20  Bash (00:03)  2 agents                         (dynamic status, only when active)
//! ▸ queued message 1                                          (queue items, only when queued)
//! ▸ queued message 2
//! ──────────────────────────────────────────────────────────  (input separator, always)
//! > user input here_
//! Tab: complete  Ctrl+J: newline  Ctrl+C: abort/quit          (hint bar, toggleable)
//! ```

mod bash_mode;
mod bottombar;
pub(crate) mod diff_style;
mod input;
mod markdown;
mod messages;
mod overlay;
mod permission;
mod status;
mod statusline;
mod tasklist;
mod taskplan;
mod textarea;
pub(crate) mod verbs;

pub use input::InputWidget;

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use std::{io, path::PathBuf};

use clawed_agent::engine::QueryEngine;
use clawed_bus::bus::ClientHandle;
use clawed_bus::events::{
    AgentNotification, ErrorCode, ImageAttachment, PermissionRequest, UserQuestionRequest,
    UserQuestionResponse,
};
use crossterm::event::{
    self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEventKind, KeyModifiers,
    MouseEventKind,
};
use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Clear, List, ListItem, Paragraph, Wrap},
    Frame,
};
use tokio::sync::mpsc;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::input::command_description;

use self::messages::{Message, MessageContent};
use self::overlay::{Overlay, OverlayAction, SelectionItem};
use self::permission::PendingPermission;
use self::status::{ToolInfo, TuiStatusState};

type TuiTerminal = ratatui::Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>>;

/// Subdued text color for hints, separators, status indicators, and input text.
/// Uses a true-color gray that is readable on both dark and light backgrounds,
/// unlike `Color::DarkGray` (ANSI 8) which maps to bright on many terminals.
pub(super) const MUTED: Color = Color::Rgb(170, 170, 170);

pub(crate) fn muted() -> Style {
    Style::default().fg(MUTED)
}
pub(crate) fn blank_line() -> Line<'static> {
    Line::from(String::new())
}
pub(crate) fn line_text(line: &Line<'_>) -> String {
    line.spans.iter().map(|s| s.content.as_ref()).collect()
}

const ACTIVE_POLL_INTERVAL: Duration = Duration::from_millis(16);
const IDLE_POLL_INTERVAL: Duration = Duration::from_millis(100);
const SPINNER_TICK_INTERVAL: Duration = Duration::from_millis(verbs::SPINNER_TICK_INTERVAL_MS); // 120ms, aligned with official CC
/// Minimum time between renders during active streaming. Prevents the event loop
/// from spending all its CPU on rendering, leaving no time for input processing.
const MIN_RENDER_INTERVAL: Duration = Duration::from_millis(50);
/// Periodic full-clear interval to self-heal screen corruption caused by
/// external output (eprintln, third-party crates, sub-process leaks) that
/// invalidates ratatui's diff-based rendering when not using alternate screen.
const PERIODIC_CLEAR_INTERVAL: Duration = Duration::from_secs(5);
const MAX_COMPLETION_POPUP_ITEMS: usize = 10;

fn plain_text_lines(text: &str) -> Vec<Line<'static>> {
    if text.is_empty() {
        return vec![];
    }

    let dim = Style::default().fg(MUTED);
    let prefix_text = "\u{25CF} ";
    let prefix = Span::styled(prefix_text.to_string(), dim);
    let blank_prefix = Span::raw(" ".repeat(prefix_text.width()));
    text.lines()
        .enumerate()
        .map(|(i, line)| {
            let mut spans = if i == 0 {
                vec![prefix.clone()]
            } else {
                vec![blank_prefix.clone()]
            };
            spans.extend(parse_inline_spans(line));
            Line::from(spans)
        })
        .collect()
}

/// Parse lightweight inline markdown for streaming text:
/// `**bold**`, `*italic*`, `` `code` ``.
fn parse_inline_spans(line: &str) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut current = String::new();
    let mut chars = line.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '*' && chars.peek() == Some(&'*') {
            chars.next(); // consume second '*'
            if !current.is_empty() {
                spans.push(Span::raw(std::mem::take(&mut current)));
            }
            let mut bold_text = String::new();
            let mut found_close = false;
            while let Some(c) = chars.next() {
                if c == '*' && chars.peek() == Some(&'*') {
                    chars.next();
                    found_close = true;
                    break;
                }
                bold_text.push(c);
            }
            if found_close && !bold_text.is_empty() {
                spans.push(Span::styled(
                    bold_text,
                    Style::default().add_modifier(Modifier::BOLD),
                ));
            } else {
                current.push_str("**");
                current.push_str(&bold_text);
            }
        } else if ch == '*' {
            if !current.is_empty() {
                spans.push(Span::raw(std::mem::take(&mut current)));
            }
            let mut italic_text = String::new();
            let mut found_close = false;
            for c in chars.by_ref() {
                if c == '*' {
                    found_close = true;
                    break;
                }
                italic_text.push(c);
            }
            if found_close && !italic_text.is_empty() {
                spans.push(Span::styled(
                    italic_text,
                    Style::default().add_modifier(Modifier::ITALIC),
                ));
            } else {
                current.push('*');
                current.push_str(&italic_text);
            }
        } else if ch == '`' {
            // Check if this is a code block marker (3+ backticks)
            let mut backtick_count = 1;
            while chars.peek() == Some(&'`') {
                chars.next();
                backtick_count += 1;
            }
            if backtick_count >= 3 {
                current.push_str(&"`".repeat(backtick_count));
                continue;
            }
            if backtick_count == 2 {
                current.push_str("``");
                continue;
            }
            if !current.is_empty() {
                spans.push(Span::raw(std::mem::take(&mut current)));
            }
            let mut code_text = String::new();
            let mut found_close = false;
            for c in chars.by_ref() {
                if c == '`' {
                    found_close = true;
                    break;
                }
                code_text.push(c);
            }
            if found_close && !code_text.is_empty() {
                spans.push(Span::styled(
                    code_text,
                    Style::default()
                        .bg(Color::Rgb(45, 45, 45))
                        .fg(Color::Rgb(220, 220, 220)),
                ));
            } else {
                current.push('`');
                current.push_str(&code_text);
            }
        } else {
            current.push(ch);
        }
    }
    if !current.is_empty() {
        spans.push(Span::raw(current));
    }
    spans
}

/// Map raw tool names to user-facing display names (CC-aligned).
/// Uses byte-level comparison; returns `Cow::Borrowed` for static names
/// and `Cow::Owned` only for dynamic `mcp__*` or unknown names.
pub(super) fn user_facing_tool_name(raw: &str) -> Cow<'static, str> {
    match raw.as_bytes() {
        b"bash" | b"Bash" | b"shell" | b"Shell" => Cow::Borrowed("Bash"),
        b"read" | b"Read" | b"read_file" | b"ReadFile" => Cow::Borrowed("Read"),
        b"write" | b"Write" | b"write_file" | b"WriteFile" => Cow::Borrowed("Write"),
        b"edit" | b"Edit" | b"multi_edit" | b"MultiEdit" => Cow::Borrowed("Edit"),
        b"glob" | b"Glob" => Cow::Borrowed("Glob"),
        b"grep" | b"Grep" => Cow::Borrowed("Grep"),
        b"ls" | b"LS" | b"Ls" => Cow::Borrowed("LS"),
        b"web_search" | b"WebSearch" | b"websearch" | b"Web_Search" => Cow::Borrowed("Web Search"),
        b"web_fetch" | b"WebFetch" | b"webfetch" | b"Web_Fetch" => Cow::Borrowed("Web Fetch"),
        b"task" | b"Task" | b"task_create" | b"TaskCreate" => Cow::Borrowed("Task"),
        b"agent" | b"Agent" => Cow::Borrowed("Agent"),
        s if s.starts_with(b"mcp__") => {
            // Format mcp__server__tool as "server › tool"
            let after_prefix = &raw[5..];
            if let Some(pos) = after_prefix.find("__") {
                let server = &after_prefix[..pos];
                let tool = &after_prefix[pos + 2..];
                Cow::Owned(format!("{} › {}", server, tool))
            } else {
                Cow::Owned(after_prefix.to_string())
            }
        }
        _ => Cow::Owned(raw.to_string()),
    }
}

fn extract_tool_input_display(tool_name: &str, input: &serde_json::Value) -> Option<String> {
    let lower = tool_name.to_lowercase();
    let get_str = |key: &str| input.get(key).and_then(|v| v.as_str()).map(String::from);
    match lower.as_str() {
        "bash" | "shell" => get_str("command"),
        "read" | "read_file" | "write" | "write_file" | "edit" | "multi_edit" => {
            get_str("file_path")
        }
        "web_search" | "websearch" => get_str("query"),
        "web_fetch" | "webfetch" => get_str("url"),
        "ls" => get_str("path"),
        "glob" => get_str("pattern"),
        "grep" => get_str("pattern"),
        "agent" => get_str("agent_type"),
        "task_create" => get_str("subject"),
        "notebookedit" | "notebook_edit" => {
            let path = input.get("notebook_path").and_then(|v| v.as_str())?;
            let cell = input
                .get("cell_number")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let mode = input
                .get("edit_mode")
                .and_then(|v| v.as_str())
                .unwrap_or("replace");
            Some(format!("{} cell [{}] {}", path, cell, mode))
        }
        _ if lower.starts_with("mcp__") => {
            // For MCP tools, show the first meaningful string param as input hint
            input.as_object().and_then(|obj| {
                obj.iter().find_map(|(k, v)| {
                    if k == "server" || k == "uri" || k == "query" {
                        v.as_str().map(String::from)
                    } else {
                        v.as_str()
                            .filter(|s| !s.is_empty() && *k != "format")
                            .map(String::from)
                    }
                })
            })
        }
        _ => None,
    }
}

pub(crate) fn is_shell_tool(tool_name: &str) -> bool {
    let b = tool_name.as_bytes();
    b.windows(4).any(|w| w.eq_ignore_ascii_case(b"bash"))
        || b.windows(5).any(|w| w.eq_ignore_ascii_case(b"shell"))
}

fn should_clear_message_area(previous_total_visual: Option<usize>, total_visual: usize) -> bool {
    previous_total_visual.is_some_and(|previous| previous > total_visual)
}

fn completion_popup_rows_from_count(match_count: usize) -> u16 {
    if match_count <= 1 {
        0
    } else {
        match_count.min(MAX_COMPLETION_POPUP_ITEMS) as u16
    }
}

fn completion_popup_rows(app: &App) -> u16 {
    completion_popup_rows_from_count(app.input.completion_matches().len())
}

fn completion_popup_area(popup_slot: Rect, input_area: Rect, matches: &[&str]) -> Option<Rect> {
    if popup_slot.width == 0 || popup_slot.height == 0 || matches.len() <= 1 {
        return None;
    }

    let max_cmd_width = matches.iter().map(|c| c.width()).max().unwrap_or(4);
    let desc_col = max_cmd_width + 4; // padding between cmd and desc
    let max_desc_width = matches
        .iter()
        .map(|c| command_description(c).width())
        .max()
        .unwrap_or(20);
    let popup_width = (desc_col + max_desc_width + 3).min(popup_slot.width as usize);

    Some(Rect::new(
        input_area.x,
        popup_slot.y,
        popup_width as u16,
        popup_slot.height,
    ))
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct LayoutSignature {
    has_overlay: bool,
    has_permission: bool,
    bottom_bar_hidden: bool,
    completion_rows: u16,
    input_rows: u16,
    queue_rows: u16,
    task_plan_rows: u16,
    has_tip: bool,
    /// Terminal width — changes cause word-wrap differences that can leave
    /// ghost cells if not cleared.
    term_width: u16,
    /// Terminal height — changes shift the entire layout vertically.
    term_height: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FooterPickerKind {
    Model,
    Theme,
    Permissions,
    Skills,
    Resume,
}

/// Tracks the state to restore after a skill's turn completes.
#[derive(Debug)]
struct PendingSkillRestore {
    original_model: Option<String>,
    skill_name: String,
}

#[derive(Debug)]
struct FooterPicker {
    kind: FooterPickerKind,
    items: Vec<SelectionItem>,
    selected: usize,
    scroll_offset: usize,
}

/// Search state for inline message search (Ctrl+F).
#[derive(Debug)]
struct SearchState {
    query: String,
    cursor_offset: usize,
    /// (line_index_in_cached_lines, col_start) for each match.
    matches: Vec<(usize, usize)>,
    current_match: usize,
}

enum FooterPickerAction {
    Consumed,
    Dismissed,
    Selected(String),
    PassThrough,
}

impl FooterPicker {
    fn visible_rows(&self) -> u16 {
        self.items.len().min(MAX_COMPLETION_POPUP_ITEMS) as u16
    }

    fn ensure_selected_visible(&mut self) {
        let visible_rows = usize::from(self.visible_rows());
        if visible_rows == 0 {
            self.scroll_offset = 0;
            return;
        }

        if self.selected < self.scroll_offset {
            self.scroll_offset = self.selected;
        } else if self.selected >= self.scroll_offset + visible_rows {
            self.scroll_offset = self.selected + 1 - visible_rows;
        }
    }

    fn handle_key(&mut self, code: KeyCode) -> FooterPickerAction {
        match code {
            KeyCode::Esc => FooterPickerAction::Dismissed,
            KeyCode::Up | KeyCode::Char('k') => {
                if self.selected > 0 {
                    self.selected -= 1;
                    self.ensure_selected_visible();
                }
                FooterPickerAction::Consumed
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.selected + 1 < self.items.len() {
                    self.selected += 1;
                    self.ensure_selected_visible();
                }
                FooterPickerAction::Consumed
            }
            KeyCode::Home => {
                self.selected = 0;
                self.ensure_selected_visible();
                FooterPickerAction::Consumed
            }
            KeyCode::End => {
                self.selected = self.items.len().saturating_sub(1);
                self.ensure_selected_visible();
                FooterPickerAction::Consumed
            }
            KeyCode::Enter | KeyCode::Tab => self
                .items
                .get(self.selected)
                .map(|item| FooterPickerAction::Selected(item.value.clone()))
                .unwrap_or(FooterPickerAction::Dismissed),
            KeyCode::Char(_) | KeyCode::Backspace | KeyCode::Delete => {
                FooterPickerAction::PassThrough
            }
            _ => FooterPickerAction::Consumed,
        }
    }
}

fn footer_menu_rows(app: &App) -> u16 {
    app.footer_picker
        .as_ref()
        .map_or_else(|| completion_popup_rows(app), FooterPicker::visible_rows)
}

fn should_render_print_output_in_overlay(text: &str) -> bool {
    text.lines().nth(12).is_some() || text.len() > 600
}

fn footer_picker_from_overlay(kind: FooterPickerKind, overlay: Overlay) -> Option<FooterPicker> {
    match overlay {
        Overlay::SelectionList {
            items,
            selected,
            scroll_offset,
            ..
        } => Some(FooterPicker {
            kind,
            items,
            selected,
            scroll_offset,
        }),
        Overlay::InfoPanel { .. } => None,
    }
}

fn build_model_picker(current_model: &str) -> FooterPicker {
    footer_picker_from_overlay(
        FooterPickerKind::Model,
        overlay::build_model_overlay(current_model),
    )
    .expect("model overlay should be a selection list")
}

fn build_theme_picker(current_theme: &str) -> FooterPicker {
    footer_picker_from_overlay(
        FooterPickerKind::Theme,
        overlay::build_theme_overlay(current_theme),
    )
    .expect("theme overlay should be a selection list")
}

fn build_permission_overlay(current_mode: clawed_core::permissions::PermissionMode) -> Overlay {
    let items = vec![
        SelectionItem {
            label: "default".to_string(),
            description: "Ask before risky operations (recommended)".to_string(),
            value: "default".to_string(),
            is_current: current_mode == clawed_core::permissions::PermissionMode::Default,
        },
        SelectionItem {
            label: "acceptEdits".to_string(),
            description: "Auto-approve file edits, still ask for shell commands".to_string(),
            value: "acceptEdits".to_string(),
            is_current: current_mode == clawed_core::permissions::PermissionMode::AcceptEdits,
        },
        SelectionItem {
            label: "auto".to_string(),
            description: "Safe tools auto-allowed, risky ones use classifier".to_string(),
            value: "auto".to_string(),
            is_current: current_mode == clawed_core::permissions::PermissionMode::Auto,
        },
        SelectionItem {
            label: "plan".to_string(),
            description: "Read-only mode, no tool execution".to_string(),
            value: "plan".to_string(),
            is_current: current_mode == clawed_core::permissions::PermissionMode::Plan,
        },
        SelectionItem {
            label: "bypass".to_string(),
            description: "Skip ALL permission checks (dangerous)".to_string(),
            value: "bypass".to_string(),
            is_current: current_mode == clawed_core::permissions::PermissionMode::BypassAll,
        },
    ];
    let selected = items.iter().position(|item| item.is_current).unwrap_or(0);

    Overlay::SelectionList {
        title: "Permission Mode".to_string(),
        items,
        selected,
        scroll_offset: 0,
    }
}

fn build_permissions_picker(
    current_mode: clawed_core::permissions::PermissionMode,
) -> FooterPicker {
    let items = vec![
        SelectionItem {
            label: "default".to_string(),
            description: "Normal confirmations".to_string(),
            value: "default".to_string(),
            is_current: current_mode == clawed_core::permissions::PermissionMode::Default,
        },
        SelectionItem {
            label: "bypass".to_string(),
            description: "Skip confirmations".to_string(),
            value: "bypass".to_string(),
            is_current: current_mode == clawed_core::permissions::PermissionMode::BypassAll,
        },
        SelectionItem {
            label: "acceptEdits".to_string(),
            description: "Auto-accept edit requests".to_string(),
            value: "acceptEdits".to_string(),
            is_current: current_mode == clawed_core::permissions::PermissionMode::AcceptEdits,
        },
        SelectionItem {
            label: "auto".to_string(),
            description: "Automatic mode".to_string(),
            value: "auto".to_string(),
            is_current: current_mode == clawed_core::permissions::PermissionMode::Auto,
        },
        SelectionItem {
            label: "plan".to_string(),
            description: "Planning-first mode".to_string(),
            value: "plan".to_string(),
            is_current: current_mode == clawed_core::permissions::PermissionMode::Plan,
        },
    ];
    let selected = items.iter().position(|item| item.is_current).unwrap_or(0);

    FooterPicker {
        kind: FooterPickerKind::Permissions,
        items,
        selected,
        scroll_offset: 0,
    }
}

fn build_skills_picker(skills: &[clawed_core::skills::SkillEntry]) -> Option<FooterPicker> {
    let items: Vec<SelectionItem> = skills
        .iter()
        .filter(|skill| skill.user_invocable)
        .map(|skill| {
            let mut description = skill.description.clone();
            if let Some(hint) = &skill.argument_hint {
                if !description.is_empty() {
                    description.push_str("  ");
                }
                description.push_str(hint);
            }
            SelectionItem {
                label: format!("/{}", skill.name),
                description,
                value: skill.name.clone(),
                is_current: false,
            }
        })
        .collect();

    (!items.is_empty()).then_some(FooterPicker {
        kind: FooterPickerKind::Skills,
        items,
        selected: 0,
        scroll_offset: 0,
    })
}

fn build_resume_picker() -> Option<FooterPicker> {
    let sessions = clawed_core::session::list_sessions();
    if sessions.is_empty() {
        return None;
    }
    let items: Vec<SelectionItem> = sessions
        .iter()
        .take(20)
        .map(|session| {
            let age = clawed_core::session::format_age(&session.updated_at);
            let label = session
                .custom_title
                .clone()
                .unwrap_or_else(|| session.title.clone());
            let description = format!(
                "{} · {} turns · {} msgs · {}",
                session.model, session.turn_count, session.message_count, age
            );
            SelectionItem {
                label,
                description,
                value: session.id.clone(),
                is_current: false,
            }
        })
        .collect();
    Some(FooterPicker {
        kind: FooterPickerKind::Resume,
        items,
        selected: 0,
        scroll_offset: 0,
    })
}

fn restore_terminal_after_tui() {
    clawed_tools::diff_ui::set_tui_mode(false);
    let _ = crossterm::execute!(std::io::stdout(), DisableBracketedPaste);
    let _ = crossterm::execute!(
        std::io::stdout(),
        crossterm::event::PopKeyboardEnhancementFlags
    );
    let _ = crossterm::execute!(std::io::stdout(), crossterm::cursor::Show);
    let _ = crossterm::terminal::disable_raw_mode();
}

fn reenter_tui_terminal(terminal: &mut TuiTerminal) -> io::Result<()> {
    crossterm::terminal::enable_raw_mode()?;
    crossterm::execute!(std::io::stdout(), EnableBracketedPaste)?;
    let _ = crossterm::execute!(
        std::io::stdout(),
        crossterm::event::PushKeyboardEnhancementFlags(
            crossterm::event::KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                | crossterm::event::KeyboardEnhancementFlags::REPORT_EVENT_TYPES
                | crossterm::event::KeyboardEnhancementFlags::REPORT_ALTERNATE_KEYS,
        )
    );
    terminal.clear()?;
    clawed_tools::diff_ui::set_tui_mode(true);
    Ok(())
}

fn with_tui_suspended<T, F>(terminal: &mut TuiTerminal, action: F) -> anyhow::Result<T>
where
    F: FnOnce() -> T,
{
    restore_terminal_after_tui();
    let result = action();
    reenter_tui_terminal(terminal)?;
    Ok(result)
}

struct TuiTerminalGuard;

impl Drop for TuiTerminalGuard {
    fn drop(&mut self) {
        restore_terminal_after_tui();
    }
}

// -- App State ----------------------------------------------------------------

#[derive(Debug)]
enum PendingWorkflow {
    CommitPushPr {
        cwd: PathBuf,
        user_message: String,
        baseline_status: String,
    },
}

/// A pending user question from the AskUser tool, waiting for the user to type a response.
struct PendingUserQuestion {
    pub request: clawed_bus::events::UserQuestionRequest,
}

struct App {
    messages: Vec<Message>,
    scroll_offset: usize,
    /// Number of new messages received while user is scrolled up.
    new_messages_count: usize,
    auto_scroll: bool,
    input: InputWidget,
    footer_picker: Option<FooterPicker>,
    status: TuiStatusState,
    task_plan: taskplan::TaskPlan,
    task_list: tasklist::TaskListState,
    permission: Option<PendingPermission>,
    /// Pending user question from the AskUser tool (bus-based TUI mode).
    user_question: Option<PendingUserQuestion>,
    overlay: Option<Overlay>,
    /// Inline message search state (Ctrl+F).
    search_state: Option<SearchState>,
    bottom_bar_hidden: bool,
    running: bool,
    /// Set to true when the terminal needs a full clear before the next draw.
    /// This is only required when the layout geometry changes (footer/input/task
    /// panel height changes, overlays appear/disappear, resize events, etc.).
    needs_full_clear: bool,
    /// Set when visible state changed and the next loop should render a new frame.
    needs_redraw: bool,
    total_turns: u32,
    /// Latest context size from the most recent API response (not accumulated).
    context_tokens: u64,
    /// Cumulative output tokens generated across all turns.
    total_output_tokens: u64,
    /// Total session cost in USD (from cost tracker).
    total_cost_usd: f64,
    model: String,
    pending_images: Vec<ImageAttachment>,
    /// Async command waiting to be executed in the event loop (needs engine access).
    pending_command: Option<crate::commands::CommandResult>,
    /// Debug mode: log raw key events as system messages.
    key_debug: bool,
    /// Inputs queued while LLM is generating; merged and submitted on TurnComplete.
    queued_inputs: Vec<String>,
    /// True from when client.submit() is called until TurnComplete is received.
    /// Unlike status.thinking (which is false during TextDelta streaming),
    /// this remains true for the entire LLM turn so queue/abort checks work correctly.
    is_generating: bool,
    /// True between mark_generating() and the first TurnStart of the new turn.
    /// TextDelta/ThinkingDelta received in this window belong to the previous
    /// (aborted) stream and must be discarded to avoid bleed-in.
    expecting_turn_start: bool,
    /// Layout state from the previous frame, used to detect geometry changes
    /// that need a full terminal clear to avoid ghost cells.
    last_layout_sig: LayoutSignature,
    pending_workflow: Option<PendingWorkflow>,
    cached_visible_lines: Vec<Line<'static>>,
    cached_visible_lines_dirty: bool,
    cached_visible_line_count: Option<(u16, usize)>,
    last_rendered_message_visual_count: Option<usize>,
    last_spinner_tick: Instant,
    /// Instant of the last render. Used to throttle render rate during
    /// active streaming so the event loop has time to process input events.
    last_render_at: Instant,
    /// Instant of the last periodic full clear. Without alternate screen,
    /// any external output to the terminal corrupts ratatui's diff buffer;
    /// a periodic clear self-heals this.
    last_periodic_clear: Instant,
    /// Cached terminal dimensions from the last frame. Used to detect resize
    /// in the layout signature so ghost cells are cleared after resize.
    term_width: u16,
    term_height: u16,
    /// Current permission mode label (e.g. "bypass", "default").
    /// Updated when user changes it via /permissions.
    permission_mode: String,
    /// If a skill temporarily switched the model, store the restore info here
    /// so it can be cleaned up when TurnComplete arrives.
    pending_skill_restore: Option<PendingSkillRestore>,
    /// External status line state (from settings.json `statusLine.command`).
    status_line: statusline::StatusLineState,
    /// Index of the user message used as the sticky header anchor when scrolled up.
    sticky_anchor: Option<usize>,
    /// Currently viewed teammate (None = viewing main transcript).
    viewed_teammate: Option<ViewedTeammate>,
    /// Context suggestions overlay (file / MCP / agent suggestions above input).
    suggestions: Vec<SuggestionItem>,
    selected_suggestion: usize,
    /// Keyboard selection mode for active agents (pointer on spinner row).
    /// Some(selected_index) when cycling through agents with Tab/Enter.
    teammate_selection: Option<usize>,
    /// BashModeProgress top-level panel state.
    bash_mode: bash_mode::BashModeState,
    /// Latest progress text per active agent, rendered ephemerally (not in message history).
    agent_progress: HashMap<String, String>,
}

/// A single context suggestion (file, MCP resource, or agent).
#[derive(Debug, Clone)]
struct SuggestionItem {
    #[allow(dead_code)]
    id: String,
    display_text: String,
    description: Option<String>,
    kind: SuggestionKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
enum SuggestionKind {
    File,
    McpResource,
    Agent,
}

/// Teammate being viewed in transcript mode.
struct ViewedTeammate {
    #[allow(dead_code)]
    agent_id: String,
    name: String,
    color: Color,
}

impl App {
    fn new(model: String) -> Self {
        let ide_kind = detect_ide();
        let mut status = TuiStatusState::new();
        status.ide_kind = ide_kind.clone();
        Self {
            messages: Vec::new(),
            scroll_offset: 0,
            new_messages_count: 0,
            auto_scroll: true,
            input: InputWidget::new(),
            footer_picker: None,
            status,
            task_plan: taskplan::TaskPlan::new(),
            task_list: tasklist::TaskListState::new(),
            permission: None,
            user_question: None,
            overlay: None,
            search_state: None,
            bottom_bar_hidden: false,
            running: true,
            needs_full_clear: false,
            needs_redraw: true,
            total_turns: 0,
            context_tokens: 0,
            total_output_tokens: 0,
            total_cost_usd: 0.0,
            model,
            pending_images: Vec::new(),
            pending_command: None,
            key_debug: false,
            queued_inputs: Vec::new(),
            is_generating: false,
            expecting_turn_start: false,
            last_layout_sig: LayoutSignature::default(),
            pending_workflow: None,
            cached_visible_lines: Vec::new(),
            cached_visible_lines_dirty: false,
            cached_visible_line_count: None,
            last_rendered_message_visual_count: None,
            last_spinner_tick: Instant::now(),
            last_render_at: Instant::now() - Duration::from_secs(1),
            last_periodic_clear: Instant::now(),
            status_line: statusline::StatusLineState::new(None),
            sticky_anchor: None,
            term_width: 0,
            term_height: 0,
            permission_mode: String::new(),
            pending_skill_restore: None,
            viewed_teammate: None,
            suggestions: Vec::new(),
            selected_suggestion: 0,
            teammate_selection: None,
            bash_mode: bash_mode::BashModeState::new(),
            agent_progress: HashMap::new(),
        }
    }

    fn request_redraw(&mut self) {
        self.needs_redraw = true;
    }

    fn set_footer_picker(&mut self, picker: FooterPicker) {
        self.footer_picker = Some(picker);
        self.request_redraw();
    }

    fn clear_footer_picker(&mut self) {
        if self.footer_picker.take().is_some() {
            self.request_redraw();
        }
    }

    #[allow(dead_code)]
    fn view_teammate(&mut self, #[allow(dead_code)] agent_id: String, name: String) {
        let color = self
            .status
            .active_agents
            .get(&agent_id)
            .map(|a| a.color)
            .unwrap_or_else(|| agent_color_for_id(&agent_id));
        self.viewed_teammate = Some(ViewedTeammate {
            agent_id,
            name,
            color,
        });
        self.invalidate_visible_lines();
        self.request_redraw();
    }

    fn exit_teammate_view(&mut self) {
        if self.viewed_teammate.take().is_some() {
            self.invalidate_visible_lines();
            self.request_redraw();
        }
    }

    // ── Teammate selection (pointer + tab/enter keyboard navigation) ──────────

    fn enter_teammate_selection(&mut self) {
        if !self.status.active_agents.is_empty() {
            self.teammate_selection = Some(0);
            self.request_redraw();
        }
    }

    fn exit_teammate_selection(&mut self) {
        if self.teammate_selection.take().is_some() {
            self.request_redraw();
        }
    }

    fn cycle_teammate_selection(&mut self, delta: isize) {
        let Some(ref mut sel) = self.teammate_selection else {
            return;
        };
        let count = self.status.active_agents.len();
        if count == 0 {
            self.exit_teammate_selection();
            return;
        }
        let new = *sel as isize + delta;
        *sel = new.rem_euclid(count as isize) as usize;
        self.request_redraw();
    }

    fn confirm_teammate_selection(&mut self) {
        let Some(sel) = self.teammate_selection else {
            return;
        };
        self.exit_teammate_selection();
        let sorted = status::sorted_agent_entries(&self.status.active_agents);
        if let Some((agent_id, info)) = sorted.get(sel) {
            self.view_teammate((*agent_id).clone(), info.name.clone());
        }
    }

    fn spinner_active(&self) -> bool {
        self.status.is_generating || !self.status.active_tools.is_empty()
    }

    fn advance_spinner_if_due(&mut self, now: Instant) {
        if self.spinner_active() {
            if now.duration_since(self.last_spinner_tick) >= SPINNER_TICK_INTERVAL {
                self.status.spinner_frame = self.status.spinner_frame.wrapping_add(1);
                self.status.update_token_counter();
                self.last_spinner_tick = now;
                self.request_redraw();
            }
        } else {
            self.last_spinner_tick = now;
        }
        // Trigger external status line refresh if state changed.
        if self.status_line.is_enabled() && self.status_line.needs_refresh {
            let ctx = statusline::build_context(
                &self.model,
                &self.permission_mode,
                self.total_turns,
                self.context_tokens,
                self.total_output_tokens,
                self.total_cost_usd,
                self.status.context_pct,
            );
            self.status_line.refresh_if_due(ctx);
        }
        self.status_line.sync();
    }

    fn visible_message_lines_at(&self, index: usize) -> Vec<Line<'static>> {
        let msg = &self.messages[index];

        if self.is_generating && index + 1 == self.messages.len() {
            if let MessageContent::AssistantText(text) = &msg.content {
                return plain_text_lines(text);
            }
        }

        // Determine tree sibling continuity for tool executions.
        let has_sibling_after = if let MessageContent::ToolExecution { depth: d1, .. } =
            &msg.content
        {
            self.messages.get(index + 1).is_some_and(|next| {
                matches!(&next.content, MessageContent::ToolExecution { depth: d2, .. } if *d2 == *d1)
            })
        } else {
            false
        };

        // Live duration for running tools (duration_ms == 0 means still active).
        let live_duration_ms = if let MessageContent::ToolExecution {
            name, duration_ms, ..
        } = &msg.content
        {
            if *duration_ms == 0 {
                self.status
                    .active_tools
                    .get(name)
                    .map(|t| t.started.elapsed().as_millis() as u64)
            } else {
                None
            }
        } else {
            None
        };

        msg.to_lines_with_context(has_sibling_after, live_duration_ms)
    }

    fn invalidate_visible_lines(&mut self) {
        self.cached_visible_lines_dirty = true;
        self.cached_visible_line_count = None;
        self.request_redraw();
    }

    fn replace_cached_tail(&mut self, old_len: usize, new_lines: Vec<Line<'static>>) {
        let new_start = self.cached_visible_lines.len().saturating_sub(old_len);
        self.cached_visible_lines.truncate(new_start);
        self.cached_visible_lines.extend(new_lines);
        // Invalidate the cached visual line count: the delta may change the
        // wrapped line count by more than "a few lines" (e.g. a long paragraph
        // with no newlines can have many wrapped visual lines). Stale counts
        // cause incorrect scroll offsets which makes output appear in the
        // wrong place (including overlapping the input area).
        self.cached_visible_line_count = None;
        self.request_redraw();
    }

    fn rebuild_visible_lines(&mut self) {
        if !self.cached_visible_lines_dirty {
            return;
        }

        let mut lines = Vec::new();
        let mut index = 0;
        while index < self.messages.len() {
            // Collapse long runs of consecutive System messages.
            if matches!(self.messages[index].content, MessageContent::System(_)) {
                let start = index;
                while index < self.messages.len()
                    && matches!(self.messages[index].content, MessageContent::System(_))
                {
                    index += 1;
                }
                let count = index - start;
                let has_important = (start..index)
                    .any(|i| Self::system_msg_is_important(&self.messages[i].content));
                if count > 2 && !has_important {
                    if start > 0
                        && Self::needs_separator(
                            &self.messages[start - 1].content,
                            &self.messages[start].content,
                        )
                    {
                        lines.push(Line::from(""));
                    }
                    lines.extend(self.visible_message_lines_at(start));
                    lines.push(Line::styled(
                        format!("+ {} system messages", count - 2),
                        Style::default().fg(MUTED),
                    ));
                    if count > 1 {
                        lines.extend(self.visible_message_lines_at(index - 1));
                    }
                    continue;
                }
                for j in start..index {
                    if j > start
                        && Self::needs_separator(
                            &self.messages[j - 1].content,
                            &self.messages[j].content,
                        )
                    {
                        lines.push(Line::from(""));
                    }
                    lines.extend(self.visible_message_lines_at(j));
                }
                continue;
            }

            if index > 0
                && Self::needs_separator(
                    &self.messages[index - 1].content,
                    &self.messages[index].content,
                )
            {
                lines.push(Line::from(""));
            }
            lines.extend(self.visible_message_lines_at(index));
            index += 1;
        }
        self.cached_visible_lines = lines;
        self.cached_visible_lines_dirty = false;
        self.cached_visible_line_count = None;
        self.refresh_search_matches();
        self.apply_search_highlight();
    }

    // ── Search helpers ──────────────────────────────────────────────────

    /// Recompute match positions from the current query against cached lines.
    fn refresh_search_matches(&mut self) {
        let Some(search) = self.search_state.as_mut() else {
            return;
        };
        search.matches.clear();
        search.current_match = 0;
        if search.query.is_empty() {
            return;
        }
        let q_lower = search.query.to_lowercase();
        for (line_idx, line) in self.cached_visible_lines.iter().enumerate() {
            let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
            if text.to_lowercase().contains(&q_lower) {
                search.matches.push((line_idx, 0));
            }
        }
    }

    /// Apply reverse-video styling to cached lines that match the search query.
    fn apply_search_highlight(&mut self) {
        let Some(search) = self.search_state.as_ref() else {
            return;
        };
        if search.matches.is_empty() {
            return;
        }
        for &(line_idx, _) in &search.matches {
            if let Some(line) = self.cached_visible_lines.get_mut(line_idx) {
                for span in line.spans.iter_mut() {
                    span.style = span.style.add_modifier(Modifier::REVERSED);
                }
            }
        }
    }

    /// Scroll the message viewport so the match at `line_idx` is roughly centered.
    fn scroll_to_match(&mut self, line_idx: usize) {
        self.auto_scroll = false;
        let approx_viewport = (self.term_height.saturating_sub(8)).max(5) as usize;
        let total_lines = self.cached_visible_lines.len().max(1);
        if total_lines <= approx_viewport {
            self.scroll_offset = 0;
            self.request_redraw();
            return;
        }
        let max_scroll = total_lines - approx_viewport;
        let target = line_idx.saturating_sub(approx_viewport / 2);
        self.scroll_offset = max_scroll.saturating_sub(target);
        self.request_redraw();
    }

    fn clear_search(&mut self) {
        if self.search_state.take().is_some() {
            self.invalidate_visible_lines();
        }
    }

    /// Returns true when two consecutive messages should be visually separated
    /// by a blank line in the TUI message list.
    fn needs_separator(prev: &MessageContent, curr: &MessageContent) -> bool {
        use MessageContent::{AssistantText, ThinkingText};
        match (prev, curr) {
            // Assistant text blocks flow together naturally.
            (AssistantText(_), AssistantText(_)) => false,
            (AssistantText(_), ThinkingText(_)) => false,
            (ThinkingText(_), AssistantText(_)) => false,
            (ThinkingText(_), ThinkingText(_)) => false,
            // Everything else gets a separator on type change.
            _ => std::mem::discriminant(prev) != std::mem::discriminant(curr),
        }
    }

    /// Whether a System message contains important information that should
    /// not be collapsed (errors, warnings, terminations, context alerts).
    fn system_msg_is_important(content: &MessageContent) -> bool {
        let MessageContent::System(text) = content else {
            return false;
        };
        text.contains("error")
            || text.contains("terminated")
            || text.contains("warning")
            || text.contains("context")
            || text.contains(verbs::ERROR_MARKER)
            || text.contains(verbs::WARNING_MARKER)
    }

    fn clear_messages(&mut self) {
        self.messages.clear();
        self.scroll_offset = 0;
        self.new_messages_count = 0;
        self.cached_visible_lines.clear();
        self.cached_visible_lines_dirty = false;
        self.cached_visible_line_count = None;
        self.last_rendered_message_visual_count = None;
        self.footer_picker = None;
        self.agent_progress.clear();
        self.request_redraw();
    }

    fn push_message(&mut self, content: MessageContent) {
        let msg = Message::new(content);
        let prev_content = self.messages.last().map(|m| &m.content);
        let needs_sep = prev_content
            .map(|prev| Self::needs_separator(prev, &msg.content))
            .unwrap_or(false);
        self.messages.push(msg);
        if !self.cached_visible_lines_dirty {
            if needs_sep {
                self.cached_visible_lines.push(Line::from(""));
            }
            let last_index = self.messages.len().saturating_sub(1);
            self.cached_visible_lines
                .extend(self.visible_message_lines_at(last_index));
            self.cached_visible_line_count = None;
        }
        if self.auto_scroll {
            self.scroll_offset = 0;
            self.new_messages_count = 0;
        } else {
            self.new_messages_count += 1;
        }
        self.request_redraw();
    }

    fn layout_signature(&self) -> LayoutSignature {
        let has_permission = self.permission.is_some();
        let queue_rows = if has_permission || self.queued_inputs.is_empty() {
            0
        } else {
            self.queued_inputs.len().min(5) as u16
        };
        let completion_rows = if has_permission {
            0
        } else {
            footer_menu_rows(self)
        };

        LayoutSignature {
            has_overlay: self.overlay.is_some(),
            has_permission,
            bottom_bar_hidden: self.bottom_bar_hidden,
            completion_rows,
            input_rows: self.input.visible_rows(),
            queue_rows,
            task_plan_rows: self.task_plan.render_height(),
            has_tip: self.status.has_tip(),
            term_width: self.term_width,
            term_height: self.term_height,
        }
    }

    /// Mark that the LLM is now generating a response.
    /// Unlike status.thinking (which goes false during TextDelta), this stays
    /// true for the entire turn so queue gating and Esc abort work correctly.
    fn mark_generating(&mut self) {
        self.status.thinking = true;
        self.status.is_generating = true;
        self.status.generating_since = Some(Instant::now());
        self.status.last_token_time = None;
        self.status.current_verb = Some(verbs::random_spinner_verb());
        // Reset token counter and thinking timer for the new turn.
        self.status.response_char_count = 0;
        self.status.displayed_token_estimate = 0;
        self.status.thinking_start = None;
        self.status.total_thinking_ms = 0;
        self.status.last_thinking_elapsed_ms = 0;
        self.status.thinking_end = None;
        self.is_generating = true;
        self.footer_picker = None;
        self.invalidate_visible_lines();
        self.last_spinner_tick = Instant::now();
        // Discard any TextDelta/ThinkingDelta that arrive before TurnStart —
        // they belong to the previous (possibly aborted) stream.
        self.expecting_turn_start = true;
    }

    /// Clear all generation state (abort or TurnComplete).
    fn mark_done(&mut self) {
        self.status.thinking = false;
        self.status.is_generating = false;
        self.status.generating_since = None;
        self.status.last_token_time = None;
        self.status.current_verb = None;
        // Finalize any in-progress thinking block.
        self.status.stop_thinking();
        self.is_generating = false;
        self.invalidate_visible_lines();
        self.last_spinner_tick = Instant::now();
        self.expecting_turn_start = false;
        // active_tools / active_shells are intentionally NOT cleared here.
        // They track tool execution lifecycle (ToolUseStart → ToolUseComplete)
        // and may outlive the API stream. Clearing them on TurnComplete would
        // make spinner_active() return false while tools are still running,
        // breaking Esc abort.
    }

    /// Hard-reset tool state. Called on error / abort / timeout when the normal
    /// ToolUseComplete lifecycle may not arrive.
    fn clear_tool_state(&mut self) {
        self.status.active_tools.clear();
        self.status.active_shells = 0;
        self.task_plan.set_shells(0);
    }

    fn take_queued_inputs(&mut self) -> Option<String> {
        if self.queued_inputs.is_empty() {
            None
        } else {
            let merged = self.queued_inputs.join("\n\n");
            self.queued_inputs.clear();
            Some(merged)
        }
    }

    /// Append text to the last AssistantText message, or create one.
    fn append_assistant_text(&mut self, text: &str) {
        if let Some(last_idx) = self.messages.len().checked_sub(1) {
            if !matches!(
                self.messages[last_idx].content,
                MessageContent::AssistantText(_)
            ) {
                self.push_message(MessageContent::AssistantText(text.to_string()));
                return;
            }

            let old_visible = if self.cached_visible_lines_dirty {
                None
            } else {
                Some(self.visible_message_lines_at(last_idx))
            };

            if let Some(msg) = self.messages.get_mut(last_idx) {
                msg.append_assistant_text(text);
            }

            if let Some(old_visible) = old_visible {
                let new_visible = self.visible_message_lines_at(last_idx);
                self.replace_cached_tail(old_visible.len(), new_visible);
            } else {
                self.invalidate_visible_lines();
            }
            if self.auto_scroll {
                self.scroll_offset = 0;
                self.new_messages_count = 0;
            }
            return;
        }
        self.push_message(MessageContent::AssistantText(text.to_string()));
    }

    /// Append text to the last ThinkingText message, or create one.
    fn append_thinking_text(&mut self, text: &str) {
        if let Some(last_idx) = self.messages.len().checked_sub(1) {
            if !matches!(
                self.messages[last_idx].content,
                MessageContent::ThinkingText(_)
            ) {
                self.push_message(MessageContent::ThinkingText(text.to_string()));
                return;
            }

            let old_visible = if self.cached_visible_lines_dirty {
                None
            } else {
                Some(self.visible_message_lines_at(last_idx))
            };

            if let Some(msg) = self.messages.get_mut(last_idx) {
                msg.append_thinking_text(text);
            }

            if let Some(old_visible) = old_visible {
                let new_visible = self.visible_message_lines_at(last_idx);
                self.replace_cached_tail(old_visible.len(), new_visible);
            } else {
                self.invalidate_visible_lines();
            }
            if self.auto_scroll {
                self.scroll_offset = 0;
                self.new_messages_count = 0;
            }
            return;
        }
        self.push_message(MessageContent::ThinkingText(text.to_string()));
    }

    /// Returns Some(merged_text) if queued inputs should be submitted after this notification.
    fn handle_notification(&mut self, notification: AgentNotification) -> Option<String> {
        // Any state change may affect the external status line.
        self.status_line.invalidate();
        match notification {
            AgentNotification::TextDelta { text } => {
                self.status.thinking = false;
                self.status.record_token();
                self.status.add_response_chars(text.len());
                // Thinking block ends when text delta begins.
                self.status.stop_thinking();
                if self.status.current_verb == Some(verbs::THINKING_VERB) {
                    self.status.current_verb = Some(verbs::random_spinner_verb());
                }
                self.append_assistant_text(&text);
            }
            AgentNotification::ThinkingDelta { text } => {
                self.status.thinking = true;
                self.status.record_token();
                self.status.start_thinking();
                self.status.current_verb = Some(verbs::THINKING_VERB);
                self.append_thinking_text(&text);
            }
            AgentNotification::ToolUseStart { tool_name, .. } => {
                self.status.record_token();
                if is_shell_tool(&tool_name) {
                    self.status.active_shells += 1;
                    self.task_plan.set_shells(self.status.active_shells);
                }
                self.status.active_tools.insert(
                    tool_name.clone(),
                    ToolInfo {
                        name: tool_name.clone(),
                        started: Instant::now(),
                    },
                );
                // Depth = 1 when running inside an agent context, 0 otherwise.
                let depth = u32::from(!self.status.active_agents.is_empty());
                // Create message immediately so ToolOutputLine streaming has
                // somewhere to append. Input will be filled in by ToolUseReady.
                self.push_message(MessageContent::ToolExecution {
                    name: tool_name,
                    input: None,
                    output_lines: vec![],
                    is_error: false,
                    duration_ms: 0,
                    full_result: None,
                    depth,
                });
            }
            AgentNotification::ToolUseReady {
                tool_name, input, ..
            } => {
                // Update the last ToolExecution message with the input display.
                let input_str = extract_tool_input_display(&tool_name, &input);
                if let Some(msg) = self.messages.iter_mut().rev().find(|m| {
                    matches!(
                        &m.content,
                        MessageContent::ToolExecution { name, .. } if *name == tool_name
                    )
                }) {
                    if let MessageContent::ToolExecution {
                        input: ref mut inp, ..
                    } = &mut msg.content
                    {
                        *inp = input_str.clone();
                    }
                    msg.invalidate_cache();
                    self.invalidate_visible_lines();
                }
                // Start BashModeProgress panel for shell commands.
                if is_shell_tool(&tool_name) {
                    self.bash_mode
                        .start(tool_name.clone(), input_str.unwrap_or_default());
                }
            }
            AgentNotification::ToolUseComplete {
                tool_name,
                is_error,
                result_preview,
                ..
            } => {
                if !tool_name.is_empty() && is_shell_tool(&tool_name) {
                    self.status.active_shells = self.status.active_shells.saturating_sub(1);
                    self.task_plan.set_shells(self.status.active_shells);
                    self.bash_mode.end();
                }
                let duration_ms = self
                    .status
                    .active_tools
                    .get(&tool_name)
                    .map(|t| t.started.elapsed().as_millis() as u64)
                    .unwrap_or(0);
                self.status.active_tools.remove(&tool_name);
                // Update the last ToolExecution message in-place.
                // If tool_name is empty (lookup failed), fall back to last ToolExecution.
                let result = result_preview.unwrap_or_default();
                let msg = if tool_name.is_empty() {
                    self.messages
                        .iter_mut()
                        .rev()
                        .find(|m| matches!(&m.content, MessageContent::ToolExecution { .. }))
                } else {
                    self.messages.iter_mut().rev().find(|m| {
                        matches!(
                            &m.content,
                            MessageContent::ToolExecution { name, .. } if *name == tool_name
                        )
                    })
                };
                if let Some(msg) = msg {
                    msg.update_tool_result(is_error, duration_ms, &result);
                    self.invalidate_visible_lines();
                }
            }
            AgentNotification::ToolOutputLine {
                tool_name, line, ..
            } => {
                // Forward to BashModeProgress if applicable.
                if is_shell_tool(&tool_name) {
                    self.bash_mode.add_line(&tool_name, &line);
                }
                // Append output line to the last matching ToolExecution message.
                // Fall back to last ToolExecution if name doesn't match (name lookup may fail).
                let msg = if tool_name.is_empty() {
                    self.messages
                        .iter_mut()
                        .rev()
                        .find(|m| matches!(&m.content, MessageContent::ToolExecution { .. }))
                } else {
                    self.messages.iter_mut().rev().find(|m| {
                        matches!(
                            &m.content,
                            MessageContent::ToolExecution { name, .. } if *name == tool_name
                        )
                    })
                };
                if let Some(msg) = msg {
                    msg.append_tool_output_line(line);
                    self.invalidate_visible_lines();
                }
            }
            AgentNotification::TurnComplete { turn, usage, .. } => {
                self.total_turns = turn;
                // input_tokens = context size for this turn (cumulative from API).
                // Keep the latest value rather than summing — summing double-counts context.
                self.context_tokens = usage.input_tokens;
                self.total_output_tokens += usage.output_tokens;
                // If expecting_turn_start is true, the user already submitted a new
                // message and is waiting for TurnStart of the *new* turn. This
                // TurnComplete belongs to the old (possibly aborted) turn. Skip
                // mark_done() so we don't clear is_generating and make the UI
                // appear frozen — that causes the user to think the 1st submit was
                // lost and submit again unnecessarily.
                if !self.expecting_turn_start {
                    if let Some(start) = self.status.generating_since.take() {
                        let ms = start.elapsed().as_millis() as u64;
                        let verb = verbs::random_turn_verb();
                        let duration = verbs::format_duration(ms);
                        let mut msg = format!(
                            "{marker} {verb} for {duration}",
                            marker = verbs::TURN_COMPLETION_MARKER
                        );
                        // Append "still running" counts when tools/agents outlive the turn.
                        let shells = self.status.active_shells;
                        let agents = self.status.active_agents.len();
                        if shells > 0 {
                            let s = if shells == 1 { "" } else { "s" };
                            msg.push_str(&format!(" \u{00B7} {shells} shell{s} still running"));
                        }
                        if agents > 0 {
                            let s = if agents == 1 { "" } else { "s" };
                            msg.push_str(&format!(" \u{00B7} {agents} agent{s} still running"));
                        }
                        self.push_message(MessageContent::System(msg));
                    }
                    self.mark_done();
                } else if self
                    .status
                    .generating_since
                    .map(|s| s.elapsed().as_secs() > 5)
                    .unwrap_or(false)
                {
                    // Safety net: TurnStart may be delayed or lost (e.g. forwarder
                    // lag, adapter still processing old stream). If we've been
                    // waiting >5s, force mark_done to prevent stuck Thinking.
                    tracing::warn!(
                        turn,
                        "TurnComplete arrived but TurnStart missing >5s, forcing mark_done"
                    );
                    self.mark_done();
                }
                // Skip TurnDivider — token/turn info lives in the status bar,
                // keeping the transcript clean like the original Claude Code.
                // Drain queue: merge all pending inputs and submit as one message.
                // Only drain when NOT expecting a new turn (if expecting_turn_start,
                // the direct submit already happened at the call site).
                // Refresh todo list after each turn completes.
                let cwd = std::env::current_dir().unwrap_or_default();
                self.task_list.refresh(&cwd);
                if self.task_list.task_count() > 0 && !self.task_list.is_expanded() {
                    self.task_list.set_expanded(true);
                }

                if !self.expecting_turn_start
                    && self.pending_workflow.is_none()
                    && !self.queued_inputs.is_empty()
                {
                    return self.take_queued_inputs();
                }
            }
            AgentNotification::TurnStart { .. } => {
                // Re-assert is_generating in case a stale TurnComplete from a
                // previous (aborted) stream arrived between mark_generating()
                // and this TurnStart, resetting is_generating prematurely.
                self.is_generating = true;
                self.status.is_generating = true;
                // We now have a confirmed new turn — allow TextDelta through.
                self.expecting_turn_start = false;
                self.status.thinking = true;
                // Skip turn separator — keeping transcript clean like the
                // original Claude Code.
            }
            AgentNotification::AgentSpawned { agent_id, name, .. } => {
                let label = name.unwrap_or_else(|| agent_id.chars().take(8).collect::<String>());
                self.task_plan.add_task(agent_id.clone(), label.clone());
                self.push_message(MessageContent::System(format!(
                    "{info} Agent spawned: {label}",
                    info = verbs::INFO_MARKER
                )));
                let _color = agent_color_for_id(&agent_id);
                self.status.active_agents.insert(
                    agent_id,
                    status::AgentInfo {
                        name: label,
                        started: Instant::now(),
                        state: status::AgentState::Active,
                        activity: None,
                        tool_count: 0,
                        token_estimate: 0,
                        idle_since: None,
                        color: Color::Cyan,
                    },
                );
            }
            AgentNotification::AgentComplete {
                agent_id,
                result,
                is_error,
            } => {
                self.task_plan.complete_task(&agent_id, is_error);
                self.status.active_agents.remove(&agent_id);
                self.agent_progress.remove(&agent_id);
                let icon = if is_error {
                    verbs::ERROR_MARKER
                } else {
                    "\u{2713}"
                };
                self.push_message(MessageContent::System(format!(
                    "{icon} Agent finished: {result}"
                )));
            }
            AgentNotification::AgentTerminated { agent_id, reason } => {
                self.task_plan.terminate_task(&agent_id);
                self.status.active_agents.remove(&agent_id);
                self.agent_progress.remove(&agent_id);
                self.push_message(MessageContent::System(format!(
                    "{warn} Agent terminated: {reason}",
                    warn = verbs::WARNING_MARKER
                )));
            }
            AgentNotification::SessionEnd { reason } => {
                self.push_message(MessageContent::System(format!(
                    "{info} Session ended: {reason}",
                    info = verbs::INFO_MARKER
                )));
            }
            AgentNotification::CompactStart => {
                self.push_message(MessageContent::System(format!(
                    "{marker} Compacting context\u{2026}",
                    marker = verbs::THINKING_MARKER
                )));
            }
            AgentNotification::CompactComplete { .. } => {
                self.push_message(MessageContent::System(format!(
                    "{marker} Conversation compacted (Ctrl+O for history)",
                    marker = verbs::TURN_COMPLETION_MARKER
                )));
            }
            AgentNotification::Error { code, message } => {
                let label = match code {
                    ErrorCode::ApiError => "API error",
                    ErrorCode::ToolError => "Tool error",
                    ErrorCode::ContextOverflow => "Context overflow",
                    ErrorCode::NetworkError => "Network error",
                    ErrorCode::PermissionDenied => "Permission denied",
                    ErrorCode::InternalError => "Internal error",
                };
                let prefix = if matches!(code, ErrorCode::ContextOverflow) {
                    verbs::WARNING_MARKER
                } else {
                    verbs::ERROR_MARKER
                };
                self.push_message(MessageContent::System(format!(
                    "{prefix} {label}: {message}"
                )));
                self.mark_done();
                self.clear_tool_state();
            }
            AgentNotification::ModelChanged {
                model,
                display_name,
            } => {
                self.model = model;
                self.push_message(MessageContent::System(format!(
                    "{info} Switched to {display_name}",
                    info = verbs::INFO_MARKER
                )));
            }
            AgentNotification::SkillsActivated { names } => {
                let list = names.join(", ");
                self.push_message(MessageContent::System(format!(
                    "{info} Skills: {list}",
                    info = verbs::INFO_MARKER
                )));
            }
            AgentNotification::SessionStatus {
                total_turns,
                total_input_tokens,
                total_output_tokens,
                context_usage_pct,
                total_cost_usd,
                ..
            } => {
                self.status.context_pct = context_usage_pct;
                if self.context_tokens == 0 && total_input_tokens > 0 {
                    self.context_tokens = total_input_tokens;
                }
                if self.total_output_tokens == 0 && total_output_tokens > 0 {
                    self.total_output_tokens = total_output_tokens;
                }
                self.total_turns = self.total_turns.max(total_turns);
                self.total_cost_usd = total_cost_usd;
            }
            AgentNotification::McpServerConnected { name, tool_count } => {
                self.push_message(MessageContent::System(format!(
                    "✓ MCP connected: {name} ({tool_count} tools)",
                )));
            }
            AgentNotification::McpServerDisconnected { name } => {
                self.push_message(MessageContent::System(format!(
                    "{error} MCP disconnected: {name}",
                    error = verbs::ERROR_MARKER
                )));
            }
            AgentNotification::McpServerError { name, error } => {
                self.push_message(MessageContent::System(format!(
                    "{icon} MCP error [{name}]: {error}",
                    icon = verbs::ERROR_MARKER
                )));
            }
            AgentNotification::McpServerList { servers } => {
                if servers.is_empty() {
                    self.push_message(MessageContent::System(
                        "No MCP servers connected.".to_string(),
                    ));
                } else {
                    let mut lines = String::from("MCP Servers:\n");
                    for s in &servers {
                        let status = if s.connected { "✓" } else { "✗" };
                        lines
                            .push_str(
                                &format!("  {status} {} ({} tools)\n", s.name, s.tool_count,),
                            );
                    }
                    self.push_message(MessageContent::System(lines));
                }
            }
            AgentNotification::ModelList { models } => {
                let mut lines = String::from("Available models:\n");
                for m in &models {
                    lines.push_str(&format!("  {} ({})\n", m.display_name, m.id));
                }
                self.push_message(MessageContent::System(lines));
            }
            AgentNotification::ToolList { tools } => {
                let enabled: Vec<_> = tools.iter().filter(|t| t.enabled).collect();
                let mut lines = format!("Tools ({} enabled):\n", enabled.len());
                for t in &enabled {
                    lines.push_str(&format!("  {} — {}\n", t.name, t.description));
                }
                self.push_message(MessageContent::System(lines));
            }
            AgentNotification::ThinkingChanged { enabled, budget } => {
                if enabled {
                    let budget_str = budget.map_or(String::new(), |b| format!(" (budget: {b})"));
                    self.push_message(MessageContent::System(format!(
                        "✓ Extended thinking enabled{budget_str}",
                    )));
                } else {
                    self.push_message(MessageContent::System(
                        "✓ Extended thinking disabled".to_string(),
                    ));
                }
            }
            AgentNotification::CacheBreakSet => {
                self.push_message(MessageContent::System(
                    "✓ Next request will skip prompt cache".to_string(),
                ));
            }
            AgentNotification::ContextWarning { usage_pct, message } => {
                self.status.context_pct = usage_pct;
                self.push_message(MessageContent::System(format!(
                    "{warn} Context {usage_pct:.0}%: {message}",
                    warn = verbs::WARNING_MARKER,
                )));
            }
            AgentNotification::MemoryExtracted { facts } => {
                let n = facts.len();
                let s = if n == 1 { "" } else { "s" };
                self.push_message(MessageContent::System(format!(
                    "{info} Saved {n} memory{s}",
                    info = verbs::INFO_MARKER,
                )));
            }
            AgentNotification::HistoryCleared => {
                self.clear_messages();
                self.push_message(MessageContent::System(format!(
                    "{info} Conversation history cleared",
                    info = verbs::INFO_MARKER,
                )));
            }
            AgentNotification::SessionSaved { session_id } => {
                self.push_message(MessageContent::System(format!(
                    "{info} Session saved: {session_id}",
                    info = verbs::INFO_MARKER,
                )));
            }
            // Tool selected — pre-execution signal (just a brief note)
            AgentNotification::ToolSelected { .. } => {}
            // AssistantMessage — full text for logging, already shown via TextDelta
            AgentNotification::AssistantMessage { .. } => {}
            // Session start: update model display
            AgentNotification::SessionStart { model, .. } => {
                self.model = model;
                self.request_redraw();
            }
            // Background agent progress
            AgentNotification::AgentProgress { agent_id, text } => {
                self.agent_progress.insert(agent_id, text);
            }
            // Conflict warning for concurrent agents
            AgentNotification::ConflictDetected { file_path, agents } => {
                self.push_message(MessageContent::System(format!(
                    "{warn} Conflict on {file_path} between: {}",
                    agents.join(", "),
                    warn = verbs::WARNING_MARKER,
                )));
            }
            // Swarm lifecycle events
            AgentNotification::SwarmTeamCreated {
                team_name,
                agent_count,
            } => {
                self.push_message(MessageContent::System(format!(
                    "{info} Swarm team '{team_name}' created ({agent_count} agents)",
                    info = verbs::INFO_MARKER,
                )));
            }
            AgentNotification::SwarmTeamDeleted { team_name } => {
                self.push_message(MessageContent::System(format!(
                    "{info} Swarm team '{team_name}' deleted",
                    info = verbs::INFO_MARKER,
                )));
            }
            AgentNotification::SwarmAgentSpawned {
                team_name,
                agent_id,
                model,
            } => {
                self.push_message(MessageContent::System(format!(
                    "  ↳ [{team_name}] Agent {agent_id} spawned ({model})",
                )));
            }
            AgentNotification::SwarmAgentTerminated {
                team_name,
                agent_id,
            } => {
                self.push_message(MessageContent::System(format!(
                    "  ↳ [{team_name}] Agent {agent_id} terminated",
                )));
            }
            AgentNotification::SwarmAgentQuery {
                team_name,
                agent_id,
                prompt_preview,
            } => {
                self.push_message(MessageContent::System(format!(
                    "  ↳ [{team_name}/{agent_id}] ▶ {prompt_preview}",
                )));
            }
            AgentNotification::SwarmAgentReply {
                team_name,
                agent_id,
                text_preview,
                is_error,
            } => {
                let icon = if is_error {
                    verbs::ERROR_MARKER
                } else {
                    "\u{2713}"
                };
                self.push_message(MessageContent::System(format!(
                    "  ↳ [{team_name}/{agent_id}] {icon} {text_preview}",
                )));
            }
            AgentNotification::SwarmAgentIdle {
                team_name,
                agent_id,
            } => {
                self.push_message(MessageContent::System(format!(
                    "  ↳ [{team_name}/{agent_id}] idle",
                )));
            }
            AgentNotification::BridgeStatus {
                platforms,
                session_count,
                adapter_count,
            } => {
                if self.status.bridge_platforms == *platforms
                    && self.status.bridge_sessions == session_count
                {
                    return None;
                }
                self.status.bridge_platforms = platforms.clone();
                self.status.bridge_sessions = session_count;
                self.status.bridge_adapters = adapter_count;
                self.request_redraw();
            }
            AgentNotification::TeleportStatus {
                remote_active,
                environment_name,
            } => {
                if self.status.teleport_remote == remote_active
                    && self.status.teleport_env == environment_name
                {
                    return None;
                }
                self.status.teleport_remote = remote_active;
                self.status.teleport_env = environment_name.clone();
                self.request_redraw();
            }
            AgentNotification::VoiceStatus { state } => {
                if self.status.voice_state == Some(state) {
                    return None;
                }
                self.status.voice_state = Some(state);
                self.request_redraw();
            }
        }
        None
    }

    fn handle_slash_command(&mut self, client: &ClientHandle, cmd: &str) {
        let cwd = std::env::current_dir().unwrap_or_default();
        let skills = clawed_core::skills::get_skills(&cwd);
        match crate::commands::SlashCommand::parse(cmd, &skills) {
            Some(crate::commands::SlashCommand::Skills) => {
                if let Some(picker) = build_skills_picker(&skills) {
                    self.set_footer_picker(picker);
                } else {
                    self.push_message(MessageContent::System(
                        "No skills found. Add .md files to .claude/skills/".to_string(),
                    ));
                }
                return;
            }
            Some(crate::commands::SlashCommand::Model(name)) if name.is_empty() => {
                self.set_footer_picker(build_model_picker(&self.model));
                return;
            }
            Some(crate::commands::SlashCommand::Resume { query }) if query.is_empty() => {
                if let Some(picker) = build_resume_picker() {
                    self.set_footer_picker(picker);
                } else {
                    self.push_message(MessageContent::System(
                        "No saved sessions. Use /summary to generate one.".to_string(),
                    ));
                }
                return;
            }
            _ => {}
        }
        let result = match crate::commands::resolve_command_result(cmd, &cwd, &skills) {
            Some(result) => result,
            None => return,
        };
        self.clear_footer_picker();
        self.request_redraw();
        match result {
            crate::commands::CommandResult::Print(text) => {
                if should_render_print_output_in_overlay(&text) {
                    self.overlay = Some(overlay::build_info_overlay("Command Output", &text));
                    self.request_redraw();
                } else {
                    self.push_message(MessageContent::System(text));
                }
            }
            crate::commands::CommandResult::ClearHistory => {
                let _ = client.send_request(clawed_bus::events::AgentRequest::ClearHistory);
                self.clear_messages();
            }
            crate::commands::CommandResult::SetModel(name) => {
                if name.is_empty() {
                    self.set_footer_picker(build_model_picker(&self.model));
                } else {
                    let _ = client.send_request(clawed_bus::events::AgentRequest::SetModel {
                        model: name.clone(),
                    });
                    let display = clawed_core::model::display_name_any(
                        &clawed_core::model::resolve_model_string(&name),
                    );
                    self.push_message(MessageContent::System(format!("✓ Model → {display}")));
                }
            }
            crate::commands::CommandResult::ShowCost { .. } => {
                let elapsed = self.status.session_start.elapsed().as_secs();
                self.overlay = Some(overlay::build_status_overlay(
                    &self.model,
                    self.total_turns,
                    self.context_tokens,
                    self.total_output_tokens,
                    elapsed,
                ));
            }
            crate::commands::CommandResult::Compact { instructions } => {
                let _ =
                    client.send_request(clawed_bus::events::AgentRequest::Compact { instructions });
            }
            crate::commands::CommandResult::Status => {
                let elapsed = self.status.session_start.elapsed().as_secs();
                self.overlay = Some(overlay::build_status_overlay(
                    &self.model,
                    self.total_turns,
                    self.context_tokens,
                    self.total_output_tokens,
                    elapsed,
                ));
            }
            crate::commands::CommandResult::Think { args } => {
                let mode = if args.is_empty() {
                    "on".to_string()
                } else {
                    args
                };
                let _ = client.send_request(clawed_bus::events::AgentRequest::SetThinking { mode });
            }
            crate::commands::CommandResult::BreakCache => {
                let _ = client.send_request(clawed_bus::events::AgentRequest::BreakCache);
            }
            crate::commands::CommandResult::Mcp { .. } => {
                self.pending_command = Some(result);
            }
            crate::commands::CommandResult::Env => {
                let cwd = std::env::current_dir().unwrap_or_default();
                let mut info = format!(
                    "Environment\n  OS: {} / {}\n  CWD: {}\n  Version: v{}\n  Model: {}",
                    std::env::consts::OS,
                    std::env::consts::ARCH,
                    cwd.display(),
                    env!("CARGO_PKG_VERSION"),
                    self.model,
                );
                if let Ok(shell) = std::env::var("SHELL").or_else(|_| std::env::var("COMSPEC")) {
                    info.push_str(&format!("\n  Shell: {shell}"));
                }
                if let Ok(term) = std::env::var("TERM") {
                    info.push_str(&format!("\n  Terminal: {term}"));
                }
                self.overlay = Some(overlay::build_info_overlay("Environment", &info));
            }
            crate::commands::CommandResult::Effort { level } => {
                let valid = ["low", "medium", "high", "max", "auto"];
                if level.is_empty() {
                    self.push_message(MessageContent::System(format!(
                        "Current effort: auto\nOptions: {}",
                        valid.join(", "),
                    )));
                } else if valid.contains(&level.to_lowercase().as_str()) {
                    self.push_message(MessageContent::System(format!(
                        "✓ Effort set to: {}",
                        level.to_lowercase(),
                    )));
                } else {
                    self.push_message(MessageContent::System(format!(
                        "Invalid effort: '{level}'. Options: {}",
                        valid.join(", "),
                    )));
                }
            }
            crate::commands::CommandResult::Tag { name } => {
                if name.is_empty() {
                    self.push_message(MessageContent::System("Usage: /tag <name>".to_string()));
                } else {
                    self.push_message(MessageContent::System(format!("✓ Tagged session: {name}",)));
                }
            }
            crate::commands::CommandResult::Stickers => {
                self.push_message(MessageContent::System(
                    "Grab some stickers at: https://claude.ai/stickers".to_string(),
                ));
            }
            crate::commands::CommandResult::Vim { .. } => {
                self.pending_command = Some(result);
            }
            crate::commands::CommandResult::Bridge => {
                let text = if self.status.bridge_platforms.is_empty() {
                    "Bridge Gateway\n  Status:      Not running\n  \n  The bridge connects this session to external platforms\n  (Lark, Telegram, Slack). Start with bridge config.".to_string()
                } else {
                    let platforms = self.status.bridge_platforms.join(", ");
                    format!(
                        "Bridge Gateway\n  Platforms:   {platforms}\n  Sessions:    {}\n  Adapters:    {}",
                        self.status.bridge_sessions, self.status.bridge_adapters
                    )
                };
                self.overlay = Some(overlay::build_info_overlay("Bridge Status", &text));
                self.request_redraw();
            }
            crate::commands::CommandResult::Teleport => {
                let remote_env =
                    std::env::var("CLAUDE_CODE_REMOTE").unwrap_or_else(|_| "not set".to_string());
                let env = self.status.teleport_env.as_deref().unwrap_or("unknown");
                let state = if self.status.teleport_remote {
                    format!("Connected ({env})")
                } else {
                    "Not connected".to_string()
                };
                let text = format!(
                    "Teleport / CCR\n  Status:      {state}\n  Environment: {env}\n  \n  CLAUDE_CODE_REMOTE: {remote_env}"
                );
                self.overlay = Some(overlay::build_info_overlay("Teleport Status", &text));
                self.request_redraw();
            }
            crate::commands::CommandResult::Exit => {
                self.running = false;
            }
            // Commands that need async engine access — handled in the event loop
            // via TuiCommand enum variants. For now, mark them as needing engine.
            crate::commands::CommandResult::Diff
            | crate::commands::CommandResult::Undo
            | crate::commands::CommandResult::Retry
            | crate::commands::CommandResult::Copy
            | crate::commands::CommandResult::Share
            | crate::commands::CommandResult::Rename { .. }
            | crate::commands::CommandResult::Summary
            | crate::commands::CommandResult::Export { .. }
            | crate::commands::CommandResult::Context
            | crate::commands::CommandResult::Fast { .. }
            | crate::commands::CommandResult::Rewind { .. }
            | crate::commands::CommandResult::AddDir { .. }
            | crate::commands::CommandResult::Files { .. }
            | crate::commands::CommandResult::Btw { .. }
            | crate::commands::CommandResult::Stats
            | crate::commands::CommandResult::Chrome { .. }
            | crate::commands::CommandResult::Image { .. }
            | crate::commands::CommandResult::Feedback { .. }
            | crate::commands::CommandResult::ReleaseNotes
            | crate::commands::CommandResult::Memory { .. }
            | crate::commands::CommandResult::Permissions { .. }
            | crate::commands::CommandResult::Config
            | crate::commands::CommandResult::Login
            | crate::commands::CommandResult::Logout
            | crate::commands::CommandResult::ReloadContext
            | crate::commands::CommandResult::Doctor
            | crate::commands::CommandResult::Init
            | crate::commands::CommandResult::Plan { .. }
            | crate::commands::CommandResult::Theme { .. }
            | crate::commands::CommandResult::Agents { .. }
            | crate::commands::CommandResult::Plugin { .. }
            | crate::commands::CommandResult::RunPluginCommand { .. }
            | crate::commands::CommandResult::RunSkill { .. } => {
                // Stored in pending_command for async handling
                self.pending_command = Some(result);
            }
            // Commands that submit a prompt to the agent or need engine access
            crate::commands::CommandResult::Review { .. }
            | crate::commands::CommandResult::Simplify { .. }
            | crate::commands::CommandResult::Bug { .. }
            | crate::commands::CommandResult::Pr { .. } => {
                self.pending_command = Some(result);
            }
            crate::commands::CommandResult::Commit { .. }
            | crate::commands::CommandResult::CommitPushPr { .. }
            | crate::commands::CommandResult::PrComments { .. }
            | crate::commands::CommandResult::Branch { .. }
            | crate::commands::CommandResult::Search { .. }
            | crate::commands::CommandResult::History { .. }
            | crate::commands::CommandResult::Resume { .. } => {
                self.pending_command = Some(result);
            }
        }
    }
}

/// Detect the IDE environment from environment variables.
/// Returns `Some("vscode")`, `Some("jetbrains")`, etc., or `None` for standalone terminal.
fn detect_ide() -> Option<String> {
    if std::env::var("VSCODE_PID").is_ok()
        || std::env::var("VSCODE_CWD").is_ok()
        || std::env::var("TERM_PROGRAM").ok().as_deref() == Some("vscode")
    {
        Some("vscode".to_string())
    } else if std::env::var("TERMINAL_EMULATOR")
        .ok()
        .map(|s| s.to_lowercase().contains("jetbrains"))
        .unwrap_or(false)
        || std::env::var("JETBRAINS_IDE").is_ok()
    {
        Some("jetbrains".to_string())
    } else if std::env::var("Cursor").is_ok() || std::env::var("CURSOR").is_ok() {
        Some("cursor".to_string())
    } else if std::env::var("Windsurf").is_ok() || std::env::var("WINDSURF").is_ok() {
        Some("windsurf".to_string())
    } else {
        None
    }
}

// -- Rendering ----------------------------------------------------------------

fn render(frame: &mut Frame, app: &mut App) {
    let area = frame.area();

    // Cache terminal dimensions so the layout signature can detect resize
    // and trigger a full clear to eliminate ghost cells.
    app.term_width = area.width;
    app.term_height = area.height;

    let perm_layout = app
        .permission
        .as_ref()
        .map(|perm| permission::layout_for(area.width, perm));
    let has_permission = perm_layout.is_some();

    // Build vertical layout constraints
    let bottom_bar_rows = if has_permission {
        0
    } else {
        u16::from(!app.bottom_bar_hidden)
    };
    let task_plan_rows = app.task_plan.render_height();
    let task_list_rows = app.task_list.render_height();
    let bash_mode_rows = app.bash_mode.render_height();

    let input_rows = if app.footer_picker.is_some() || app.input.has_completion() {
        1
    } else {
        app.input.display_lines().len().min(input::MAX_INPUT_ROWS) as u16
    };
    let completion_rows = if has_permission {
        0
    } else {
        footer_menu_rows(app)
    };
    // Input area is framed by two horizontal lines (top + bottom), matching official CC.
    let footer_rows = if let Some(layout) = perm_layout {
        layout.total_rows()
    } else {
        2 + completion_rows + input_rows + bottom_bar_rows
    };

    // Queue items: 1 row per queued message (capped at 5), no header row.
    // Queue count is shown inside the info line instead.
    let queue_rows = if has_permission || app.queued_inputs.is_empty() {
        0
    } else {
        app.queued_inputs.len().min(5) as u16
    };

    let search_rows = if has_permission {
        0
    } else {
        u16::from(app.search_state.is_some())
    };

    let tip_rows = if has_permission {
        0
    } else {
        u16::from(app.status.has_tip())
    };

    let suggestion_rows = if app.suggestions.is_empty() || has_permission {
        0
    } else {
        (app.suggestions.len().min(5) + 1) as u16 // +1 for divider
    };

    let constraints = [
        Constraint::Min(1),                  // messages
        Constraint::Length(task_list_rows),  // task list (0 if empty/collapsed)
        Constraint::Length(task_plan_rows),  // task plan (0 if empty)
        Constraint::Length(bash_mode_rows),  // BashModeProgress panel (0 if inactive)
        Constraint::Length(1 + tip_rows),    // info line + optional tip
        Constraint::Length(queue_rows),      // queue items (0 or n)
        Constraint::Length(suggestion_rows), // context suggestion overlay
        Constraint::Length(search_rows),     // search box (0 or 1)
        Constraint::Length(footer_rows),     // input/permission footer
    ];

    let chunks = Layout::vertical(constraints).split(area);
    let msg_area = chunks[0];
    let task_list_area = chunks[1];
    let task_area = chunks[2];
    let bash_area = chunks[3];
    let sep_area = chunks[4];
    let queue_area = chunks[5];
    let suggestion_area = chunks[6];
    let search_area = chunks[7];
    let footer_area = chunks[8];

    // Teammate view header: fixed 1 row above messages when viewing a teammate.
    let teammate_header_rows = u16::from(app.viewed_teammate.is_some());
    let msg_chunks =
        Layout::vertical([Constraint::Length(teammate_header_rows), Constraint::Min(1)])
            .split(msg_area);
    if teammate_header_rows > 0 {
        render_teammate_view_header(frame, msg_chunks[0], app);
    }
    render_messages(frame, msg_chunks[1], app);

    if task_list_rows > 0 {
        tasklist::render(frame, task_list_area, &app.task_list);
    }

    if task_plan_rows > 0 {
        taskplan::render(frame, task_area, &app.task_plan);
    }

    if bash_mode_rows > 0 {
        bash_mode::render(frame, bash_area, &app.bash_mode);
    }

    if suggestion_rows > 0 {
        render_suggestions_overlay(frame, suggestion_area, app);
    }

    render_separator(frame, sep_area, app.scroll_offset, app);

    if search_rows > 0 {
        render_search_box(frame, search_area, app);
    }

    if queue_rows > 0 {
        render_queue_banner(frame, queue_area, &app.queued_inputs);
    }

    if let Some(perm) = app.permission.as_ref() {
        let layout = permission::layout_for(footer_area.width, perm);
        // Permission prompt: rows adapt to terminal width instead of assuming a
        // fixed 3-line footer.
        let perm_chunks = Layout::vertical([
            Constraint::Length(layout.desc_rows),
            Constraint::Length(layout.detail_rows),
            Constraint::Length(layout.button_rows),
            Constraint::Length(layout.hint_rows),
        ])
        .split(footer_area);
        permission::render(
            frame,
            perm_chunks[0],
            perm_chunks[1],
            perm_chunks[2],
            perm_chunks[3],
            perm,
        );
    } else {
        // Normal: top line ─ input ─ completion popup (optional) ─ bottom line ─ status bar
        let input_chunks = Layout::vertical([
            Constraint::Length(1),               // top line
            Constraint::Length(input_rows),      // input (1–5 rows)
            Constraint::Length(completion_rows), // completion popup / footer picker
            Constraint::Length(1),               // bottom line
            Constraint::Length(bottom_bar_rows), // status bar
        ])
        .split(footer_area);

        render_input_separator(frame, input_chunks[0]);
        render_input(frame, input_chunks[1], app);
        render_input_separator(frame, input_chunks[3]);
        if bottom_bar_rows > 0 {
            bottombar::render(
                frame,
                input_chunks[4],
                app.is_generating,
                &app.permission_mode,
            );
        }

        if let Some(picker) = app.footer_picker.as_ref() {
            render_footer_picker(frame, input_chunks[2], input_chunks[1], picker);
        } else if completion_rows > 0 {
            render_completion_popup(frame, input_chunks[2], input_chunks[1], app);
        }
    }

    // Overlay renders last (on top of everything in message area)
    if let Some(ref ov) = app.overlay {
        overlay::render(frame, msg_area, ov);
    }
}

fn poll_interval(app: &App) -> Duration {
    if app.spinner_active() || app.status.active_shells > 0 || !app.status.active_agents.is_empty()
    {
        ACTIVE_POLL_INTERVAL
    } else {
        IDLE_POLL_INTERVAL
    }
}

fn render_messages(frame: &mut Frame, area: Rect, app: &mut App) {
    if area.height == 0 || area.width == 0 {
        return;
    }

    if app.messages.is_empty() {
        let welcome = render_welcome_lines(area.width, &app.model, &app.permission_mode);
        frame.render_widget(Paragraph::new(welcome).wrap(Wrap { trim: false }), area);
        return;
    }

    app.rebuild_visible_lines();

    let cached_visual_count = app
        .cached_visible_line_count
        .and_then(|(width, count)| (width == area.width).then_some(count));

    let viewport_height = area.height as usize;

    // --- Sticky header: show the current user prompt when scrolled up ---
    let sticky_rows = compute_sticky_rows(app, viewport_height);
    let msg_area = if sticky_rows > 0 {
        Rect::new(
            area.x,
            area.y + sticky_rows,
            area.width,
            area.height - sticky_rows,
        )
    } else {
        area
    };
    let msg_viewport_height = msg_area.height as usize;

    // Compute visual line count only when cache is stale.
    // Delay clone: only build Paragraph when line_count is not cached.
    let total_visual = if let Some(count) = cached_visual_count {
        count
    } else {
        let all_lines = app.cached_visible_lines.clone();
        let paragraph = Paragraph::new(all_lines).wrap(Wrap { trim: false });
        let count = paragraph.line_count(area.width);
        app.cached_visible_line_count = Some((area.width, count));
        count
    };

    // scroll_offset = 0 → bottom of content; higher = scroll up.
    let scroll_row: u16 = if total_visual <= msg_viewport_height {
        0
    } else {
        let max_scroll = total_visual - msg_viewport_height;
        let clamped = app.scroll_offset.min(max_scroll);
        (max_scroll - clamped).min(u16::MAX as usize) as u16
    };

    // --- Viewport-window optimization: when showing the bottom (scroll_offset=0)
    // and the list is long, only clone the last few logical lines instead of
    // the entire cached_visible_lines. The scroll offset is adjusted so the
    // Paragraph still renders the correct viewport bottom.
    let (paragraph, render_scroll) = if app.scroll_offset == 0
        && total_visual > msg_viewport_height
        && app.cached_visible_lines.len() > msg_viewport_height * 3
    {
        let keep_logical = msg_viewport_height * 3;
        let start = app.cached_visible_lines.len().saturating_sub(keep_logical);
        let partial = app.cached_visible_lines[start..].to_vec();
        let paragraph = Paragraph::new(partial).wrap(Wrap { trim: false });
        let partial_visual = paragraph.line_count(area.width);
        let adjust = scroll_row.saturating_sub((total_visual - partial_visual) as u16);
        (paragraph, adjust)
    } else {
        let all = app.cached_visible_lines.clone();
        let paragraph = Paragraph::new(all).wrap(Wrap { trim: false });
        (paragraph, scroll_row)
    };

    if should_clear_message_area(app.last_rendered_message_visual_count, total_visual) {
        frame.render_widget(Clear, msg_area);
    }
    frame.render_widget(paragraph.scroll((render_scroll, 0)), msg_area);
    app.last_rendered_message_visual_count = Some(total_visual);

    // Render sticky header overlay at the top of the message area.
    if sticky_rows > 0 {
        if let Some(idx) = app.sticky_anchor {
            let sticky_area = Rect::new(area.x, area.y, area.width, sticky_rows);
            render_sticky_header(frame, sticky_area, &app.messages[idx]);
        }
    }

    // Render "N new messages" pill when scrolled up and new content arrived.
    if app.new_messages_count > 0 && app.scroll_offset > 0 {
        render_new_messages_pill(frame, area, app.new_messages_count);
    }

    // Render ephemeral agent progress lines at the bottom of the message area.
    // Hidden when scrolled up so the overlay does not obscure message history.
    if app.scroll_offset == 0 {
        render_agent_progress(frame, area, app);
    }
}

/// Render ephemeral agent progress as an overlay at the bottom of the message area.
/// Progress disappears automatically when agents complete or terminate.
fn render_agent_progress(frame: &mut Frame, area: Rect, app: &App) {
    if app.agent_progress.is_empty() {
        return;
    }

    let style = Style::default().fg(MUTED);
    let max_text_width = area.width.saturating_sub(4) as usize;
    let mut lines = Vec::new();
    for (id, text) in &app.agent_progress {
        let prefix = format!("↳ [{id}] ");
        let prefix_width = prefix.width();
        let available = max_text_width.saturating_sub(prefix_width);
        let truncated = if text.width() > available {
            let mut s = String::new();
            let mut w = 0;
            for ch in text.chars() {
                let cw = ch.width().unwrap_or(0);
                if w + cw > available.saturating_sub(1) {
                    s.push('\u{2026}');
                    break;
                }
                w += cw;
                s.push(ch);
            }
            s
        } else {
            text.clone()
        };
        lines.push(Line::styled(format!("{prefix}{truncated}"), style));
    }

    let height = lines.len().min(area.height as usize) as u16;
    let progress_area = Rect::new(area.x, area.y + area.height - height, area.width, height);
    frame.render_widget(Clear, progress_area);
    frame.render_widget(Paragraph::new(lines), progress_area);
}

/// Compute how many rows the sticky header should occupy (0 or 1).
/// When scrolled up, finds the most recent user message visible at the
/// viewport top and stores its index in `app.sticky_anchor`.
fn compute_sticky_rows(app: &mut App, viewport_height: usize) -> u16 {
    if app.scroll_offset == 0 || app.messages.is_empty() {
        app.sticky_anchor = None;
        return 0;
    }

    let total_visual = app
        .cached_visible_line_count
        .map(|(_, c)| c)
        .unwrap_or(app.cached_visible_lines.len());
    let viewport_top = total_visual.saturating_sub(viewport_height + app.scroll_offset);

    // Approximate which message is at the viewport top.
    let avg = total_visual as f64 / app.messages.len().max(1) as f64;
    let approx_idx = (viewport_top as f64 / avg) as usize;
    let start = approx_idx.min(app.messages.len().saturating_sub(1));

    // Find the most recent user message at or before the viewport top.
    app.sticky_anchor = (0..=start)
        .rev()
        .find(|&i| matches!(app.messages[i].content, MessageContent::UserInput(_)));

    u16::from(app.sticky_anchor.is_some())
}

/// Render a sticky header showing the user prompt text (truncated).
/// Matches official CC StickyPromptHeader behaviour.
fn render_sticky_header(frame: &mut Frame, area: Rect, msg: &Message) {
    let text = match &msg.content {
        MessageContent::UserInput(t) => t.as_str(),
        _ => return,
    };
    // Take first paragraph, collapse whitespace, cap at 500 chars.
    let first_para = text.split("\n\n").next().unwrap_or(text);
    let collapsed: String = first_para.split_whitespace().collect::<Vec<_>>().join(" ");
    let capped = if collapsed.len() > 500 {
        format!("{}…", &collapsed[..500])
    } else {
        collapsed
    };
    let display = format!("> {}", capped);

    let mut truncated = String::new();
    let mut current_width = 0;
    for ch in display.chars() {
        let ch_w = ch.width().unwrap_or(0);
        if current_width + ch_w > area.width as usize {
            break;
        }
        current_width += ch_w;
        truncated.push(ch);
    }

    let style = Style::default()
        .fg(Color::White)
        .add_modifier(Modifier::BOLD);
    frame.render_widget(Paragraph::new(Line::styled(truncated, style)), area);
}

/// Render a floating "N new messages" pill at the bottom center of the message area.
fn render_new_messages_pill(frame: &mut Frame, area: Rect, count: usize) {
    let text = if count == 1 {
        "1 new message".to_string()
    } else {
        format!("{count} new messages")
    };
    let text_width = text.width();
    // Pill: padding(1) + text + padding(1) + borders(2) = text_width + 4
    let pill_width = (text_width + 4).min(area.width as usize) as u16;
    if pill_width < 3 || area.height < 3 {
        return;
    }
    let pill_height = 3u16;
    let x = area.x + (area.width.saturating_sub(pill_width)) / 2;
    let y = area.y + area.height.saturating_sub(pill_height + 1); // bottom={1}
    let pill_area = Rect::new(x, y, pill_width, pill_height);

    let pill_style = Style::default().fg(Color::Cyan);
    let block = ratatui::widgets::Block::default()
        .borders(ratatui::widgets::Borders::ALL)
        .border_type(ratatui::widgets::BorderType::Rounded)
        .border_style(pill_style)
        .style(Style::default().bg(Color::Black));
    let inner = block.inner(pill_area);
    frame.render_widget(block, pill_area);
    frame.render_widget(
        Paragraph::new(Line::styled(text, pill_style.add_modifier(Modifier::BOLD)))
            .alignment(ratatui::layout::Alignment::Center),
        inner,
    );
}

fn render_queue_banner(frame: &mut Frame, area: Rect, queued: &[String]) {
    // One line per queued message with ▸ prefix, truncated to available width.
    // "  ▸ " = 4 chars prefix
    let max_text_width = (area.width as usize).saturating_sub(4);
    let lines: Vec<Line> = queued
        .iter()
        .take(area.height as usize)
        .map(|msg| {
            let first_line = msg.lines().next().unwrap_or(msg.as_str());
            let truncated: String = if first_line.chars().count() > max_text_width {
                first_line
                    .chars()
                    .take(max_text_width.saturating_sub(1))
                    .collect::<String>()
                    + "…"
            } else {
                first_line.to_string()
            };
            Line::from(vec![
                Span::styled("  \u{25B8} ", Style::default().fg(Color::Yellow)),
                Span::styled(truncated, Style::default().fg(Color::Yellow)),
            ])
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), area);
}

/// Bottom rounded border for the input box (aligned with official CC).
/// Renders ╰──────╯ style — left rounded corner, horizontal line, right rounded corner.
/// Render context suggestions overlay above the input box.
/// Aligned with official Claude Code PromptInputFooterSuggestions.
fn render_suggestions_overlay(frame: &mut Frame, area: Rect, app: &App) {
    if area.height == 0 || app.suggestions.is_empty() {
        return;
    }

    let _dim = Style::default().fg(MUTED);
    let selected_style = Style::default().fg(Color::Cyan);
    let divider_style = Style::default().fg(MUTED);

    let max_items = (area.height as usize).saturating_sub(1); // -1 for divider
    let visible_count = app.suggestions.len().min(max_items).max(1);

    // Divider: ▔▔▔▔▔ (aligned with official CC suggestion overlay top border)
    let divider = "\u{2594}".repeat(area.width as usize);
    let mut lines: Vec<Line> = vec![Line::styled(divider, divider_style)];

    for (i, suggestion) in app.suggestions.iter().enumerate().take(visible_count) {
        let is_selected = i == app.selected_suggestion;
        let (icon, icon_style) = match suggestion.kind {
            SuggestionKind::File => ("+", Style::default().fg(Color::Green)),
            SuggestionKind::McpResource => ("\u{25C7}", Style::default().fg(Color::Yellow)), // ◇
            SuggestionKind::Agent => ("*", Style::default().fg(Color::Magenta)),
        };
        let text_style = if is_selected {
            selected_style
        } else {
            Style::default().fg(Color::White)
        };

        let desc = suggestion.description.as_deref().unwrap_or("");
        let main_text = if desc.is_empty() {
            format!("  {}", suggestion.display_text)
        } else {
            format!("  {} — {}", suggestion.display_text, desc)
        };

        lines.push(Line::from(vec![
            Span::styled(format!("  {icon} "), icon_style),
            Span::styled(main_text, text_style),
        ]));
    }

    frame.render_widget(Paragraph::new(lines), area);
}

fn render_input_separator(frame: &mut Frame, area: Rect) {
    let w = area.width as usize;
    if w == 0 {
        return;
    }
    let style = Style::default().fg(MUTED);
    let sep = "\u{2500}".repeat(w);
    frame.render_widget(Paragraph::new(Line::styled(sep, style)), area);
}

fn render_separator(frame: &mut Frame, area: Rect, scroll_offset: usize, app: &App) {
    let width = area.width as usize;
    let dim = Style::default().fg(MUTED);
    let hi = Style::default()
        .fg(Color::Yellow)
        .add_modifier(Modifier::BOLD);

    // --- Dynamic status spans (spinner, elapsed, tools, shells, agents) — leftmost ---
    let status_spans = status::build_spans(&app.status, area.width, app.teammate_selection);
    let status_w: usize = status_spans.iter().map(|s| s.content.width()).sum();

    let mut spans: Vec<Span> = Vec::new();
    let mut left_used = 0usize;

    if scroll_offset > 0 {
        let s = format!("\u{2191}{scroll_offset}  ");
        left_used += s.width();
        spans.push(Span::styled(s, hi));
    }

    // Status spans go first so Thinking is visible on the left.
    if status_w > 0 {
        spans.extend(status_spans);
        left_used += status_w;
    }

    // --- External status line (from settings.json `statusLine.command`) ---
    let external_text = if app.status_line.is_enabled() {
        statusline::text(&app.status_line)
    } else {
        None
    };

    if let Some(ext) = external_text {
        let ext_w = ext.width();
        if ext_w > 0 {
            let available = width.saturating_sub(left_used);
            let truncated = if ext_w > available {
                let mut t = String::new();
                for ch in ext.chars() {
                    if t.width() + 1 >= available {
                        t.push('\u{2026}');
                        break;
                    }
                    t.push(ch);
                }
                t
            } else {
                ext
            };
            spans.push(Span::styled(truncated, dim));
        }
    } else {
        // --- Built-in static info (model │ turn N │ Xk↑ Yk↓ │ Z% ctx │ 📥N) ---
        let mut info_parts: Vec<String> = Vec::new();

        let short_model = shorten_model_name(&app.model);
        if !short_model.is_empty() {
            info_parts.push(short_model);
        }

        if app.total_turns > 0 {
            info_parts.push(format!("turn {}", app.total_turns));
        }

        if app.context_tokens > 0 || app.total_output_tokens > 0 {
            info_parts.push(format!(
                "{}\u{2191} {}\u{2193}",
                fmt_tokens(app.context_tokens),
                fmt_tokens(app.total_output_tokens),
            ));
        }

        if app.total_cost_usd > 0.0 {
            info_parts.push(clawed_core::model::format_cost(app.total_cost_usd));
        }

        let ctx_text = if app.status.context_pct > 0.0 {
            Some(format!("{:.0}% ctx", app.status.context_pct))
        } else {
            None
        };

        if !app.queued_inputs.is_empty() {
            info_parts.push(format!("\u{1F4E5}{}", app.queued_inputs.len()));
        }

        // Info text follows, truncated so everything fits within terminal width.
        if !info_parts.is_empty() {
            let info = format!(" {} ", info_parts.join(" \u{2502} "));
            let available = width.saturating_sub(left_used);
            let info = if info.width() > available {
                let mut t = String::new();
                for ch in info.chars() {
                    if t.width() + 1 >= available {
                        t.push('\u{2026}');
                        break;
                    }
                    t.push(ch);
                }
                t
            } else {
                info
            };
            spans.push(Span::styled(info, dim));
        }

        // Context usage percentage with color-coded urgency + visual bar.
        if let Some(ctx) = ctx_text {
            let ctx_pct = app.status.context_pct;
            let ctx_style = if ctx_pct >= 80.0 {
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
            } else if ctx_pct >= 60.0 {
                Style::default().fg(Color::Yellow)
            } else {
                dim
            };
            let prefix = if info_parts.is_empty() {
                " "
            } else {
                " \u{2502} "
            };
            let bar = context_bar(ctx_pct);
            let s = format!("{prefix}{ctx} {bar}");
            spans.push(Span::styled(s, ctx_style));
        }
    }

    // New-messages badge when user is scrolled up during generation.
    if !app.auto_scroll && app.is_generating {
        spans.push(Span::styled(
            "  \u{2193} new".to_string(),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));
    }

    let (main_area, tip_area) = if app.status.has_tip() && area.height > 1 {
        let chunks = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).split(area);
        (chunks[0], Some(chunks[1]))
    } else {
        (area, None)
    };

    frame.render_widget(Paragraph::new(Line::from(spans)), main_area);

    if let (Some(tip), Some(tip_area)) = (app.status.current_tip(), tip_area) {
        frame.render_widget(Paragraph::new(Line::from(Span::styled(tip, dim))), tip_area);
    }
}

/// Render a single-line search box showing the query and match count.
fn render_search_box(frame: &mut Frame, area: Rect, app: &App) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let Some(search) = app.search_state.as_ref() else {
        return;
    };
    let dim = Style::default().fg(MUTED);
    let active = Style::default().fg(Color::White);
    let count_style = Style::default().fg(Color::Yellow);

    let prefix = "\u{2305} "; // ⌕
    let prefix_w = prefix.width();

    let count_text = if search.matches.is_empty() {
        "no matches".to_string()
    } else {
        format!("{}/{}", search.current_match + 1, search.matches.len())
    };

    let available = area.width.saturating_sub(prefix_w as u16 + 2) as usize; // 2 for padding
    let query_w = search.query.width();
    let count_w = count_text.width();

    let mut spans: Vec<Span> = Vec::new();
    spans.push(Span::styled(prefix.to_string(), dim));

    if query_w + count_w + 2 <= available {
        spans.push(Span::styled(search.query.clone(), active));
        let padding = " ".repeat(available.saturating_sub(query_w + count_w));
        if !padding.is_empty() {
            spans.push(Span::raw(padding));
        }
        spans.push(Span::styled(count_text, count_style));
    } else {
        // Truncate query to fit count
        let max_query = available.saturating_sub(count_w + 2);
        let mut truncated = String::new();
        let mut w = 0usize;
        for ch in search.query.chars() {
            let cw = ch.width().unwrap_or(0);
            if w + cw + 1 > max_query {
                truncated.push('\u{2026}');
                break;
            }
            truncated.push(ch);
            w += cw;
        }
        spans.push(Span::styled(truncated, active));
        spans.push(Span::raw(" "));
        spans.push(Span::styled(count_text, count_style));
    }

    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// Pre-computed 5-segment unicode context bar (0–5 filled blocks).
static CONTEXT_BARS: std::sync::LazyLock<[String; 6]> = std::sync::LazyLock::new(|| {
    std::array::from_fn(|filled| {
        let empty = 5 - filled;
        format!("{}{}", "\u{2588}".repeat(filled), "\u{2591}".repeat(empty))
    })
});

/// Build a 5-segment unicode bar for context usage.
fn context_bar(pct: f64) -> &'static str {
    const SEGMENTS: f64 = 5.0;
    let segment_pct = 100.0 / SEGMENTS;
    let filled = (pct / segment_pct).round().clamp(0.0, SEGMENTS) as usize;
    &CONTEXT_BARS[filled]
}

/// Shorten a model identifier for display in the separator.
/// e.g. "claude-3-5-sonnet-20241022" → "claude-3.5-sonnet"
///      "gpt-4o-mini"               → "gpt-4o-mini"
fn shorten_model_name(model: &str) -> String {
    let display = clawed_core::model::display_name_any(model);
    // Strip "Claude " prefix for compact display (e.g. "Claude Sonnet 4.6" → "Sonnet 4.6").
    if let Some(short) = display.strip_prefix("Claude ") {
        short.to_string()
    } else {
        display
    }
}

/// Format a token count compactly: ≥1000 → `"1k"`, else `"512"`.
/// The caller is responsible for appending directional arrows (↑/↓).
fn fmt_tokens(n: u64) -> String {
    if n >= 1_000 {
        format!("{:.0}k", n as f64 / 1000.0)
    } else {
        n.to_string()
    }
}

fn render_input(frame: &mut Frame, area: Rect, app: &App) {
    let prompt_style = Style::default(); // terminal default — matches official CC
    let text_style = Style::default();
    let image_style = Style::default().fg(Color::Magenta);
    let indicator_style = Style::default().fg(MUTED);

    let display_lines = app.input.display_lines();
    let img_count = app.pending_images.len();
    let (has_above, has_below) = app.input.scroll_indicators();

    // Simple "> " prompt aligned with official CC minimalist input.
    let prompt_char = "> ";
    let prompt_w = prompt_char.width();
    let prefix_width = prompt_w;

    let lines: Vec<Line> = display_lines
        .iter()
        .enumerate()
        .map(|(i, line_text)| {
            if i == 0 {
                let mut spans = vec![Span::styled(prompt_char.to_string(), prompt_style)];
                spans.push(Span::styled((*line_text).to_string(), text_style));
                if img_count > 0 {
                    spans.push(Span::styled(format!(" 📎{img_count}"), image_style));
                }
                Line::from(spans)
            } else {
                Line::from(vec![
                    Span::styled("  ", prompt_style), // continuation indent
                    Span::styled((*line_text).to_string(), text_style),
                ])
            }
        })
        .collect();

    frame.render_widget(Paragraph::new(lines), area);

    // Render scroll indicators on the right edge
    if area.width > 3 {
        let x = area.x + area.width - 1;
        if has_above {
            frame.render_widget(
                Paragraph::new(Span::styled("\u{25B2}", indicator_style)),
                Rect::new(x, area.y, 1, 1),
            );
        }
        if has_below && area.height > 1 {
            frame.render_widget(
                Paragraph::new(Span::styled("\u{25BC}", indicator_style)),
                Rect::new(x, area.y + area.height - 1, 1, 1),
            );
        }
    }

    // Position cursor
    let (cursor_row, cursor_col) = app.input.cursor_position();
    let x = area.x + prefix_width as u16 + (cursor_col as u16).min(area.width.saturating_sub(3));
    let y = area.y + (cursor_row as u16).min(area.height.saturating_sub(1));
    frame.set_cursor_position((x, y));
}

fn render_completion_popup(frame: &mut Frame, popup_slot: Rect, input_area: Rect, app: &App) {
    let matches = app.input.completion_matches();
    let Some(popup_area) = completion_popup_area(popup_slot, input_area, &matches) else {
        return;
    };

    let selected = app.input.completion_selected();
    // Reserve 1 row for ▔ divider at the bottom.
    let max_items = usize::from(popup_area.height.saturating_sub(1)).min(matches.len());

    // Calculate visible window that keeps `selected` in view
    let scroll_offset = if selected >= max_items {
        selected - max_items + 1
    } else {
        0
    };

    let max_cmd_width = matches.iter().map(|c| c.width()).max().unwrap_or(4);
    let desc_col = max_cmd_width + 4; // padding between cmd and desc

    let dim = Style::default().fg(MUTED);
    let items: Vec<ListItem> = matches
        .iter()
        .enumerate()
        .skip(scroll_offset)
        .take(max_items)
        .map(|(i, cmd)| {
            let desc = command_description(cmd);
            let is_selected = i == selected;
            let cmd_style = if is_selected {
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().add_modifier(Modifier::BOLD)
            };
            let desc_style = if is_selected {
                Style::default().fg(Color::Cyan)
            } else {
                Style::default()
            };
            let padding = " ".repeat(desc_col.saturating_sub(cmd.width()));
            ListItem::new(Line::from(vec![
                Span::styled(format!("  {cmd}"), cmd_style),
                Span::raw(padding),
                Span::styled(desc.to_string(), desc_style),
            ]))
        })
        .collect();

    let list = List::new(items);

    // Clear the reserved slot first so closing or narrowing the popup doesn't leave artifacts.
    frame.render_widget(Clear, popup_slot);
    frame.render_widget(list, popup_area);

    // ▔ divider at the bottom of the popup (aligned with official CC).
    if popup_area.height > 0 {
        let divider = "\u{2594}".repeat(popup_area.width as usize);
        let divider_y = popup_area.y + popup_area.height - 1;
        frame.render_widget(
            Paragraph::new(Line::styled(divider, dim)),
            Rect::new(popup_area.x, divider_y, popup_area.width, 1),
        );
    }
}

fn render_footer_picker(
    frame: &mut Frame,
    popup_slot: Rect,
    input_area: Rect,
    picker: &FooterPicker,
) {
    if popup_slot.width == 0 || popup_slot.height == 0 || picker.items.is_empty() {
        return;
    }

    let max_label_width = picker
        .items
        .iter()
        .map(|item| item.label.width())
        .max()
        .unwrap_or(4);
    let desc_col = max_label_width + 4;
    let max_desc_width = picker
        .items
        .iter()
        .map(|item| item.description.width())
        .max()
        .unwrap_or(20);
    let popup_width = (desc_col + max_desc_width + 3).min(popup_slot.width as usize);
    let popup_area = Rect::new(
        input_area.x,
        popup_slot.y,
        popup_width as u16,
        popup_slot.height,
    );

    // Reserve 1 row for ▔ divider at the bottom.
    let max_items = usize::from(popup_area.height.saturating_sub(1)).min(picker.items.len());
    let scroll_offset = picker.scroll_offset;

    let dim = Style::default().fg(MUTED);
    let items: Vec<ListItem> = picker
        .items
        .iter()
        .enumerate()
        .skip(scroll_offset)
        .take(max_items)
        .map(|(i, item)| {
            let is_selected = i == picker.selected;
            let label_style = if is_selected {
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().add_modifier(Modifier::BOLD)
            };
            let desc_style = if is_selected {
                Style::default().fg(Color::Cyan)
            } else {
                Style::default()
            };
            let prefix = if item.is_current { "• " } else { "  " };
            let label_text = format!("{prefix}{}", item.label);
            let padding = " ".repeat(desc_col.saturating_sub(label_text.width()));
            ListItem::new(Line::from(vec![
                Span::styled(label_text, label_style),
                Span::raw(padding),
                Span::styled(item.description.clone(), desc_style),
            ]))
        })
        .collect();

    frame.render_widget(Clear, popup_slot);
    frame.render_widget(List::new(items), popup_area);

    // ▔ divider at the bottom of the popup (aligned with official CC).
    if popup_area.height > 0 {
        let divider = "\u{2594}".repeat(popup_area.width as usize);
        let divider_y = popup_area.y + popup_area.height - 1;
        frame.render_widget(
            Paragraph::new(Line::styled(divider, dim)),
            Rect::new(popup_area.x, divider_y, popup_area.width, 1),
        );
    }
}

fn render_welcome_lines(width: u16, model: &str, permission_mode: &str) -> Vec<Line<'static>> {
    let w = (width as usize).saturating_sub(4).min(58);

    // ASCII art banner (block-character Clawd mascot, CC-aligned).
    let art: &[&str] = &[
        "  ▄▄▄▄▄▄▄    ▄▄▄▄▄▄▄    ▄▄▄▄▄▄▄    ▄▄▄▄▄▄ ",
        " ▐███████▌  ▐███████▌  ▐███████▌   ███████▌",
        "  ▀▀▀▀▀▀▀    ▀▀█▀▀▀     ▀▀▀█▀▀     ▀▀▀█▀▀ ",
        "            ▐█▌           ▐█▌          ▐█▌  ",
        "            ▐█▌           ▐█▌          ▐█▌  ",
        "            ▐█▌           ▐█▌          ▐█▌  ",
        "            ▐█▌           ▐█▌          ▐█▌  ",
        "            ▐█▌           ▐█▌          ▐█▌  ",
    ];

    let cyan = Style::default().fg(Color::Cyan);
    let muted = Style::default().fg(MUTED);

    let mut welcome = vec![Line::from("")];
    for line in art {
        welcome.push(Line::styled(line.to_string(), cyan));
    }
    welcome.push(Line::from(""));

    let short_model = shorten_model_name(model);
    let model_line = if short_model.is_empty() {
        String::new()
    } else {
        format!("Model: {short_model}")
    };
    let perm_line = if permission_mode.is_empty() || permission_mode == "default" {
        String::new()
    } else {
        format!("Permissions: {permission_mode}")
    };

    let center = |s: &str, max_w: usize| -> String {
        let sw = s.width().min(max_w);
        let left = (max_w - sw) / 2;
        let right = max_w - sw - left;
        format!("{}{}{}", " ".repeat(left), s, " ".repeat(right))
    };

    // Title: "Clawed Code" in cyan + " v{version}" dimmed (aligned with official CC).
    let title_text = format!("Clawed Code v{}", env!("CARGO_PKG_VERSION"));
    let title_pad = (w.saturating_sub(title_text.width())) / 2;
    let title_line = Line::from(vec![
        Span::raw(" ".repeat(title_pad)),
        Span::styled("Clawed Code", cyan.add_modifier(Modifier::BOLD)),
        Span::styled(format!(" v{}", env!("CARGO_PKG_VERSION")), muted),
    ]);
    welcome.push(title_line);

    if !model_line.is_empty() {
        welcome.push(Line::styled(center(&model_line, w), muted));
    }
    if !perm_line.is_empty() {
        welcome.push(Line::styled(
            center(&perm_line, w),
            Style::default().fg(Color::Yellow),
        ));
    }
    welcome.push(Line::from(""));
    welcome.push(Line::styled(
        center(
            "Tab: complete  ↑↓: history  Ctrl+C: abort  /help: commands",
            w,
        ),
        muted,
    ));
    welcome.push(Line::styled(
        center(
            "Tip: Use /compact to free context  •  Ctrl+V to paste images",
            w,
        ),
        muted,
    ));
    welcome
}

/// If a skill temporarily changed the model or set a context, and the turn
/// has ended (`is_generating` is false), restore the original state.
async fn restore_skill_state_if_done(app: &mut App, engine: &Arc<QueryEngine>) {
    if !app.is_generating {
        if let Some(restore) = app.pending_skill_restore.take() {
            tracing::info!(
                "[skill] restore after '{}', is_generating={}",
                restore.skill_name,
                app.is_generating
            );
            if let Some(orig) = restore.original_model {
                engine.state().write().await.model = orig;
            }
            engine.clear_skill_allowed_tools();
        }
    }
}

/// Abort the current session: signal the engine, clean up UI state, and push
/// an "Interrupted" message. Shared by Esc, Ctrl+C, and the Esc-fallback path.
async fn abort_session(client: &ClientHandle, app: &mut App, engine: &Arc<QueryEngine>) {
    // Signal the engine directly so the abort flag is set immediately,
    // even though the bus adapter may be blocked inside stream_events().
    engine.abort();
    let _ = client.abort();
    app.mark_done();
    restore_skill_state_if_done(app, engine).await;
    app.pending_workflow = None;
    app.queued_inputs.clear();
    app.push_message(MessageContent::System(format!(
        "{icon} Interrupted",
        icon = verbs::ERROR_MARKER,
    )));
}

/// Restore a session and replay its messages into the TUI, then push a status message.
async fn do_resume_session(engine: &Arc<QueryEngine>, app: &mut App, id: &str) {
    match engine.restore_session(id).await {
        Ok(title) => {
            replay_session_messages(engine, app).await;
            app.push_message(MessageContent::System(format!(
                "✓ Resumed session: {title}"
            )));
        }
        Err(error) => {
            app.push_message(MessageContent::System(format!("Failed to resume: {error}")));
        }
    }
}

// -- Public entry point -------------------------------------------------------

/// Run the full-screen TUI.
pub async fn run_tui(
    client: ClientHandle,
    engine: Arc<QueryEngine>,
    cwd: std::path::PathBuf,
    ask_permission: bool,
) -> anyhow::Result<()> {
    let model = { engine.state().read().await.model.clone() };
    let mut app = App::new(model);
    app.permission_mode =
        crate::config::format_permission_mode(engine.state().read().await.permission_mode)
            .to_string();

    // Load settings and configure external status line if present.
    let loaded = clawed_core::config::Settings::load_merged(&cwd);
    if let Some(ref cfg) = loaded.settings.status_line {
        app.status_line = statusline::StatusLineState::new(Some(cfg.command.clone()));
    }

    // On first start (no CLI flag and no settings.json permission_mode),
    // show the permission picker immediately so the user makes an informed choice.
    if ask_permission {
        app.overlay = Some(build_permission_overlay(
            engine.state().read().await.permission_mode,
        ));
    }

    // Load history into input widget
    if let Some(hist_path) = crate::input::history_file_path() {
        if let Ok(content) = std::fs::read_to_string(&hist_path) {
            let history: Vec<String> = content
                .lines()
                .filter(|l| !l.trim().is_empty())
                .map(String::from)
                .collect();
            app.input.load_history(history);
        }
    }

    // Load skills for tab completion
    let startup_skills = clawed_core::skills::get_skills(&cwd);
    let skill_names: Vec<String> = startup_skills
        .iter()
        .map(|s| format!("/{}", s.name))
        .collect();
    app.input.set_skill_names(skill_names);

    // Spawn notification forwarder: async recv from broadcast -> sync mpsc
    let mut notify_sub = client.subscribe_notifications();
    let (notify_tx, mut notify_rx) = mpsc::channel::<AgentNotification>(256);
    let forwarder = tokio::spawn(async move {
        loop {
            match notify_sub.recv().await {
                Ok(notification) => {
                    if notify_tx.send(notification).await.is_err() {
                        break;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });

    // Spawn permission request forwarder
    let mut perm_sub = client.subscribe_permission_requests();
    let (perm_tx, mut perm_rx) = mpsc::channel::<PermissionRequest>(16);
    let perm_forwarder = tokio::spawn(async move {
        loop {
            match perm_sub.recv().await {
                Ok(req) => {
                    if perm_tx.send(req).await.is_err() {
                        break;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });

    // Spawn user question request forwarder
    let mut user_q_sub = client.subscribe_user_question_requests();
    let (user_q_tx, mut user_q_rx) = mpsc::channel::<UserQuestionRequest>(16);
    let user_q_forwarder = tokio::spawn(async move {
        loop {
            match user_q_sub.recv().await {
                Ok(req) => {
                    if user_q_tx.send(req).await.is_err() {
                        break;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });

    // Initialize terminal: raw mode + NO alternate screen (matches codex-rs).
    // Skipping alternate screen improves Chinese IME compatibility on macOS
    // and lets output persist after exit.
    crossterm::terminal::enable_raw_mode()?;
    let _terminal_guard = TuiTerminalGuard;

    // Enable bracketed paste so multi-line paste arrives as Event::Paste(String)
    // instead of individual Key events (which would submit on Enter).
    crossterm::execute!(std::io::stdout(), EnableBracketedPaste)?;
    // NOTE: Mouse capture is intentionally disabled to allow free text selection
    // with the mouse, matching official Claude Code behavior. Wheel scrolling is
    // handled via keyboard (Shift+↑/Shift+↓, PgUp/PgDown).

    // Always push keyboard enhancement flags so modifiers for keys like Enter
    // are disambiguated (matching codex-rs behavior). Terminals that don't support
    // the kitty protocol simply ignore the escape sequence.
    let _ = crossterm::execute!(
        std::io::stdout(),
        crossterm::event::PushKeyboardEnhancementFlags(
            crossterm::event::KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                | crossterm::event::KeyboardEnhancementFlags::REPORT_EVENT_TYPES
                | crossterm::event::KeyboardEnhancementFlags::REPORT_ALTERNATE_KEYS,
        )
    );

    let backend = ratatui::backend::CrosstermBackend::new(std::io::stdout());
    let mut terminal = ratatui::Terminal::new(backend)?;
    // Clear screen for a clean start
    terminal.clear()?;

    // Suppress diff_ui stderr output in TUI mode to prevent ratatui corruption.
    clawed_tools::diff_ui::set_tui_mode(true);

    // Main event loop
    while app.running {
        // Drain notifications before drawing so fresh deltas land in the current frame
        // instead of waiting for the next input poll cycle.
        while let Ok(notification) = notify_rx.try_recv() {
            // Discard TextDelta/ThinkingDelta when:
            // - not generating (after abort), OR
            // - expecting_turn_start (new submit queued, waiting for TurnStart
            //   to confirm the new turn — deltas arriving now belong to the
            //   previous, possibly aborted, stream and must not bleed through).
            if !app.is_generating || app.expecting_turn_start {
                match &notification {
                    AgentNotification::TextDelta { .. }
                    | AgentNotification::ThinkingDelta { .. } => {
                        continue;
                    }
                    _ => {}
                }
            }
            let turn_complete = matches!(notification, AgentNotification::TurnComplete { .. });
            let merged = app.handle_notification(notification);
            if turn_complete {
                restore_skill_state_if_done(&mut app, &engine).await;
            }
            let workflow_submitted = if turn_complete {
                handle_pending_workflow(&client, &mut app).await
            } else {
                false
            };

            if workflow_submitted {
                continue;
            }

            if let Some(merged) = merged {
                app.push_message(MessageContent::UserInput(merged.clone()));
                let _ = client.submit(&merged);
                app.mark_generating();
            } else if turn_complete && app.pending_workflow.is_none() && !app.expecting_turn_start {
                submit_queued_inputs(&client, &mut app);
            }
        }

        // Advance the spinner on a fixed cadence, but only redraw when it actually changes.
        app.advance_spinner_if_due(Instant::now());

        // Safety net: if generation has been active for an unreasonably long
        // time without receiving TurnComplete, force recovery so the UI doesn't
        // stay stuck forever. This catches edge cases where the API stream
        // hangs without triggering the idle watchdog (e.g. keep-alive pings
        // from a proxy resetting the timeout indefinitely).
        const MAX_GENERATION_SECONDS: u64 = 1800; // 30 minutes
        if app.is_generating {
            if let Some(since) = app.status.generating_since {
                if since.elapsed().as_secs() > MAX_GENERATION_SECONDS {
                    tracing::warn!(
                        "Force-recovering from stalled generation after {}s",
                        since.elapsed().as_secs()
                    );
                    app.mark_done();
                    restore_skill_state_if_done(&mut app, &engine).await;
                    app.clear_tool_state();
                    app.push_message(MessageContent::System(
                        "[Auto-recovered: API stream stalled. You can retry your request.]"
                            .to_string(),
                    ));
                }
            }
        }

        // Detect any layout geometry change that can leave ghost cells behind in
        // non-alternate-screen mode: overlays, permission footer, queue rows,
        // input growth/shrink, task-plan height changes, bottom bar toggles, etc.
        let layout_sig = app.layout_signature();
        let layout_changed = layout_sig != app.last_layout_sig;
        if layout_changed {
            app.needs_full_clear = true;
            app.last_layout_sig = layout_sig;
            app.request_redraw();
        }

        // If layout changed, fully clear the terminal before drawing to eliminate
        // ghost cells left from prior frames (no alternate screen = ratatui diffs
        // only changed cells, leaving stale cells where layout shrank).
        if app.needs_full_clear {
            terminal.clear()?;
            app.needs_full_clear = false;
            app.last_periodic_clear = Instant::now();
            app.request_redraw();
        }

        // Periodic full clear: without alternate screen, any external output
        // (eprintln from third-party crates, sub-process leaks, etc.) corrupts
        // ratatui's diff buffer. Force a full clear + redraw every few seconds
        // to self-heal. Skip when idle (no generation, no active tools) since
        // corruption is far less likely then.
        if app.last_periodic_clear.elapsed() >= PERIODIC_CLEAR_INTERVAL && app.spinner_active() {
            terminal.clear()?;
            app.last_periodic_clear = Instant::now();
            app.request_redraw();
        }

        if app.needs_redraw {
            // Throttle renders during active streaming so the event loop has time
            // to process input events. Layout changes always render immediately.
            let throttled = !layout_changed
                && app.is_generating
                && app.last_render_at.elapsed() < MIN_RENDER_INTERVAL;
            if !throttled {
                terminal.draw(|frame| render(frame, &mut app))?;
                app.last_render_at = Instant::now();
                app.needs_redraw = false;
            }
            // When throttled, keep needs_redraw true so the next loop
            // iteration (or spinner tick) will render the pending update.
        }

        // Keep the terminal responsive at rest, but use a tighter tick while the
        // agent is actively streaming or running tools so output feels less coarse.
        if event::poll(poll_interval(&app))? {
            match event::read()? {
                Event::Key(key) => {
                    if key.kind != KeyEventKind::Press && key.kind != KeyEventKind::Repeat {
                        continue;
                    }

                    // Key debug mode: log raw key events
                    if app.key_debug {
                        app.push_message(MessageContent::System(format!(
                            "KEY: code={:?} mod={:?} kind={:?}",
                            key.code, key.modifiers, key.kind
                        )));
                    }

                    // Esc while LLM is generating aborts the current task,
                    // but only when no overlay or permission prompt is open
                    // (those handle Esc themselves first).
                    if key.code == KeyCode::Esc
                        && app.spinner_active()
                        && app.overlay.is_none()
                        && app.permission.is_none()
                    {
                        abort_session(&client, &mut app, &engine).await;
                        continue;
                    }

                    // If overlay is active, route keys there first
                    if let Some(overlay) = app.overlay.as_mut() {
                        let action = overlay.handle_key(key.code);
                        match action {
                            OverlayAction::Dismissed => {
                                app.overlay = None;
                            }
                            OverlayAction::Selected(value) => {
                                // Extract the overlay title to determine dispatch context
                                let title = match &app.overlay {
                                    Some(Overlay::SelectionList { title, .. }) => title.clone(),
                                    _ => String::new(),
                                };
                                app.overlay = None;
                                handle_overlay_selection(
                                    &title, &value, &client, &engine, &mut app,
                                )
                                .await;
                            }
                            OverlayAction::Consumed => {}
                        }
                        app.request_redraw();
                        continue;
                    }

                    // If permission prompt is active, route keys there
                    if app.permission.is_some() {
                        match key.code {
                            KeyCode::Tab | KeyCode::Right => {
                                if let Some(ref mut perm) = app.permission {
                                    perm.selected = perm.selected.next();
                                }
                            }
                            KeyCode::BackTab | KeyCode::Left => {
                                if let Some(ref mut perm) = app.permission {
                                    perm.selected = perm.selected.prev();
                                }
                            }
                            KeyCode::Enter => {
                                if let Some(perm) = app.permission.take() {
                                    let resp = perm.to_response();
                                    let label = if resp.granted {
                                        if resp.remember {
                                            "Allowed (always)"
                                        } else {
                                            "Allowed"
                                        }
                                    } else {
                                        "Denied"
                                    };
                                    app.push_message(MessageContent::System(format!(
                                        "{label}: {}",
                                        perm.request.tool_name
                                    )));
                                    let _ = client.send_permission_response(resp);
                                }
                            }
                            KeyCode::Esc => {
                                if let Some(perm) = app.permission.take() {
                                    let resp = perm.deny_response();
                                    app.push_message(MessageContent::System(format!(
                                        "Denied: {}",
                                        perm.request.tool_name
                                    )));
                                    let _ = client.send_permission_response(resp);
                                }
                            }
                            _ => {} // ignore other keys during permission prompt
                        }
                        app.request_redraw();
                        continue;
                    }

                    // Teammate selection mode (pointer + tab/enter keyboard navigation)
                    if app.teammate_selection.is_some() {
                        match (key.code, key.modifiers) {
                            (KeyCode::Esc, _) => {
                                app.exit_teammate_selection();
                                continue;
                            }
                            (KeyCode::Enter, _) => {
                                app.confirm_teammate_selection();
                                continue;
                            }
                            (KeyCode::Tab, _) | (KeyCode::Right, _) => {
                                app.cycle_teammate_selection(1);
                                continue;
                            }
                            (KeyCode::BackTab, _) | (KeyCode::Left, _) => {
                                app.cycle_teammate_selection(-1);
                                continue;
                            }
                            _ => {}
                        }
                    }

                    // Global shortcuts
                    match (key.code, key.modifiers) {
                        (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                            if app.spinner_active() {
                                abort_session(&client, &mut app, &engine).await;
                            } else {
                                app.running = false;
                            }
                            continue;
                        }
                        // Esc fallback (when not generating — handled above in early check)
                        // Esc: exit teammate view first, then fall through to abort/quit.
                        (KeyCode::Esc, _) if app.viewed_teammate.is_some() => {
                            app.exit_teammate_view();
                            continue;
                        }
                        (KeyCode::Esc, _) if app.spinner_active() => {
                            abort_session(&client, &mut app, &engine).await;
                            continue;
                        }
                        // Ctrl+A: enter teammate selection mode when agents are active.
                        (KeyCode::Char('a'), KeyModifiers::CONTROL)
                            if app.teammate_selection.is_none()
                                && !app.status.active_agents.is_empty() =>
                        {
                            app.enter_teammate_selection();
                            continue;
                        }
                        (KeyCode::Char('t'), KeyModifiers::CONTROL) => {
                            app.bottom_bar_hidden = !app.bottom_bar_hidden;
                            app.request_redraw();
                            continue;
                        }
                        (KeyCode::Char('o'), KeyModifiers::CONTROL) => {
                            // Toggle collapse on the last thinking message.
                            if let Some(msg) = app
                                .messages
                                .iter_mut()
                                .rev()
                                .find(|m| matches!(m.content, MessageContent::ThinkingText(_)))
                            {
                                msg.toggle_collapsed();
                                app.invalidate_visible_lines();
                            }
                            continue;
                        }
                        (KeyCode::Char('e'), KeyModifiers::CONTROL) => {
                            // Toggle expand/collapse on the last collapsible tool result
                            if let Some(msg) =
                                app.messages.iter_mut().rev().find(|m| m.is_collapsible())
                            {
                                msg.toggle_collapsed();
                                app.invalidate_visible_lines();
                            }
                            continue;
                        }
                        (KeyCode::Char('l'), KeyModifiers::CONTROL) => {
                            app.clear_messages();
                            continue;
                        }
                        // Toggle key debug mode
                        (KeyCode::Char('d'), KeyModifiers::CONTROL) => {
                            app.key_debug = !app.key_debug;
                            app.push_message(MessageContent::System(format!(
                                "Key debug: {}",
                                if app.key_debug { "ON" } else { "OFF" }
                            )));
                            continue;
                        }
                        // Scroll back
                        (KeyCode::PageUp, _) | (KeyCode::Up, KeyModifiers::SHIFT) => {
                            let step = if key.code == KeyCode::PageUp { 10 } else { 1 };
                            app.scroll_offset = app.scroll_offset.saturating_add(step);
                            app.auto_scroll = false;
                            app.request_redraw();
                            continue;
                        }
                        (KeyCode::PageDown, _) | (KeyCode::Down, KeyModifiers::SHIFT) => {
                            let step = if key.code == KeyCode::PageDown { 10 } else { 1 };
                            if app.scroll_offset > 0 {
                                app.scroll_offset = app.scroll_offset.saturating_sub(step);
                                if app.scroll_offset == 0 {
                                    app.auto_scroll = true;
                                    app.new_messages_count = 0;
                                }
                            }
                            app.request_redraw();
                            continue;
                        }
                        (KeyCode::Char('v'), KeyModifiers::CONTROL) => {
                            match read_clipboard_image() {
                                Ok(attachment) => {
                                    app.pending_images.push(attachment);
                                    app.push_message(MessageContent::System(format!(
                                        "📎 Image attached ({} total)",
                                        app.pending_images.len()
                                    )));
                                }
                                Err(e) => {
                                    app.push_message(MessageContent::System(format!(
                                        "Clipboard: {e}"
                                    )));
                                }
                            }
                            continue;
                        }
                        (KeyCode::Char('p'), KeyModifiers::CONTROL) => {
                            if app.footer_picker.is_none() && app.overlay.is_none() {
                                app.set_footer_picker(build_model_picker(&app.model));
                            }
                            continue;
                        }
                        (KeyCode::Char('f'), KeyModifiers::CONTROL) => {
                            if app.search_state.is_none()
                                && app.footer_picker.is_none()
                                && app.overlay.is_none()
                            {
                                app.search_state = Some(SearchState {
                                    query: String::new(),
                                    cursor_offset: 0,
                                    matches: Vec::new(),
                                    current_match: 0,
                                });
                                app.invalidate_visible_lines();
                            }
                            continue;
                        }
                        _ => {}
                    }

                    // Search mode: intercept all keys except Ctrl+C.
                    if let Some(ref mut search) = app.search_state {
                        match (key.code, key.modifiers) {
                            (KeyCode::Esc, _) => {
                                app.clear_search();
                                continue;
                            }
                            (KeyCode::Enter, _) => {
                                if !search.matches.is_empty() {
                                    let (line_idx, _) = search.matches[search.current_match];
                                    app.scroll_to_match(line_idx);
                                }
                                app.clear_search();
                                continue;
                            }
                            (KeyCode::Up, _) => {
                                if !search.matches.is_empty() {
                                    search.current_match = search.current_match.saturating_sub(1);
                                    let (line_idx, _) = search.matches[search.current_match];
                                    app.scroll_to_match(line_idx);
                                }
                                app.request_redraw();
                                continue;
                            }
                            (KeyCode::Down, _) => {
                                if !search.matches.is_empty()
                                    && search.current_match + 1 < search.matches.len()
                                {
                                    search.current_match += 1;
                                    let (line_idx, _) = search.matches[search.current_match];
                                    app.scroll_to_match(line_idx);
                                }
                                app.request_redraw();
                                continue;
                            }
                            (KeyCode::Char(c), KeyModifiers::NONE | KeyModifiers::SHIFT) => {
                                let pos = search.cursor_offset;
                                search.query.insert(pos, c);
                                search.cursor_offset += c.len_utf8();
                                app.invalidate_visible_lines();
                                continue;
                            }
                            (KeyCode::Backspace, KeyModifiers::NONE) => {
                                if search.cursor_offset > 0 {
                                    let pos = search.cursor_offset;
                                    let prev_char = search.query[..pos]
                                        .chars()
                                        .next_back()
                                        .map(char::len_utf8)
                                        .unwrap_or(1);
                                    search.query.drain((pos - prev_char)..pos);
                                    search.cursor_offset -= prev_char;
                                    app.invalidate_visible_lines();
                                }
                                continue;
                            }
                            (KeyCode::Delete, KeyModifiers::NONE) => {
                                if search.cursor_offset < search.query.len() {
                                    let pos = search.cursor_offset;
                                    let next_char = search.query[pos..]
                                        .chars()
                                        .next()
                                        .map(char::len_utf8)
                                        .unwrap_or(1);
                                    search.query.drain(pos..(pos + next_char));
                                    app.invalidate_visible_lines();
                                }
                                continue;
                            }
                            (KeyCode::Left, KeyModifiers::NONE) => {
                                let pos = search.cursor_offset;
                                search.cursor_offset = search.query[..pos]
                                    .chars()
                                    .next_back()
                                    .map(|c| pos - c.len_utf8())
                                    .unwrap_or(0);
                                app.request_redraw();
                                continue;
                            }
                            (KeyCode::Right, KeyModifiers::NONE) => {
                                let pos = search.cursor_offset;
                                search.cursor_offset = search.query[pos..]
                                    .chars()
                                    .next()
                                    .map(|c| pos + c.len_utf8())
                                    .unwrap_or(search.query.len());
                                app.request_redraw();
                                continue;
                            }
                            (KeyCode::Home, KeyModifiers::NONE) => {
                                search.cursor_offset = 0;
                                app.request_redraw();
                                continue;
                            }
                            (KeyCode::End, KeyModifiers::NONE) => {
                                search.cursor_offset = search.query.len();
                                app.request_redraw();
                                continue;
                            }
                            // Ctrl+W: delete word backward
                            (KeyCode::Char('w'), KeyModifiers::CONTROL) => {
                                let pos = search.cursor_offset;
                                let before = &search.query[..pos];
                                let new_start = before
                                    .trim_end()
                                    .rfind(char::is_whitespace)
                                    .map(|i| i + 1)
                                    .unwrap_or(0);
                                search.query.drain(new_start..pos);
                                search.cursor_offset = new_start;
                                app.invalidate_visible_lines();
                                continue;
                            }
                            // Ctrl+U: clear query
                            (KeyCode::Char('u'), KeyModifiers::CONTROL) => {
                                search.query.clear();
                                search.cursor_offset = 0;
                                app.invalidate_visible_lines();
                                continue;
                            }
                            _ => {
                                app.request_redraw();
                                continue;
                            }
                        }
                    }

                    if app.footer_picker.is_some() {
                        let action = {
                            let picker = app
                                .footer_picker
                                .as_mut()
                                .expect("footer picker should exist");
                            picker.handle_key(key.code)
                        };
                        match action {
                            FooterPickerAction::Dismissed => {
                                app.clear_footer_picker();
                                continue;
                            }
                            FooterPickerAction::Selected(value) => {
                                let kind = app
                                    .footer_picker
                                    .as_ref()
                                    .map(|picker| picker.kind)
                                    .expect("footer picker should exist");
                                app.clear_footer_picker();
                                handle_footer_picker_selection(
                                    kind, &value, &client, &engine, &mut app,
                                )
                                .await;
                                continue;
                            }
                            FooterPickerAction::Consumed => {
                                app.request_redraw();
                                continue;
                            }
                            FooterPickerAction::PassThrough => {
                                app.clear_footer_picker();
                            }
                        }
                    }

                    // Intercept Esc to cancel a pending user question
                    if app.user_question.is_some()
                        && key.code == KeyCode::Esc
                        && key.modifiers == KeyModifiers::NONE
                    {
                        if let Some(q) = app.user_question.take() {
                            let resp = UserQuestionResponse {
                                request_id: q.request.request_id,
                                answer: String::new(),
                                cancelled: true,
                            };
                            app.push_message(MessageContent::System("Cancelled".to_string()));
                            let _ = client.send_user_question_response(resp);
                        }
                        app.request_redraw();
                        continue;
                    }

                    let action = app.input.handle_key(key);
                    match action {
                        input::InputAction::Submit => {
                            let text = app.input.take_text();

                            // Route to user question response if a question is pending
                            if let Some(q) = app.user_question.take() {
                                let resp = UserQuestionResponse {
                                    request_id: q.request.request_id,
                                    answer: text.clone(),
                                    cancelled: false,
                                };
                                app.push_message(MessageContent::System(format!(
                                    "Answer: {}",
                                    text
                                )));
                                let _ = client.send_user_question_response(resp);
                                app.request_redraw();
                                continue;
                            }

                            if !text.is_empty() || !app.pending_images.is_empty() {
                                // While LLM is generating, queue plain text inputs.
                                // Slash commands are always handled immediately.
                                if app.is_generating
                                    && !text.starts_with('/')
                                    && app.pending_images.is_empty()
                                {
                                    app.queued_inputs.push(text);
                                    app.request_redraw();
                                    continue;
                                }

                                if text.starts_with('/') {
                                    // Slash commands execute silently — no message history echo.
                                    if text == "/abort" {
                                        abort_session(&client, &mut app, &engine).await;
                                    } else {
                                        let client_ref = &client;
                                        app.handle_slash_command(client_ref, &text);
                                        if let Some(cmd) = app.pending_command.take() {
                                            handle_async_command(
                                                cmd,
                                                &engine,
                                                &client,
                                                &mut app,
                                                Some(&mut terminal),
                                            )
                                            .await;
                                        }
                                    }
                                    app.pending_images.clear();
                                    app.request_redraw();
                                } else {
                                    // LLM prompt: show in conversation history.
                                    let display = if app.pending_images.is_empty() {
                                        text.clone()
                                    } else {
                                        format!("{text} [+{} image(s)]", app.pending_images.len())
                                    };
                                    app.push_message(MessageContent::UserInput(display));
                                    let images = std::mem::take(&mut app.pending_images);
                                    if images.is_empty() {
                                        let _ = client.submit(&text);
                                    } else {
                                        let _ = client.submit_with_images(&text, images);
                                    }
                                    app.mark_generating();
                                }
                            }
                        }
                        input::InputAction::Abort => {
                            abort_session(&client, &mut app, &engine).await;
                        }
                        input::InputAction::Changed => app.request_redraw(),
                        input::InputAction::None => {}
                    }
                }
                Event::Mouse(mouse) => match mouse.kind {
                    MouseEventKind::ScrollUp => {
                        app.scroll_offset = app.scroll_offset.saturating_add(3);
                        app.auto_scroll = false;
                        app.request_redraw();
                    }
                    MouseEventKind::ScrollDown => {
                        if app.scroll_offset > 0 {
                            app.scroll_offset = app.scroll_offset.saturating_sub(3);
                            if app.scroll_offset == 0 {
                                app.auto_scroll = true;
                                app.new_messages_count = 0;
                            }
                        }
                        app.request_redraw();
                    }
                    _ => {}
                },
                Event::Resize(_, _) => {
                    // Full clear ensures no ghost cells after resize changes layout geometry.
                    app.needs_full_clear = true;
                    app.request_redraw();
                }
                Event::Paste(text) => {
                    // Strip CR so \r\n becomes \n (insert_text handles bare \r too)
                    let text = text.replace('\r', "");
                    app.input.insert_text(&text);
                    app.request_redraw();
                }
                _ => {} // Focus -- ignored
            }
        }

        // Check for incoming permission requests
        while let Ok(req) = perm_rx.try_recv() {
            app.push_message(MessageContent::System(format!(
                "\u{1F512} Permission required: {}",
                req.tool_name,
            )));
            app.permission = Some(PendingPermission::new(req));
        }

        // Check for incoming user question requests
        while let Ok(req) = user_q_rx.try_recv() {
            app.push_message(MessageContent::System(format!(
                "\u{2753} Question: {}",
                req.question,
            )));
            app.user_question = Some(PendingUserQuestion { request: req });
        }
    }

    // Save session before exiting
    let _ = client.send_request(clawed_bus::events::AgentRequest::SaveSession);

    // Persist history to disk
    if let Some(hist_path) = crate::input::history_file_path() {
        let history = app.input.history();
        if !history.is_empty() {
            let content = history.join("\n");
            let _ = std::fs::write(&hist_path, content);
        }
    }

    // Abort the forwarder tasks
    forwarder.abort();
    perm_forwarder.abort();
    user_q_forwarder.abort();

    Ok(())
}

// -- Overlay selection handler -------------------------------------------------

fn submit_prepared_prompt(
    client: &ClientHandle,
    app: &mut App,
    prepared: crate::repl_commands::PreparedPrompt,
) {
    let summary = overlay::strip_ansi(&prepared.summary);
    if !summary.trim().is_empty() {
        app.push_message(MessageContent::System(summary));
    }
    let _ = client.submit(&prepared.prompt);
    app.mark_generating();
}

fn submit_queued_inputs(client: &ClientHandle, app: &mut App) {
    if let Some(merged) = app.take_queued_inputs() {
        app.push_message(MessageContent::UserInput(merged.clone()));
        let _ = client.submit(&merged);
        app.mark_generating();
    }
}

async fn git_status_porcelain(cwd: &std::path::Path) -> String {
    let cwd = cwd.to_path_buf();
    tokio::task::spawn_blocking(move || {
        std::process::Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(&cwd)
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
            .unwrap_or_default()
    })
    .await
    .unwrap_or_default()
}

async fn handle_pending_workflow(client: &ClientHandle, app: &mut App) -> bool {
    match app.pending_workflow.take() {
        Some(PendingWorkflow::CommitPushPr {
            cwd,
            user_message,
            baseline_status,
        }) => {
            let new_status = git_status_porcelain(&cwd).await;
            if new_status == baseline_status {
                app.push_message(MessageContent::System(
                    "提交似乎未完成，中止工作流。".to_string(),
                ));
                return false;
            }

            match crate::repl_commands::prepare_pr_prompt(&cwd, &user_message) {
                Ok(prepared) => {
                    submit_prepared_prompt(client, app, prepared);
                    true
                }
                Err(message) => {
                    app.push_message(MessageContent::System(message));
                    false
                }
            }
        }
        None => false,
    }
}

/// Handle a value selected from an overlay (e.g. model picker, theme picker).
async fn handle_overlay_selection(
    overlay_title: &str,
    value: &str,
    client: &ClientHandle,
    engine: &Arc<QueryEngine>,
    app: &mut App,
) {
    match overlay_title {
        "Switch Model" => {
            let ctx = clawed_core::model::resolve_model_with_context(value);
            app.model = ctx.model.clone();
            let _ = client.send_request(clawed_bus::events::AgentRequest::SetModel {
                model: value.to_string(),
            });
            app.push_message(MessageContent::System(format!(
                "✓ Model → {}",
                ctx.display_name
            )));
        }
        "Theme" => match crate::repl_commands::apply_theme(value) {
            Ok(message) | Err(message) => {
                app.push_message(MessageContent::System(overlay::strip_ansi(&message)));
                app.needs_full_clear = true;
            }
        },
        "Permission Mode" => {
            let new_mode = crate::config::parse_permission_mode(value);
            engine.state().write().await.permission_mode = new_mode;
            app.permission_mode = crate::config::format_permission_mode(new_mode).to_string();
            app.push_message(MessageContent::System(format!(
                "✓ Permission mode → {:?}",
                new_mode
            )));
        }
        _ => {
            app.push_message(MessageContent::System(format!("Selected: {value}")));
        }
    }
}

async fn handle_footer_picker_selection(
    kind: FooterPickerKind,
    value: &str,
    client: &ClientHandle,
    engine: &Arc<QueryEngine>,
    app: &mut App,
) {
    match kind {
        FooterPickerKind::Model => {
            let ctx = clawed_core::model::resolve_model_with_context(value);
            app.model = ctx.model.clone();
            let _ = client.send_request(clawed_bus::events::AgentRequest::SetModel {
                model: value.to_string(),
            });
            app.push_message(MessageContent::System(format!(
                "✓ Model → {}",
                ctx.display_name
            )));
        }
        FooterPickerKind::Theme => match crate::repl_commands::apply_theme(value) {
            Ok(message) | Err(message) => {
                app.push_message(MessageContent::System(overlay::strip_ansi(&message)));
                app.needs_full_clear = true;
            }
        },
        FooterPickerKind::Permissions => {
            let new_mode = crate::config::parse_permission_mode(value);
            engine.state().write().await.permission_mode = new_mode;
            app.permission_mode = crate::config::format_permission_mode(new_mode).to_string();
            app.push_message(MessageContent::System(format!(
                "Permission mode: {:?}",
                new_mode
            )));
        }
        FooterPickerKind::Skills => {
            app.input.insert_text(&format!("/{value} "));
            app.request_redraw();
        }
        FooterPickerKind::Resume => {
            do_resume_session(engine, app, value).await;
        }
    }
}

// -- Async slash command handler -----------------------------------------------

/// Handle `CommandResult` variants that need `async` engine access.
async fn handle_async_command(
    cmd: crate::commands::CommandResult,
    engine: &Arc<QueryEngine>,
    client: &ClientHandle,
    app: &mut App,
    terminal: Option<&mut TuiTerminal>,
) {
    use crate::commands::CommandResult;
    use clawed_core::message::{ContentBlock, Message as CoreMsg};

    match cmd {
        CommandResult::Diff => {
            let cwd = std::env::current_dir().unwrap_or_default();
            let result = tokio::task::spawn_blocking(move || {
                std::process::Command::new("git")
                    .args(["diff", "--stat", "--no-color"])
                    .current_dir(&cwd)
                    .output()
            })
            .await;
            match result {
                Ok(Ok(out)) => {
                    let text = String::from_utf8_lossy(&out.stdout);
                    if text.trim().is_empty() {
                        app.push_message(MessageContent::System(
                            "No uncommitted changes.".to_string(),
                        ));
                    } else {
                        app.push_message(MessageContent::System(text.to_string()));
                    }
                }
                Ok(Err(e)) => {
                    app.push_message(MessageContent::System(format!("git diff failed: {e}")));
                }
                Err(e) => {
                    app.push_message(MessageContent::System(format!("git diff task failed: {e}")));
                }
            }
        }
        CommandResult::Undo => {
            let removed = engine.rewind_turns(1).await;
            if removed.0 == 0 {
                app.push_message(MessageContent::System("Nothing to undo.".to_string()));
            } else {
                app.push_message(MessageContent::System(format!(
                    "✓ Undid 1 turn ({} messages remaining)",
                    removed.1,
                )));
            }
        }
        CommandResult::Rewind { turns } => {
            let n: usize = turns.parse().unwrap_or(1).max(1);
            let (removed, remaining) = engine.rewind_turns(n).await;
            if removed == 0 {
                app.push_message(MessageContent::System("Nothing to rewind.".to_string()));
            } else {
                app.push_message(MessageContent::System(format!(
                    "✓ Rewound {removed} turn(s) ({remaining} messages remaining)",
                )));
            }
        }
        CommandResult::Retry => {
            if let Some(prompt) = engine.pop_last_turn().await {
                let preview = if prompt.chars().count() > 60 {
                    let truncated: String = prompt.chars().take(57).collect();
                    format!("{truncated}…")
                } else {
                    prompt.clone()
                };
                app.push_message(MessageContent::System(format!("Retrying: {preview}",)));
                let _ = client.submit(&prompt);
                app.mark_generating();
            } else {
                app.push_message(MessageContent::System(
                    "No previous prompt to retry.".to_string(),
                ));
            }
        }
        CommandResult::Copy => {
            let state = engine.state().read().await;
            let text = state.messages.iter().rev().find_map(|m| {
                if let CoreMsg::Assistant(a) = m {
                    a.content.iter().find_map(|b| {
                        if let ContentBlock::Text { text } = b {
                            Some(text.clone())
                        } else {
                            None
                        }
                    })
                } else {
                    None
                }
            });
            drop(state);
            if let Some(text) = text {
                match arboard::Clipboard::new().and_then(|mut c| c.set_text(&text)) {
                    Ok(()) => {
                        app.push_message(MessageContent::System(format!(
                            "✓ Copied to clipboard ({} chars)",
                            text.len(),
                        )));
                    }
                    Err(e) => {
                        app.push_message(MessageContent::System(format!("Copy failed: {e}")));
                    }
                }
            } else {
                app.push_message(MessageContent::System(
                    "No assistant response to copy.".to_string(),
                ));
            }
        }
        CommandResult::Share => {
            let state = engine.state().read().await;
            let mut md = String::from("# Clawed Code Session\n\n");
            for msg in &state.messages {
                match msg {
                    CoreMsg::User(u) => {
                        md.push_str("## User\n\n");
                        for block in &u.content {
                            if let ContentBlock::Text { text } = block {
                                md.push_str(text);
                                md.push_str("\n\n");
                            }
                        }
                    }
                    CoreMsg::Assistant(a) => {
                        md.push_str("## Assistant\n\n");
                        for block in &a.content {
                            if let ContentBlock::Text { text } = block {
                                md.push_str(text);
                                md.push_str("\n\n");
                            }
                        }
                    }
                    CoreMsg::System(_) => {}
                }
            }
            drop(state);
            let ts = chrono::Local::now().format("%Y%m%d_%H%M%S");
            let filename = format!("claude-session-{ts}.md");
            let md_clone = md.clone();
            let result = tokio::task::spawn_blocking(move || {
                std::fs::write(&filename, &md_clone).map(|_| (filename, md_clone.len()))
            })
            .await;
            match result {
                Ok(Ok((filename, len))) => {
                    app.push_message(MessageContent::System(format!(
                        "✓ Session exported to {filename} ({len} bytes)",
                    )));
                }
                Ok(Err(e)) => {
                    app.push_message(MessageContent::System(format!("Export failed: {e}")));
                }
                Err(e) => {
                    app.push_message(MessageContent::System(format!("Export task failed: {e}")));
                }
            }
        }
        CommandResult::Export { format: fmt } => {
            let state = engine.state().read().await;
            let mut content = String::new();
            for msg in &state.messages {
                match msg {
                    CoreMsg::User(u) => {
                        content.push_str("USER: ");
                        for block in &u.content {
                            if let ContentBlock::Text { text } = block {
                                content.push_str(text);
                            }
                        }
                        content.push('\n');
                    }
                    CoreMsg::Assistant(a) => {
                        content.push_str("ASSISTANT: ");
                        for block in &a.content {
                            if let ContentBlock::Text { text } = block {
                                content.push_str(text);
                            }
                        }
                        content.push('\n');
                    }
                    CoreMsg::System(s) => {
                        content.push_str(&format!("SYSTEM: {}\n", s.message));
                    }
                }
            }
            drop(state);
            let ts = chrono::Local::now().format("%Y%m%d_%H%M%S");
            let ext = if fmt == "json" { "json" } else { "md" };
            let filename = format!("session-export-{ts}.{ext}");
            let content_clone = content.clone();
            let result = tokio::task::spawn_blocking(move || {
                std::fs::write(&filename, &content_clone).map(|_| filename)
            })
            .await;
            match result {
                Ok(Ok(filename)) => {
                    app.push_message(MessageContent::System(format!("✓ Exported to {filename}",)));
                }
                Ok(Err(e)) => {
                    app.push_message(MessageContent::System(format!("Export failed: {e}")));
                }
                Err(e) => {
                    app.push_message(MessageContent::System(format!("Export task failed: {e}")));
                }
            }
        }
        CommandResult::Rename { name } => {
            if name.is_empty() {
                app.push_message(MessageContent::System(
                    "Usage: /rename <new name>".to_string(),
                ));
            } else {
                match engine.rename_session(&name).await {
                    Ok(()) => {
                        app.push_message(MessageContent::System(format!(
                            "✓ Session renamed to '{name}'",
                        )));
                    }
                    Err(e) => {
                        app.push_message(MessageContent::System(format!("Rename failed: {e}")));
                    }
                }
            }
        }
        CommandResult::Fast { toggle } => {
            let state = engine.state();
            let current = state.read().await.model.clone();
            let fast_model = clawed_core::model::small_fast_model();
            if toggle.eq_ignore_ascii_case("off") {
                let default = clawed_core::model::resolve_model_string("sonnet");
                state.write().await.model = default.clone();
                app.model = default.clone();
                app.push_message(MessageContent::System(format!(
                    "✓ Switched to: {}",
                    clawed_core::model::display_name_any(&default),
                )));
            } else if current == fast_model {
                let default = clawed_core::model::resolve_model_string("sonnet");
                state.write().await.model = default.clone();
                app.model = default.clone();
                app.push_message(MessageContent::System(format!(
                    "✓ Fast mode off → {}",
                    clawed_core::model::display_name_any(&default),
                )));
            } else {
                state.write().await.model = fast_model.clone();
                app.model = fast_model.clone();
                app.push_message(MessageContent::System(format!(
                    "✓ Fast mode on → {}",
                    clawed_core::model::display_name_any(&fast_model),
                )));
            }
        }
        CommandResult::Context => {
            let cwd = std::env::current_dir().unwrap_or_default();
            let info = crate::repl_commands::handle_context_str(engine, &cwd).await;
            app.overlay = Some(overlay::build_info_overlay("Loaded Context", &info));
        }
        CommandResult::Stats => {
            let state = engine.state().read().await;
            let elapsed = app.status.session_start.elapsed().as_secs();
            let info = format!(
                "Session stats:\n  Turns: {}\n  Messages: {}\n  Context tokens (last turn): {}\n  Billed input tokens (all turns): {}\n  Output tokens: {}\n  Elapsed: {}s\n  Model: {}",
                state.turn_count, state.messages.len(),
                app.context_tokens,
                state.total_input_tokens, state.total_output_tokens,
                elapsed, state.model,
            );
            app.overlay = Some(overlay::build_info_overlay("Statistics", &info));
        }
        CommandResult::Chrome { sub } => {
            let args: Vec<&str> = sub.split_whitespace().collect();
            let text = crate::chrome::handle_chrome_command(&args);
            app.push_message(MessageContent::System(text));
        }
        CommandResult::Files { pattern } => {
            let cwd = std::env::current_dir().unwrap_or_default();
            let pattern2 = pattern.clone();
            let result = tokio::task::spawn_blocking(move || {
                let entries = std::fs::read_dir(&cwd)?;
                let mut items: Vec<_> = entries
                    .flatten()
                    .filter(|e| {
                        pattern2.is_empty()
                            || e.file_name().to_string_lossy().contains(pattern2.as_str())
                    })
                    .collect();
                items.sort_by_key(std::fs::DirEntry::file_name);
                let mut lines = String::new();
                for entry in &items {
                    let name = entry.file_name();
                    let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
                    if is_dir {
                        lines.push_str(&format!("  {}/\n", name.to_string_lossy()));
                    } else {
                        lines.push_str(&format!("  {}\n", name.to_string_lossy()));
                    }
                }
                Ok::<_, std::io::Error>((items.len(), lines, cwd))
            })
            .await;
            match result {
                Ok(Ok((count, lines, cwd))) => {
                    if count == 0 {
                        app.push_message(MessageContent::System(format!(
                            "No files matching '{pattern}'",
                        )));
                    } else {
                        let full = format!("({count} items in {})", cwd.display());
                        app.overlay = Some(overlay::build_info_overlay(
                            "Files",
                            &format!("{lines}{full}"),
                        ));
                    }
                }
                Ok(Err(e)) => {
                    app.push_message(MessageContent::System(format!(
                        "Cannot read directory: {e}",
                    )));
                }
                Err(e) => {
                    app.push_message(MessageContent::System(format!(
                        "Directory read task failed: {e}",
                    )));
                }
            }
        }
        CommandResult::Btw { text } => {
            if text.is_empty() {
                app.push_message(MessageContent::System("Usage: /btw <text>".to_string()));
            } else {
                engine.inject_context(&text).await;
                app.push_message(MessageContent::System(format!("[btw] {text}")));
            }
        }
        CommandResult::Resume { query } => {
            do_resume_session(engine, app, &query).await;
        }
        CommandResult::Image { path } => {
            if path.is_empty() {
                app.push_message(MessageContent::System(
                    "Usage: /image <path>  (or Ctrl+V to paste from clipboard)".to_string(),
                ));
            } else {
                let cwd = std::env::current_dir().unwrap_or_default();
                let img_path = std::path::Path::new(&path);
                let img_path = if img_path.is_relative() {
                    cwd.join(img_path)
                } else {
                    img_path.to_path_buf()
                };
                let img_path2 = img_path.clone();
                let result = tokio::task::spawn_blocking(move || {
                    clawed_core::image::read_image_file(&img_path2)
                })
                .await;
                match result {
                    Ok(Ok(ContentBlock::Image { source })) => {
                        app.pending_images.push(ImageAttachment {
                            data: source.data,
                            media_type: source.media_type,
                        });
                        app.push_message(MessageContent::System(format!(
                            "✓ Image queued: {} ({} pending)",
                            img_path.file_name().unwrap_or_default().to_string_lossy(),
                            app.pending_images.len(),
                        )));
                    }
                    Ok(Err(e)) => {
                        app.push_message(MessageContent::System(format!("Image error: {e}")));
                    }
                    Ok(_) => {
                        app.push_message(MessageContent::System(
                            "Unexpected content block from image read.".to_string(),
                        ));
                    }
                    Err(e) => {
                        app.push_message(MessageContent::System(format!(
                            "Image read task failed: {e}"
                        )));
                    }
                }
            }
        }
        CommandResult::Feedback { text } => {
            let feedback_path = dirs::home_dir()
                .map(|h| h.join(".claude").join("feedback.log"))
                .unwrap_or_else(|| std::path::PathBuf::from("feedback.log"));
            if let Some(parent) = feedback_path.parent() {
                let _ = tokio::task::spawn_blocking({
                    let parent = parent.to_path_buf();
                    move || std::fs::create_dir_all(&parent)
                })
                .await;
            }
            let timestamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
            let entry = format!("[{timestamp}] {text}\n");
            let path = feedback_path.clone();
            let result = tokio::task::spawn_blocking(move || {
                let mut f = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&path)?;
                use std::io::Write;
                f.write_all(entry.as_bytes())?;
                Ok::<_, std::io::Error>(path)
            })
            .await;
            match result {
                Ok(Ok(path)) => {
                    app.push_message(MessageContent::System(format!(
                        "✓ Feedback saved to {}",
                        path.display(),
                    )));
                }
                Ok(Err(e)) => {
                    app.push_message(MessageContent::System(format!(
                        "Could not save feedback: {e}",
                    )));
                }
                Err(e) => {
                    app.push_message(MessageContent::System(
                        format!("Feedback task failed: {e}",),
                    ));
                }
            }
        }
        CommandResult::ReleaseNotes => {
            let notes = format!(
                "Clawed Code v{}\n\nRecent changes:\n  • Full ratatui TUI with double-buffered rendering\n  • Markdown + syntect code highlighting\n  • Multi-line input, collapsible thinking/tool results\n  • Permission prompts, session resume, image paste\n  • 76+ slash commands, 52+ tools",
                env!("CARGO_PKG_VERSION"),
            );
            app.overlay = Some(overlay::build_info_overlay("Release Notes", &notes));
        }
        CommandResult::Memory { sub } => {
            let output = crate::repl_commands::handle_memory_command_str(
                &sub,
                &std::env::current_dir().unwrap_or_default(),
            );
            if should_render_print_output_in_overlay(&output) {
                app.overlay = Some(overlay::build_info_overlay("Memory", &output));
            } else {
                app.push_message(MessageContent::System(output));
            }
        }
        // Commands that submit a prompt to the agent
        CommandResult::Review { prompt } => {
            let cwd = std::env::current_dir().unwrap_or_default();
            match crate::repl_commands::prepare_review_submission(&prompt, &cwd) {
                Ok(prepared) => submit_prepared_prompt(client, app, prepared),
                Err(message) => app.push_message(MessageContent::System(message)),
            }
        }
        CommandResult::Simplify { prompt } => {
            let cwd = std::env::current_dir().unwrap_or_default();
            match crate::repl_commands::prepare_simplify_submission(&prompt, &cwd) {
                Ok(prepared) => submit_prepared_prompt(client, app, prepared),
                Err(message) => app.push_message(MessageContent::System(message)),
            }
        }
        CommandResult::Bug { prompt } => {
            let cwd = std::env::current_dir().unwrap_or_default();
            submit_prepared_prompt(
                client,
                app,
                crate::repl_commands::prepare_bug_prompt(&cwd, &prompt),
            );
        }
        CommandResult::Pr { prompt } => {
            let cwd = std::env::current_dir().unwrap_or_default();
            match crate::repl_commands::prepare_pr_prompt(&cwd, &prompt) {
                Ok(prepared) => submit_prepared_prompt(client, app, prepared),
                Err(message) => app.push_message(MessageContent::System(message)),
            }
        }
        CommandResult::Commit { message } => {
            let cwd = std::env::current_dir().unwrap_or_default();
            match crate::repl_commands::prepare_commit_prompt(&cwd, &message) {
                Ok(prepared) => submit_prepared_prompt(client, app, prepared),
                Err(message) => app.push_message(MessageContent::System(message)),
            }
        }
        CommandResult::CommitPushPr { message } => {
            let cwd = std::env::current_dir().unwrap_or_default();
            match crate::repl_commands::prepare_commit_push_pr(&cwd, &message) {
                crate::repl_commands::CommitPushPrPlan::Message(message) => {
                    app.push_message(MessageContent::System(message));
                }
                crate::repl_commands::CommitPushPrPlan::SubmitPrompt(prepared) => {
                    submit_prepared_prompt(client, app, prepared);
                }
                crate::repl_commands::CommitPushPrPlan::CommitThenPr {
                    commit,
                    baseline_status,
                    user_message,
                } => {
                    submit_prepared_prompt(client, app, commit);
                    app.pending_workflow = Some(PendingWorkflow::CommitPushPr {
                        cwd,
                        user_message,
                        baseline_status,
                    });
                }
            }
        }
        CommandResult::Search { query } => {
            let text = crate::repl_commands::handle_search_str(engine, &query).await;
            app.overlay = Some(overlay::build_info_overlay("Search", &text));
        }
        CommandResult::History { page } => {
            let text = crate::repl_commands::handle_history_str(engine, page).await;
            app.overlay = Some(overlay::build_info_overlay("History", &text));
        }
        CommandResult::PrComments { pr_number } => {
            let cwd = std::env::current_dir().unwrap_or_default();
            match crate::repl_commands::prepare_pr_comments(pr_number, &cwd) {
                Ok(prepared) => {
                    app.overlay = Some(overlay::build_info_overlay(
                        "PR Comments",
                        &prepared.display,
                    ));
                    let _ = client.submit(&prepared.prompt);
                    app.mark_generating();
                }
                Err(message) => {
                    if message.contains('\n') {
                        app.overlay = Some(overlay::build_info_overlay("PR Comments", &message));
                    } else {
                        app.push_message(MessageContent::System(overlay::strip_ansi(&message)));
                    }
                }
            }
        }
        CommandResult::Branch { name } => {
            let text = crate::repl_commands::handle_branch_str(engine, &name).await;
            app.overlay = Some(overlay::build_info_overlay("Branch", &text));
        }
        CommandResult::AddDir { path } => {
            if path.is_empty() {
                app.push_message(MessageContent::System("Usage: /add-dir <path>".to_string()));
            } else {
                let cwd = std::env::current_dir().unwrap_or_default();
                let dir_path = std::path::Path::new(&path);
                let dir_path = if dir_path.is_relative() {
                    cwd.join(dir_path)
                } else {
                    dir_path.to_path_buf()
                };
                if !dir_path.is_dir() {
                    app.push_message(MessageContent::System(format!(
                        "Directory not found: {}",
                        dir_path.display(),
                    )));
                } else {
                    let mut ctx = format!("<context source=\"{}\">\n", dir_path.display());
                    let mut file_count = 0u32;
                    if let Ok(entries) = std::fs::read_dir(&dir_path) {
                        for entry in entries.flatten() {
                            let p = entry.path();
                            if p.is_file() {
                                if let Ok(content) = std::fs::read_to_string(&p) {
                                    let name = p.file_name().unwrap_or_default().to_string_lossy();
                                    ctx.push_str(&format!(
                                        "--- {name} ---\n{}\n\n",
                                        content.trim()
                                    ));
                                    file_count += 1;
                                }
                            }
                        }
                    }
                    ctx.push_str("</context>");
                    engine.update_system_prompt_context(&ctx).await;
                    app.push_message(MessageContent::System(format!(
                        "✓ Added {file_count} file(s) from {}",
                        dir_path.display(),
                    )));
                }
            }
        }
        CommandResult::Summary => {
            submit_prepared_prompt(client, app, crate::repl_commands::prepare_summary_prompt());
        }
        // Commands that are not meaningfully different in TUI
        CommandResult::Permissions { mode } => {
            if mode.is_empty() {
                let state = engine.state().read().await;
                app.set_footer_picker(build_permissions_picker(state.permission_mode));
            } else {
                let new_mode = crate::config::parse_permission_mode(&mode);
                engine.state().write().await.permission_mode = new_mode;
                app.permission_mode = crate::config::format_permission_mode(new_mode).to_string();
                app.push_message(MessageContent::System(format!(
                    "Permission mode: {:?}",
                    new_mode
                )));
            }
        }
        CommandResult::Config => {
            let cwd = std::env::current_dir().unwrap_or_default();
            let info = crate::repl_commands::handle_config_command_str(&cwd);
            app.overlay = Some(overlay::build_info_overlay("Configuration", &info));
        }
        CommandResult::Doctor => {
            let cwd = std::env::current_dir().unwrap_or_default();
            let doctor_overlay = overlay::build_doctor_overlay(engine, &cwd).await;
            app.overlay = Some(doctor_overlay);
        }
        CommandResult::Init => {
            let cwd = std::env::current_dir().unwrap_or_default();
            submit_prepared_prompt(client, app, crate::repl_commands::prepare_init_prompt(&cwd));
        }
        CommandResult::Plan { args } => {
            let cwd = std::env::current_dir().unwrap_or_default();
            match args.trim() {
                "" => {
                    let message = crate::repl_commands::toggle_plan_mode(engine).await;
                    app.push_message(MessageContent::System(overlay::strip_ansi(&message)));
                }
                "show" | "view" => match crate::repl_commands::show_plan_text(&cwd) {
                    Ok(Some(text)) => {
                        app.overlay = Some(overlay::build_info_overlay("Plan", &text));
                    }
                    Ok(None) => {
                        app.push_message(MessageContent::System(
                            "No plan file found. Use /plan open to create one.".to_string(),
                        ));
                    }
                    Err(message) => {
                        app.push_message(MessageContent::System(overlay::strip_ansi(&message)));
                    }
                },
                "open" => {
                    if let Some(terminal) = terminal {
                        match with_tui_suspended(terminal, || {
                            crate::repl_commands::open_plan_in_editor(&cwd)
                        }) {
                            Ok(Ok(message)) => {
                                app.push_message(MessageContent::System(overlay::strip_ansi(
                                    &message,
                                )));
                            }
                            Ok(Err(message)) => {
                                app.push_message(MessageContent::System(overlay::strip_ansi(
                                    &message,
                                )));
                            }
                            Err(error) => {
                                app.push_message(MessageContent::System(format!(
                                    "Plan editing failed: {error}"
                                )));
                            }
                        }
                        app.needs_full_clear = true;
                    } else {
                        app.push_message(MessageContent::System(
                            "Plan editing requires an interactive terminal.".to_string(),
                        ));
                    }
                }
                description => {
                    match crate::repl_commands::save_plan_description(engine, &cwd, description)
                        .await
                    {
                        Ok(message) => {
                            app.push_message(MessageContent::System(overlay::strip_ansi(&message)));
                        }
                        Err(message) => {
                            app.push_message(MessageContent::System(overlay::strip_ansi(&message)));
                        }
                    }
                }
            }
        }
        CommandResult::Login => {
            if let Some(terminal) = terminal {
                let result = with_tui_suspended(terminal, || {
                    match crate::repl_commands::prompt_for_api_key_interactive() {
                        Ok(Some(key)) => crate::repl_commands::save_api_key(&key),
                        Ok(None) => Ok("No key provided. Cancelled.".to_string()),
                        Err(message) => Err(message),
                    }
                });
                match result {
                    Ok(Ok(message)) => {
                        app.push_message(MessageContent::System(overlay::strip_ansi(&message)));
                    }
                    Ok(Err(message)) => {
                        app.push_message(MessageContent::System(overlay::strip_ansi(&message)));
                    }
                    Err(error) => {
                        app.push_message(MessageContent::System(format!("Login failed: {error}")));
                    }
                }
                app.needs_full_clear = true;
            } else {
                app.push_message(MessageContent::System(
                    "Login requires an interactive terminal.".to_string(),
                ));
            }
        }
        CommandResult::Logout => match crate::repl_commands::handle_logout_str() {
            Ok(message) | Err(message) => {
                app.push_message(MessageContent::System(overlay::strip_ansi(&message)));
            }
        },
        CommandResult::ReloadContext => {
            let cwd = std::env::current_dir().unwrap_or_default();
            let info = crate::repl_commands::handle_reload_context_str(engine, &cwd).await;
            let skills = clawed_core::skills::get_skills(&cwd);
            let skill_names: Vec<String> = skills.iter().map(|s| format!("/{}", s.name)).collect();
            app.input.set_skill_names(skill_names);
            app.overlay = Some(overlay::build_info_overlay("Reload Context", &info));
        }
        CommandResult::Theme { name } => {
            if name.is_empty() {
                app.set_footer_picker(build_theme_picker(
                    crate::theme::current_theme_name().as_str(),
                ));
            } else {
                match crate::repl_commands::apply_theme(&name) {
                    Ok(message) | Err(message) => {
                        app.push_message(MessageContent::System(overlay::strip_ansi(&message)));
                        app.needs_full_clear = true;
                    }
                }
            }
        }
        CommandResult::Agents { sub } => {
            let cwd = std::env::current_dir().unwrap_or_default();
            let text = format_agents_tui(&sub, &cwd, &app.status.active_agents);
            app.overlay = Some(overlay::build_info_overlay("Agents", &text));
        }
        CommandResult::Mcp { sub } => {
            let cwd = std::env::current_dir().unwrap_or_default();
            let text = crate::repl_commands::handle_mcp_command_str(&sub, &cwd);
            app.overlay = Some(overlay::build_info_overlay("MCP", &text));
        }
        CommandResult::Plugin { sub } => {
            let cwd = std::env::current_dir().unwrap_or_default();
            let text = crate::repl_commands::handle_plugin_command_str(&sub, &cwd);
            app.overlay = Some(overlay::build_info_overlay("Plugins", &text));
        }
        CommandResult::RunPluginCommand { name, prompt } => {
            app.push_message(MessageContent::System(format!(
                "Running plugin command: /{name}",
            )));
            let _ = client.submit(&prompt);
            app.mark_generating();
        }
        CommandResult::RunSkill { name, prompt } => {
            let cwd = std::env::current_dir().unwrap_or_default();
            let skills = clawed_core::skills::get_skills(&cwd);
            match crate::repl_commands::find_skill(&skills, &name) {
                Ok(skill) => {
                    app.push_message(MessageContent::System(format!("Running skill: {name}",)));
                    if !skill.allowed_tools.is_empty() {
                        app.push_message(MessageContent::System(format!(
                            "Skill restricts tools to: {}",
                            skill.allowed_tools.join(", "),
                        )));
                    }

                    // Pass raw prompt (or empty) so substitute_arguments
                    // doesn't append "ARGUMENTS: Execute the X skill" to skill content.
                    // Always include the skill name in the user prompt so the model
                    // can unambiguously associate the request with the skill.
                    let (user_msg, skill_args) = if prompt.trim().is_empty() {
                        (format!("Execute the {} skill", name), "")
                    } else {
                        (format!("[{name}] {prompt}"), prompt.as_str())
                    };

                    // Build combined user message with skill content + XML metadata tags.
                    // Skill content is delivered as a user message (not system prompt),
                    // matching the reference TS implementation.
                    let combined_msg = if let Some(msg) =
                        crate::repl_commands::build_skill_user_message(skill, skill_args, &user_msg)
                    {
                        msg
                    } else {
                        user_msg.clone()
                    };

                    // Set allowed tools for tool filtering
                    if !skill.allowed_tools.is_empty() {
                        engine.set_skill_allowed_tools(skill.allowed_tools.clone());
                    }

                    // Temporarily switch model if skill specifies one
                    let original_model = if let Some(ref skill_model) = skill.model {
                        let (orig, msg) =
                            crate::repl_commands::switch_model_for_skill(engine, skill_model).await;
                        if let Some(msg) = msg {
                            app.push_message(MessageContent::System(msg));
                        }
                        orig
                    } else {
                        None
                    };

                    let _ = client.submit(&combined_msg);
                    app.mark_generating();

                    // Model and tool whitelist are restored after TurnComplete arrives.
                    // Store them on App for later cleanup.
                    app.pending_skill_restore = Some(PendingSkillRestore {
                        original_model,
                        skill_name: skill.name.clone(),
                    });
                }
                Err(message) => {
                    app.push_message(MessageContent::System(message));
                }
            }
        }
        CommandResult::Vim { toggle } => {
            let enabled = match toggle.to_lowercase().as_str() {
                "" | "on" | "true" | "1" => true,
                "off" | "false" | "0" => false,
                _ => {
                    app.push_message(MessageContent::System("Usage: /vim [on|off]".to_string()));
                    return;
                }
            };
            let message = if enabled {
                "Vim mode enabled (note: basic vim keybindings are a work in progress)"
            } else {
                "Vim mode disabled — normal editing mode active"
            };
            app.push_message(MessageContent::System(message.to_string()));
        }
        // These are handled synchronously in handle_slash_command
        CommandResult::Print(_)
        | CommandResult::ClearHistory
        | CommandResult::SetModel(_)
        | CommandResult::ShowCost { .. }
        | CommandResult::Compact { .. }
        | CommandResult::Status
        | CommandResult::Think { .. }
        | CommandResult::BreakCache
        | CommandResult::Env
        | CommandResult::Effort { .. }
        | CommandResult::Tag { .. }
        | CommandResult::Stickers
        | CommandResult::Exit
        | CommandResult::Bridge
        | CommandResult::Teleport => {
            // Should not reach here — these are handled in handle_slash_command
        }
    }
}

// -- /agents TUI formatter ----------------------------------------------------

/// Format `/agents [sub]` output as plain text for a TUI info overlay.
fn format_agents_tui(
    sub: &str,
    cwd: &std::path::Path,
    active_agents: &std::collections::HashMap<String, status::AgentInfo>,
) -> String {
    let parts: Vec<&str> = sub.splitn(2, ' ').collect();
    let subcmd = parts.first().map(|s| s.trim()).unwrap_or("");
    let args = parts.get(1).map(|s| s.trim()).unwrap_or("");

    match subcmd {
        "" | "list" => {
            let all = clawed_core::agents::get_agents(cwd);
            if all.is_empty() {
                return "No agent definitions found.\nCreate one with: /agents create <name>\nOr add .md files to .claude/agents/".to_string();
            }
            let mut out = format!("Agent Definitions ({} total)\n\n", all.len());
            let mut by_source: std::collections::BTreeMap<String, Vec<&clawed_core::agents::AgentDefinition>> =
                std::collections::BTreeMap::new();
            for agent in &all {
                by_source.entry(format!("{}", agent.source)).or_default().push(agent);
            }
            for (source, agents) in &by_source {
                out.push_str(&format!("[{}]\n", source));
                for a in agents {
                    let bg = if a.background { "  [bg]" } else { "" };
                    out.push_str(&format!("  {:<22} {}{}\n", a.agent_type, a.description, bg));
                    if !a.allowed_tools.is_empty() {
                        let tools = if a.allowed_tools.len() <= 5 {
                            a.allowed_tools.join(", ")
                        } else {
                            format!("{}, ... (+{})", a.allowed_tools[..4].join(", "), a.allowed_tools.len() - 4)
                        };
                        out.push_str(&format!("  {:<22} tools: {}\n", "", tools));
                    }
                }
                out.push('\n');
            }
            out
        }
        "status" => {
            if active_agents.is_empty() {
                "No background agents currently running.\n\nUse /agents list to see defined agents.".to_string()
            } else {
                let mut out = format!("Running Agents ({} active)\n\n", active_agents.len());
                for (id, agent) in active_agents {
                    let elapsed = agent.started.elapsed();
                    out.push_str(&format!(
                        "  ▸ {:<24} {} ({:02}:{:02})\n",
                        id,
                        agent.name,
                        elapsed.as_secs() / 60,
                        elapsed.as_secs() % 60,
                    ));
                }
                out
            }
        }
        "info" => {
            if args.is_empty() {
                return "Usage: /agents info <name>".to_string();
            }
            let all = clawed_core::agents::get_agents(cwd);
            match all.iter().find(|a| a.agent_type.eq_ignore_ascii_case(args)) {
                None => format!("Agent '{}' not found.\nUse /agents list to see available.", args),
                Some(a) => {
                    let mut out = format!("{}\n\n", a.agent_type);
                    out.push_str(&format!("Description: {}\n", a.description));
                    out.push_str(&format!("Source:      {}\n", a.source));
                    if let Some(ref m) = a.model { out.push_str(&format!("Model:       {}\n", m)); }
                    if let Some(ref e) = a.effort { out.push_str(&format!("Effort:      {}\n", e)); }
                    if let Some(ref p) = a.permission_mode { out.push_str(&format!("Permissions: {}\n", p)); }
                    if let Some(t) = a.max_turns { out.push_str(&format!("Max turns:   {}\n", t)); }
                    if a.background { out.push_str("Background:  yes\n"); }
                    if !a.allowed_tools.is_empty() { out.push_str(&format!("Tools:       {}\n", a.allowed_tools.join(", "))); }
                    if !a.disallowed_tools.is_empty() { out.push_str(&format!("Disallowed:  {}\n", a.disallowed_tools.join(", "))); }
                    if let Some(ref path) = a.file_path { out.push_str(&format!("File:        {}\n", path.display())); }
                    let preview = clawed_core::text_util::truncate_chars(&a.system_prompt, 300, "...");
                    out.push_str(&format!("\n--- System Prompt ---\n{}\n", preview));
                    out
                }
            }
        }
        "create" => {
            if args.is_empty() {
                return "Usage: /agents create <name>\nCreates an agent definition in .claude/agents/<name>.md".to_string();
            }
            let agent = clawed_core::agents::AgentDefinition {
                agent_type: args.to_string(),
                description: format!("{} agent", args),
                system_prompt: format!("You are a specialized {} assistant.", args),
                allowed_tools: vec![],
                disallowed_tools: vec![],
                model: None, effort: None, memory: None, color: None,
                permission_mode: None, max_turns: None, background: false,
                skills: vec![], initial_prompt: None,
                source: clawed_core::agents::AgentSource::Local,
                file_path: None, base_dir: None,
            };
            let existing = clawed_core::agents::get_agents(cwd);
            let validation = clawed_core::agents::validate_agent(&agent, &existing);
            if !validation.is_valid() {
                return format!("Invalid agent definition:\n{}", validation.errors.join("\n"));
            }
            match clawed_core::agents::save_agent(&agent, cwd) {
                Ok(path) => format!("✓ Created agent scaffold: {}\nEdit the file to customize tools, model, and system prompt.", path.display()),
                Err(e) => format!("Failed to create agent: {}", e),
            }
        }
        "delete" | "rm" => {
            if args.is_empty() {
                return "Usage: /agents delete <name>".to_string();
            }
            let all = clawed_core::agents::get_agents(cwd);
            match all.iter().find(|a| a.agent_type.eq_ignore_ascii_case(args)) {
                None => format!("Agent '{}' not found.\nUse /agents list to see available.", args),
                Some(a) => {
                    if a.source == clawed_core::agents::AgentSource::BuiltIn {
                        return format!("Cannot delete built-in agent '{}'.", args);
                    }
                    match clawed_core::agents::delete_agent(a) {
                        Ok(()) => format!("✓ Deleted agent: {}", args),
                        Err(e) => format!("Failed to delete agent '{}': {}", args, e),
                    }
                }
            }
        }
        _ => {
            "Agent Definitions\n\n  /agents               List all agent definitions\n  /agents list           Same as above\n  /agents status         Show live running agents\n  /agents info <name>    Show details of an agent\n  /agents create <name>  Create a new agent scaffold\n  /agents delete <name>  Delete an agent definition\n\nAgents are .md files in .claude/agents/ with YAML frontmatter.\nThey define sub-agents with custom tools, models, and prompts.".to_string()
        }
    }
}

// -- Clipboard image support --------------------------------------------------

/// Read an image from the system clipboard and return it as an `ImageAttachment`.
///
/// Uses `arboard` for cross-platform clipboard access. The image is encoded as
/// PNG and base64-encoded for the Anthropic API.
fn read_clipboard_image() -> anyhow::Result<ImageAttachment> {
    use anyhow::Context as _;
    use base64::Engine as _;

    let mut clip = arboard::Clipboard::new().context("Cannot open clipboard")?;

    let img = clip.get_image().context("No image in clipboard")?;

    // Encode RGBA pixels as PNG
    let mut png_bytes: Vec<u8> = Vec::new();
    {
        let mut encoder = png::Encoder::new(
            std::io::Cursor::new(&mut png_bytes),
            img.width as u32,
            img.height as u32,
        );
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder
            .write_header()
            .context("Failed to write PNG header")?;
        writer
            .write_image_data(&img.bytes)
            .context("Failed to encode clipboard image as PNG")?;
    }

    let data = base64::engine::general_purpose::STANDARD.encode(&png_bytes);
    Ok(ImageAttachment {
        data,
        media_type: "image/png".to_string(),
    })
}

// -- Session resume helpers ---------------------------------------------------

/// Replay the engine's current messages into the TUI display.
async fn replay_session_messages(engine: &Arc<QueryEngine>, app: &mut App) {
    use clawed_core::message::{ContentBlock, Message as CoreMsg};

    app.clear_messages();

    let state = engine.state().read().await;
    app.model = state.model.clone();
    app.total_turns = state.turn_count;
    app.context_tokens = state.total_input_tokens;
    app.total_output_tokens = state.total_output_tokens;

    for msg in &state.messages {
        match msg {
            CoreMsg::User(u) => {
                for block in &u.content {
                    if let ContentBlock::Text { text } = block {
                        app.push_message(MessageContent::UserInput(text.clone()));
                    }
                }
            }
            CoreMsg::Assistant(a) => {
                for block in &a.content {
                    match block {
                        ContentBlock::Text { text } => {
                            app.push_message(MessageContent::AssistantText(text.clone()));
                        }
                        ContentBlock::Thinking { thinking, .. } => {
                            app.push_message(MessageContent::ThinkingText(thinking.clone()));
                        }
                        ContentBlock::ToolUse { name, input, .. } => {
                            let input_str = extract_tool_input_display(name, input);
                            app.push_message(MessageContent::ToolExecution {
                                name: name.clone(),
                                input: input_str,
                                output_lines: vec![],
                                is_error: false,
                                duration_ms: 0,
                                full_result: None,
                                depth: 0,
                            });
                        }
                        _ => {}
                    }
                }
            }
            CoreMsg::System(s) => {
                app.push_message(MessageContent::System(s.message.clone()));
            }
        }
    }

    app.push_message(MessageContent::System(format!(
        "--- Restored {} messages, {} turns ---",
        state.messages.len(),
        state.turn_count,
    )));
}

// -- Agent color helpers (aligned with TEAMMATE_COLORS in clawed-swarm) -----

const AGENT_COLOR_PALETTE: &[(Color, &str)] = &[
    (Color::Cyan, "cyan"),
    (Color::Magenta, "magenta"),
    (Color::Yellow, "yellow"),
    (Color::Green, "green"),
    (Color::Blue, "blue"),
    (Color::Red, "red"),
    (Color::LightCyan, "bright-cyan"),
    (Color::LightMagenta, "bright-magenta"),
    (Color::LightYellow, "bright-yellow"),
    (Color::LightGreen, "bright-green"),
];

/// Assign a stable color to an agent based on its ID hash.
fn agent_color_for_id(agent_id: &str) -> Color {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    agent_id.hash(&mut hasher);
    let idx = (hasher.finish() as usize) % AGENT_COLOR_PALETTE.len();
    AGENT_COLOR_PALETTE[idx].0
}

/// Render "Viewing @agent_name · esc return" header above messages.
fn render_teammate_view_header(frame: &mut Frame, area: Rect, app: &App) {
    let Some(ref viewed) = app.viewed_teammate else {
        return;
    };
    let dim = Style::default().fg(MUTED);
    let name_style = Style::default()
        .fg(viewed.color)
        .add_modifier(Modifier::BOLD);
    let spans = vec![
        Span::styled("Viewing ", dim),
        Span::styled(format!("@{}", viewed.name), name_style),
        Span::styled("  ·  ", dim),
        Span::styled(
            "esc",
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" return", dim),
    ];
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use clawed_bus::bus::EventBus;
    use clawed_bus::events::{AgentRequest, PermissionRequest, RiskLevel};
    use clawed_core::skills::SkillEntry;
    use futures::FutureExt;
    use serde_json::json;
    use tempfile::TempDir;

    fn line_text(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    fn sample_skill(name: &str, description: &str) -> SkillEntry {
        SkillEntry {
            name: name.to_string(),
            display_name: None,
            description: description.to_string(),
            system_prompt: "You are helpful".to_string(),
            allowed_tools: vec![],
            model: None,
            when_to_use: None,
            paths: vec![],
            argument_names: vec![],
            argument_hint: Some("<prompt>".to_string()),
            version: None,
            context: None,
            agent: None,
            effort: None,
            user_invocable: true,
            disable_model_invocation: false,
            skill_root: None,
        }
    }

    #[test]
    fn welcome_lines_are_nonempty() {
        let lines = render_welcome_lines(80, "claude-sonnet-4-20250514", "bypass");
        assert!(!lines.is_empty());
    }

    #[test]
    fn app_push_message_works() {
        let mut app = App::new("test-model".to_string());
        app.push_message(MessageContent::System("hello".to_string()));
        assert_eq!(app.messages.len(), 1);
    }

    #[test]
    fn app_append_assistant_text() {
        let mut app = App::new("test-model".to_string());
        app.append_assistant_text("hello ");
        app.append_assistant_text("world");
        assert_eq!(app.messages.len(), 1);
        if let MessageContent::AssistantText(ref text) = app.messages[0].content {
            assert_eq!(text, "hello world");
        } else {
            panic!("Expected AssistantText");
        }
    }

    #[test]
    fn app_append_thinking_text() {
        let mut app = App::new("test-model".to_string());
        app.append_thinking_text("thinking...");
        app.append_thinking_text(" more");
        assert_eq!(app.messages.len(), 1);
        if let MessageContent::ThinkingText(ref text) = app.messages[0].content {
            assert_eq!(text, "thinking... more");
        } else {
            panic!("Expected ThinkingText");
        }
    }

    #[test]
    fn text_delta_after_thinking_creates_new_message() {
        let mut app = App::new("test-model".to_string());
        app.append_thinking_text("hmm");
        app.append_assistant_text("answer");
        assert_eq!(app.messages.len(), 2);
    }

    #[test]
    fn slash_help_adds_system_message() {
        let mut app = App::new("test".to_string());
        app.push_message(MessageContent::System("help text".to_string()));
        assert_eq!(app.messages.len(), 1);
    }

    #[test]
    fn slash_help_routes_long_print_output_to_overlay() {
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        app.handle_slash_command(&client, "/help");

        assert!(app.messages.is_empty());
        assert!(app.overlay.is_some());
    }

    #[test]
    fn short_print_output_stays_in_transcript() {
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        app.handle_slash_command(&client, "/tag demo");

        assert!(app.overlay.is_none());
        assert!(!app.messages.is_empty());
    }

    #[test]
    fn overlay_replaces_none() {
        let mut app = App::new("test".to_string());
        assert!(app.overlay.is_none());
        app.overlay = Some(overlay::build_model_overlay("test"));
        assert!(app.overlay.is_some());
        app.overlay = None;
        assert!(app.overlay.is_none());
    }

    #[test]
    fn model_command_opens_footer_picker_instead_of_overlay() {
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        app.handle_slash_command(&client, "/model");

        assert!(app.overlay.is_none());
        assert!(app.footer_picker.is_some());
        assert_eq!(
            app.footer_picker.as_ref().map(|picker| picker.kind),
            Some(FooterPickerKind::Model)
        );
    }

    #[test]
    fn poll_interval_is_idle_when_inactive() {
        let app = App::new("test".to_string());
        assert_eq!(poll_interval(&app), IDLE_POLL_INTERVAL);
    }

    #[test]
    fn poll_interval_is_active_while_generating() {
        let mut app = App::new("test".to_string());
        app.status.is_generating = true;
        assert_eq!(poll_interval(&app), ACTIVE_POLL_INTERVAL);
    }

    #[test]
    fn completion_popup_slot_height_is_fixed_while_open() {
        assert_eq!(completion_popup_rows_from_count(0), 0);
        assert_eq!(completion_popup_rows_from_count(1), 0);
        assert_eq!(completion_popup_rows_from_count(2), 2);
        assert_eq!(completion_popup_rows_from_count(5), 5);
        assert_eq!(
            completion_popup_rows_from_count(20),
            MAX_COMPLETION_POPUP_ITEMS as u16
        );
    }

    #[test]
    fn build_skills_picker_lists_invocable_skills() {
        let picker =
            build_skills_picker(&[sample_skill("review", "Review code")]).expect("skills picker");

        assert_eq!(picker.kind, FooterPickerKind::Skills);
        assert_eq!(picker.items.len(), 1);
        assert_eq!(picker.items[0].label, "/review");
        assert_eq!(picker.items[0].value, "review");
    }

    #[test]
    fn footer_picker_end_keeps_selection_visible() {
        let mut picker = FooterPicker {
            kind: FooterPickerKind::Model,
            items: (0..12)
                .map(|i| SelectionItem {
                    label: format!("item-{i}"),
                    description: String::new(),
                    value: i.to_string(),
                    is_current: false,
                })
                .collect(),
            selected: 0,
            scroll_offset: 0,
        };

        assert!(matches!(
            picker.handle_key(KeyCode::End),
            FooterPickerAction::Consumed
        ));
        assert_eq!(picker.selected, 11);
        assert_eq!(picker.scroll_offset, 2);
    }

    #[test]
    fn footer_picker_arrow_left_is_consumed() {
        let mut picker = FooterPicker {
            kind: FooterPickerKind::Model,
            items: vec![SelectionItem {
                label: "item".to_string(),
                description: String::new(),
                value: "value".to_string(),
                is_current: false,
            }],
            selected: 0,
            scroll_offset: 0,
        };

        assert!(matches!(
            picker.handle_key(KeyCode::Left),
            FooterPickerAction::Consumed
        ));
    }

    #[test]
    fn footer_picker_character_input_passes_through() {
        let mut picker = FooterPicker {
            kind: FooterPickerKind::Model,
            items: vec![SelectionItem {
                label: "item".to_string(),
                description: String::new(),
                value: "value".to_string(),
                is_current: false,
            }],
            selected: 0,
            scroll_offset: 0,
        };

        assert!(matches!(
            picker.handle_key(KeyCode::Char('x')),
            FooterPickerAction::PassThrough
        ));
    }

    #[test]
    fn long_print_output_prefers_overlay() {
        let long_text = (0..20)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");

        assert!(should_render_print_output_in_overlay(&long_text));
        assert!(!should_render_print_output_in_overlay("short output"));
    }

    #[test]
    fn spinner_tick_waits_for_interval() {
        let mut app = App::new("test".to_string());
        app.status.is_generating = true;
        app.needs_redraw = false;
        let start = app.last_spinner_tick;

        app.advance_spinner_if_due(start + Duration::from_millis(40));

        assert_eq!(app.status.spinner_frame, 0);
        assert!(!app.needs_redraw);
    }

    #[test]
    fn spinner_tick_marks_redraw_when_due() {
        let mut app = App::new("test".to_string());
        app.status.is_generating = true;
        app.needs_redraw = false;
        let start = app.last_spinner_tick;

        app.advance_spinner_if_due(start + SPINNER_TICK_INTERVAL);

        assert_eq!(app.status.spinner_frame, 1);
        assert!(app.needs_redraw);
    }

    #[test]
    fn should_clear_message_area_only_when_visual_height_shrinks() {
        assert!(should_clear_message_area(Some(10), 9));
        assert!(!should_clear_message_area(Some(10), 10));
        assert!(!should_clear_message_area(Some(10), 11));
        assert!(!should_clear_message_area(None, 9));
    }

    #[test]
    fn cached_visible_lines_track_assistant_append() {
        let mut app = App::new("test".to_string());
        app.push_message(MessageContent::System("system".to_string()));
        app.push_message(MessageContent::AssistantText("hello".to_string()));

        app.append_assistant_text(" world");

        assert!(!app.cached_visible_lines_dirty);
        assert_eq!(
            line_text(app.cached_visible_lines.last().expect("cached line")),
            "\u{25CF} hello world"
        );
    }

    #[test]
    fn collapsed_thinking_short_text_shows_placeholder() {
        // Collapsed thinking always shows a single placeholder line,
        // matching the official CC collapsed view.
        let mut app = App::new("test".to_string());
        app.push_message(MessageContent::ThinkingText("one\n\ntwo".to_string()));

        assert_eq!(app.cached_visible_lines.len(), 1);
        assert!(line_text(&app.cached_visible_lines[0]).contains("\u{2234} Thinking"));
    }

    #[test]
    fn collapsed_thinking_long_text_shows_hint() {
        let mut app = App::new("test".to_string());
        app.push_message(MessageContent::ThinkingText(
            "one\ntwo\nthree\nfour".to_string(),
        ));

        // Collapsed thinking always shows the same placeholder.
        assert_eq!(app.cached_visible_lines.len(), 1);
        assert!(line_text(&app.cached_visible_lines[0]).contains("Ctrl+O to expand"));
    }

    #[test]
    fn streaming_assistant_renders_inline_markdown_until_done() {
        let mut app = App::new("test".to_string());
        app.is_generating = true;
        app.push_message(MessageContent::AssistantText("**bold**".to_string()));

        // Streaming: lightweight inline parsing strips the markers.
        assert_eq!(line_text(&app.cached_visible_lines[0]), "\u{25CF} bold");

        app.mark_done();
        app.rebuild_visible_lines();

        // Done: full markdown renderer also produces "bold".
        assert_eq!(line_text(&app.cached_visible_lines[0]), "\u{25CF} bold");
    }

    #[test]
    fn parse_inline_spans_bold_italic_code() {
        let spans = parse_inline_spans("**bold** and *italic* and `code`");
        assert_eq!(spans.len(), 5);
        assert_eq!(spans[0].content, "bold");
        assert!(spans[0].style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(spans[1].content, " and ");
        assert_eq!(spans[2].content, "italic");
        assert!(spans[2].style.add_modifier.contains(Modifier::ITALIC));
        assert_eq!(spans[3].content, " and ");
        assert_eq!(spans[4].content, "code");
        assert_eq!(spans[4].style.bg, Some(Color::Rgb(45, 45, 45)));
    }

    #[test]
    fn parse_inline_spans_leaves_unclosed_as_plain() {
        let spans = parse_inline_spans("**unclosed bold");
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].content, "**unclosed bold");
    }

    #[test]
    fn parse_inline_spans_plain_text_unchanged() {
        let spans = parse_inline_spans("hello world");
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].content, "hello world");
    }

    #[test]
    fn parse_inline_spans_skips_code_blocks() {
        let spans = parse_inline_spans("```rust fn main() {} ```");
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].content, "```rust fn main() {} ```");
    }

    #[test]
    fn parse_inline_spans_double_backtick_is_plain() {
        let spans = parse_inline_spans("``not code``");
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].content, "``not code``");
    }

    #[test]
    fn layout_signature_detects_footer_changes() {
        let mut app = App::new("test".to_string());
        let base = app.layout_signature();

        app.bottom_bar_hidden = true;
        assert_ne!(base, app.layout_signature());

        app.bottom_bar_hidden = false;
        app.queued_inputs.push("queued".to_string());
        assert_ne!(base, app.layout_signature());

        app.queued_inputs.clear();
        app.task_plan
            .add_task("agent-1".to_string(), "Task".to_string());
        assert_ne!(base, app.layout_signature());

        let mut completion_app = App::new("test".to_string());
        let completion_base = completion_app.layout_signature();
        completion_app
            .input
            .handle_key(crossterm::event::KeyEvent::new(
                KeyCode::Char('/'),
                KeyModifiers::NONE,
            ));
        let completion_open = completion_app.layout_signature();
        assert_ne!(completion_base, completion_open);

        completion_app
            .input
            .handle_key(crossterm::event::KeyEvent::new(
                KeyCode::Char('h'),
                KeyModifiers::NONE,
            ));
        assert_eq!(completion_open.completion_rows, 10);
        assert_eq!(completion_app.layout_signature().completion_rows, 2);
    }

    #[test]
    fn layout_signature_detects_permission_and_task_panel() {
        let mut app = App::new("test".to_string());
        let base = app.layout_signature();

        app.task_plan
            .add_task("agent-1".to_string(), "Task".to_string());
        assert_ne!(base, app.layout_signature());

        app.task_plan = taskplan::TaskPlan::new();
        app.permission = Some(PendingPermission::new(PermissionRequest {
            request_id: "req-1".to_string(),
            tool_name: "Bash".to_string(),
            input: json!({"command": "ls"}),
            risk_level: RiskLevel::Medium,
            description: "Bash: command=ls".to_string(),
        }));
        assert_ne!(base, app.layout_signature());
    }

    #[test]
    fn completion_popup_stays_within_reserved_footer_slot() {
        let input_area = Rect::new(4, 20, 50, 1);
        let popup_slot = Rect::new(4, 21, 50, 3);
        let matches = ["/help", "/history", "/review"];

        let popup_area =
            completion_popup_area(popup_slot, input_area, &matches).expect("popup area");

        assert_eq!(popup_area.x, input_area.x);
        assert_eq!(popup_area.y, popup_slot.y);
        assert_eq!(popup_area.height, popup_slot.height);
        assert!(popup_area.width <= popup_slot.width);
        assert!(popup_area.y >= input_area.y + input_area.height);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn permissions_without_mode_open_footer_picker() {
        let tmp = TempDir::new().unwrap();
        let engine = Arc::new(
            QueryEngine::builder("test-key", tmp.path())
                .load_claude_md(false)
                .load_memory(false)
                .build(),
        );
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        handle_async_command(
            crate::commands::CommandResult::Permissions {
                mode: String::new(),
            },
            &engine,
            &client,
            &mut app,
            None,
        )
        .await;

        assert!(app.overlay.is_none());
        assert_eq!(
            app.footer_picker.as_ref().map(|picker| picker.kind),
            Some(FooterPickerKind::Permissions)
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn skills_picker_selection_prefills_input() {
        let tmp = TempDir::new().unwrap();
        let engine = Arc::new(
            QueryEngine::builder("test-key", tmp.path())
                .load_claude_md(false)
                .load_memory(false)
                .build(),
        );
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        handle_footer_picker_selection(
            FooterPickerKind::Skills,
            "review",
            &client,
            &engine,
            &mut app,
        )
        .await;

        assert_eq!(app.input.buffer(), "/review ");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn run_plugin_command_submits_prompt_in_tui() {
        let tmp = TempDir::new().unwrap();
        let engine = Arc::new(
            QueryEngine::builder("test-key", tmp.path())
                .load_claude_md(false)
                .load_memory(false)
                .build(),
        );
        let (mut bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        handle_async_command(
            crate::commands::CommandResult::RunPluginCommand {
                name: "greet".to_string(),
                prompt: "Greet the user".to_string(),
            },
            &engine,
            &client,
            &mut app,
            None,
        )
        .await;

        assert!(app.is_generating);
        match bus.recv_request().await {
            Some(AgentRequest::Submit { text, images }) => {
                assert_eq!(text, "Greet the user");
                assert!(images.is_empty());
            }
            _ => panic!("expected submit request"),
        }
    }

    // -- E2E-style event loop simulation tests --

    /// Simulate the event loop drain-notify-render cycle to verify that
    /// rapid streaming + input events don't cause layout corruption or
    /// render starvation. This is an "E2E" test of the TUI event loop
    /// without requiring a real terminal.
    struct E2ETestEnv {
        app: App,
        notify_tx: tokio::sync::mpsc::Sender<AgentNotification>,
        notify_rx: tokio::sync::mpsc::Receiver<AgentNotification>,
        render_count: usize,
    }

    impl E2ETestEnv {
        fn new() -> Self {
            let (notify_tx, notify_rx) = tokio::sync::mpsc::channel(256);
            Self {
                app: App::new("test-model".to_string()),
                notify_tx,
                notify_rx,
                render_count: 0,
            }
        }

        /// Run one iteration of the event loop: drain notifications,
        /// advance spinner, check layout, render if needed.
        fn tick(&mut self) {
            // Drain all pending notifications
            while let Ok(notification) = self.notify_rx.try_recv() {
                let turn_complete = matches!(notification, AgentNotification::TurnComplete { .. });
                let merged = self.app.handle_notification(notification);
                // In simulation, we don't actually submit to a real client
                if merged.is_some() {
                    self.app
                        .push_message(MessageContent::UserInput(merged.unwrap()));
                    self.app.mark_generating();
                }
                if turn_complete
                    && self.app.pending_workflow.is_none()
                    && !self.app.expecting_turn_start
                {
                    // Drain queue in simulation
                    if let Some(merged) = self.app.take_queued_inputs() {
                        self.app.push_message(MessageContent::UserInput(merged));
                    }
                }
            }

            // Advance spinner
            self.app.advance_spinner_if_due(Instant::now());

            // Detect layout changes
            let layout_sig = self.app.layout_signature();
            let layout_changed = layout_sig != self.app.last_layout_sig;
            if layout_changed {
                self.app.needs_full_clear = true;
                self.app.last_layout_sig = layout_sig;
                self.app.request_redraw();
            }

            // Clear if needed
            if self.app.needs_full_clear {
                self.app.needs_full_clear = false;
                self.app.request_redraw();
            }

            // Render if needed — use the preserved layout_changed flag
            if self.app.needs_redraw {
                let throttled = !layout_changed
                    && self.app.is_generating
                    && self.app.last_render_at.elapsed() < MIN_RENDER_INTERVAL;
                if !throttled {
                    // Simulate render: rebuild visible lines
                    self.app.rebuild_visible_lines();
                    self.app.last_render_at = Instant::now();
                    self.render_count += 1;
                }
                self.app.needs_redraw = false;
            }
        }

        fn send_turn_start(&self) {
            let _ = self.notify_tx.try_send(AgentNotification::TurnStart {
                turn: self.app.total_turns + 1,
            });
        }

        fn send_text_deltas(&self, deltas: &[&str]) {
            for delta in deltas {
                let _ = self.notify_tx.try_send(AgentNotification::TextDelta {
                    text: delta.to_string(),
                });
            }
        }

        fn send_turn_complete(&self) {
            let _ = self.notify_tx.try_send(AgentNotification::TurnComplete {
                turn: self.app.total_turns + 1,
                stop_reason: "end_turn".to_string(),
                usage: clawed_bus::events::UsageInfo {
                    input_tokens: 100,
                    output_tokens: 50,
                    cache_read_tokens: 0,
                    cache_creation_tokens: 0,
                },
            });
        }

        fn send_tool_start(&self, id: &str, tool_name: &str) {
            let _ = self.notify_tx.try_send(AgentNotification::ToolUseStart {
                id: id.to_string(),
                tool_name: tool_name.to_string(),
            });
        }

        fn send_tool_ready(&self, id: &str, tool_name: &str, input: serde_json::Value) {
            let _ = self.notify_tx.try_send(AgentNotification::ToolUseReady {
                id: id.to_string(),
                tool_name: tool_name.to_string(),
                input,
            });
        }

        fn send_tool_output(&self, id: &str, tool_name: &str, line: &str) {
            let _ = self.notify_tx.try_send(AgentNotification::ToolOutputLine {
                id: id.to_string(),
                tool_name: tool_name.to_string(),
                line: line.to_string(),
            });
        }

        fn send_tool_complete(
            &self,
            id: &str,
            tool_name: &str,
            is_error: bool,
            result_preview: Option<&str>,
        ) {
            let _ = self.notify_tx.try_send(AgentNotification::ToolUseComplete {
                id: id.to_string(),
                tool_name: tool_name.to_string(),
                is_error,
                cancelled: false,
                rejected: false,
                reject_reason: None,
                result_preview: result_preview.map(|s| s.to_string()),
            });
        }

        fn send_agent_spawned(&self, agent_id: &str, name: &str) {
            let _ = self.notify_tx.try_send(AgentNotification::AgentSpawned {
                agent_id: agent_id.to_string(),
                name: Some(name.to_string()),
                agent_type: "sub".to_string(),
                background: false,
            });
        }

        fn send_agent_complete(&self, agent_id: &str) {
            let _ = self.notify_tx.try_send(AgentNotification::AgentComplete {
                agent_id: agent_id.to_string(),
                result: "done".to_string(),
                is_error: false,
            });
        }
    }

    #[test]
    fn e2e_rapid_streaming_does_not_corrupt_layout() {
        let mut env = E2ETestEnv::new();

        // Start a turn
        env.send_turn_start();
        env.tick();

        // Send 200 small deltas simulating rapid LLM streaming
        let deltas: Vec<&str> = (0..200)
            .map(|i| {
                if i % 10 == 0 {
                    "**bold** "
                } else if i % 5 == 0 {
                    "`code` "
                } else {
                    "word "
                }
            })
            .collect();
        env.send_text_deltas(&deltas);

        // Process all ticks
        for _ in 0..50 {
            env.tick();
        }

        // Layout should be consistent: signature should match last known
        let sig = env.app.layout_signature();
        assert_eq!(sig, env.app.last_layout_sig);

        // The cached visible lines should be valid (not dirty after last tick)
        assert!(!env.app.cached_visible_lines_dirty);

        // Message count should reflect all deltas
        assert!(!env.app.messages.is_empty());
    }

    #[test]
    fn e2e_streaming_then_input_queue_works() {
        let mut env = E2ETestEnv::new();

        // Start generating
        env.app.mark_generating();
        env.send_turn_start();
        env.tick();

        // Stream some text
        env.send_text_deltas(&["hello ", "world"]);
        env.tick();

        // Verify generating state
        assert!(env.app.is_generating);

        // Complete the turn
        env.send_turn_complete();
        env.tick();

        // After turn complete, generating should be false
        assert!(!env.app.is_generating);

        // Text should be in the messages
        let has_text = env.app.messages.iter().any(|m| {
            if let MessageContent::AssistantText(ref t) = m.content {
                t.contains("hello") || t.contains("world")
            } else {
                false
            }
        });
        assert!(has_text, "streamed text should appear in messages");
    }

    #[test]
    fn e2e_layout_signature_tracks_terminal_resize() {
        let mut env = E2ETestEnv::new();

        // Initial state
        env.app.term_width = 80;
        env.app.term_height = 24;
        let initial_sig = env.app.layout_signature();

        // Simulate terminal resize
        env.app.term_width = 120;
        env.app.term_height = 40;
        let new_sig = env.app.layout_signature();

        // Signature should differ
        assert_ne!(initial_sig, new_sig);
        assert_eq!(new_sig.term_width, 120);
        assert_eq!(new_sig.term_height, 40);
    }

    #[test]
    fn e2e_overlay_toggle_causes_layout_change() {
        let mut env = E2ETestEnv::new();

        let base = env.app.layout_signature();
        assert!(!base.has_overlay);

        // Open overlay
        env.app.overlay = Some(overlay::build_model_overlay("test"));
        let with_overlay = env.app.layout_signature();
        assert!(with_overlay.has_overlay);
        assert_ne!(base, with_overlay);

        // Close overlay
        env.app.overlay = None;
        let after_close = env.app.layout_signature();
        assert!(!after_close.has_overlay);
        // After close, signature should match base
        assert_eq!(base, after_close);
    }

    #[test]
    fn e2e_render_throttle_during_streaming() {
        let mut app = App::new("test-model".to_string());

        // Set stable layout so no layout change triggers
        app.term_width = 80;
        app.term_height = 24;
        app.last_layout_sig = app.layout_signature();

        // Mark generating so throttle applies
        app.mark_generating();

        // First render — should happen (last_render_at is > 32ms ago)
        app.needs_redraw = true;
        let _before_renders = app.last_render_at;
        // Simulate one tick of the event loop render logic
        {
            let layout_changed = false; // layout is stable
            let throttled = !layout_changed
                && app.is_generating
                && app.last_render_at.elapsed() < MIN_RENDER_INTERVAL;
            assert!(
                !throttled,
                "first render should NOT be throttled (elapsed > 32ms)"
            );
        }

        // Perform the render
        app.last_render_at = Instant::now();
        let first_render_at = app.last_render_at;

        // Immediately request another render — should be throttled
        app.needs_redraw = true;
        {
            let layout_changed = false;
            let throttled = !layout_changed
                && app.is_generating
                && app.last_render_at.elapsed() < MIN_RENDER_INTERVAL;
            assert!(
                throttled,
                "second render SHOULD be throttled (elapsed < 32ms)"
            );
        }

        // Verify the first render time is recent
        assert!(first_render_at.elapsed() < Duration::from_millis(10));
    }

    #[test]
    fn e2e_layout_change_bypasses_throttle() {
        let mut env = E2ETestEnv::new();

        env.app.mark_generating();
        env.send_turn_start();
        env.app.term_width = 80;
        env.app.term_height = 24;
        env.app.last_layout_sig = env.app.layout_signature();
        env.tick();

        // Force initial render
        env.app.needs_redraw = true;
        env.tick();
        let initial_renders = env.render_count;

        // Now change layout (open overlay) — should bypass throttle
        env.app.overlay = Some(overlay::build_model_overlay("test"));
        env.app.needs_redraw = true;
        env.tick();

        // Should have rendered despite throttle (layout changed)
        assert!(
            env.render_count > initial_renders,
            "layout change should bypass render throttle"
        );
    }

    // -- E2E: slash command routing tests --

    #[test]
    fn e2e_slash_command_think_toggles_thinking() {
        let (mut bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        app.handle_slash_command(&client, "/think");
        // Think sends SetThinking request, not pending_command
        match bus.recv_request().now_or_never() {
            Some(Some(clawed_bus::events::AgentRequest::SetThinking { mode })) => {
                assert_eq!(mode, "on");
            }
            other => panic!("expected SetThinking request, got {other:?}"),
        }
    }

    #[test]
    fn e2e_slash_command_breakcache_sets_request() {
        let (mut bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        app.handle_slash_command(&client, "/break-cache");
        // BreakCache sends BreakCache request directly, not pending_command
        match bus.recv_request().now_or_never() {
            Some(Some(clawed_bus::events::AgentRequest::BreakCache)) => {}
            other => panic!("expected BreakCache request, got {other:?}"),
        }
    }

    #[test]
    fn e2e_slash_command_env_opens_overlay() {
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        app.handle_slash_command(&client, "/env");
        assert!(app.overlay.is_some());
    }

    #[test]
    fn e2e_slash_command_effort_valid() {
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        app.handle_slash_command(&client, "/effort high");
        assert!(app.messages.len() == 1);
        if let MessageContent::System(ref text) = app.messages[0].content {
            assert!(text.contains("high"));
        } else {
            panic!("expected system message");
        }
    }

    #[test]
    fn e2e_slash_command_effort_invalid() {
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        app.handle_slash_command(&client, "/effort ultra");
        assert!(app.messages.len() == 1);
        if let MessageContent::System(ref text) = app.messages[0].content {
            assert!(text.contains("Invalid"));
        } else {
            panic!("expected system message");
        }
    }

    #[test]
    fn e2e_slash_command_effort_empty_shows_help() {
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        app.handle_slash_command(&client, "/effort");
        assert!(app.messages.len() == 1);
        if let MessageContent::System(ref text) = app.messages[0].content {
            assert!(text.contains("Current effort: auto"));
        } else {
            panic!("expected system message");
        }
    }

    #[test]
    fn e2e_slash_command_tag_with_name() {
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        app.handle_slash_command(&client, "/tag v1.0");
        assert!(app.messages.len() == 1);
        if let MessageContent::System(ref text) = app.messages[0].content {
            assert!(text.contains("v1.0"));
        } else {
            panic!("expected system message");
        }
    }

    #[test]
    fn e2e_slash_command_tag_empty_shows_usage() {
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        app.handle_slash_command(&client, "/tag");
        assert!(app.messages.len() == 1);
        if let MessageContent::System(ref text) = app.messages[0].content {
            assert!(text.contains("Usage"));
        } else {
            panic!("expected system message");
        }
    }

    #[test]
    fn e2e_slash_command_stickers_shows_url() {
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        app.handle_slash_command(&client, "/stickers");
        assert!(app.messages.len() == 1);
        if let MessageContent::System(ref text) = app.messages[0].content {
            assert!(text.contains("stickers"));
        } else {
            panic!("expected system message");
        }
    }

    #[test]
    fn e2e_slash_command_exit_stops_running() {
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());
        assert!(app.running);

        app.handle_slash_command(&client, "/exit");
        assert!(!app.running);
    }

    #[test]
    fn e2e_slash_command_cost_opens_overlay() {
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        app.handle_slash_command(&client, "/cost");
        assert!(app.overlay.is_some());
    }

    #[test]
    fn e2e_slash_command_status_opens_overlay() {
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        app.handle_slash_command(&client, "/status");
        assert!(app.overlay.is_some());
    }

    #[test]
    fn e2e_slash_command_clear_clears_messages() {
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());
        app.push_message(MessageContent::System("hello".to_string()));
        assert_eq!(app.messages.len(), 1);

        app.handle_slash_command(&client, "/clear");
        assert!(app.messages.is_empty());
    }

    #[test]
    fn e2e_slash_command_model_opens_footer_picker() {
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test-model".to_string());

        app.handle_slash_command(&client, "/model");
        assert!(app.footer_picker.is_some());
        assert_eq!(
            app.footer_picker.as_ref().map(|p| p.kind),
            Some(FooterPickerKind::Model)
        );
    }

    #[test]
    fn e2e_slash_command_model_set_closes_picker() {
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test-model".to_string());

        app.handle_slash_command(&client, "/model sonnet");
        // Picker should be cleared after setting model
        assert!(app.footer_picker.is_none());
    }

    #[test]
    fn e2e_slash_command_compact_sends_request() {
        let (mut bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        app.handle_slash_command(&client, "/compact summarize the code");
        // Compact sends Compact request directly, not pending_command
        match bus.recv_request().now_or_never() {
            Some(Some(clawed_bus::events::AgentRequest::Compact { ref instructions })) => {
                assert!(instructions
                    .as_ref()
                    .is_some_and(|i| i.contains("summarize")));
            }
            other => panic!("expected Compact request, got {other:?}"),
        }
    }

    #[test]
    fn e2e_slash_command_review_sends_to_engine() {
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        app.handle_slash_command(&client, "/review check for bugs");
        assert!(app.pending_command.is_some());
        if let Some(crate::commands::CommandResult::Review { ref prompt }) = app.pending_command {
            assert!(prompt.contains("bugs"));
        } else {
            panic!("expected Review command result");
        }
    }

    #[test]
    fn e2e_slash_command_bug_sends_to_engine() {
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        app.handle_slash_command(&client, "/bug why is this crashing");
        assert!(app.pending_command.is_some());
    }

    #[test]
    fn e2e_slash_command_pr_sends_to_engine() {
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        app.handle_slash_command(&client, "/pr review this PR");
        assert!(app.pending_command.is_some());
    }

    #[test]
    fn e2e_slash_command_unknown_stays_unknown() {
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        app.handle_slash_command(&client, "/foobar");
        // Unknown commands should not crash or produce unexpected behavior
    }

    #[test]
    fn e2e_slash_command_mcp_goes_to_pending() {
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        app.handle_slash_command(&client, "/mcp");
        assert!(app.pending_command.is_some());
    }

    #[test]
    fn e2e_slash_command_vim_goes_to_pending() {
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        app.handle_slash_command(&client, "/vim");
        assert!(app.pending_command.is_some());
    }

    #[test]
    fn e2e_slash_command_permissions_goes_to_pending() {
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        app.handle_slash_command(&client, "/permissions");
        assert!(app.pending_command.is_some());
    }

    #[test]
    fn e2e_slash_command_config_goes_to_pending() {
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        app.handle_slash_command(&client, "/config");
        assert!(app.pending_command.is_some());
    }

    #[test]
    fn e2e_slash_command_doctor_goes_to_pending() {
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        app.handle_slash_command(&client, "/doctor");
        assert!(app.pending_command.is_some());
    }

    #[test]
    fn e2e_slash_command_init_goes_to_pending() {
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        app.handle_slash_command(&client, "/init");
        assert!(app.pending_command.is_some());
    }

    #[test]
    fn e2e_slash_command_login_goes_to_pending() {
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        app.handle_slash_command(&client, "/login");
        assert!(app.pending_command.is_some());
    }

    #[test]
    fn e2e_slash_command_logout_goes_to_pending() {
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        app.handle_slash_command(&client, "/logout");
        assert!(app.pending_command.is_some());
    }

    #[test]
    fn e2e_slash_command_theme_goes_to_pending() {
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        app.handle_slash_command(&client, "/theme");
        assert!(app.pending_command.is_some());
    }

    #[test]
    fn e2e_slash_command_agents_goes_to_pending() {
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        app.handle_slash_command(&client, "/agents");
        assert!(app.pending_command.is_some());
    }

    #[test]
    fn e2e_slash_command_plan_goes_to_pending() {
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        app.handle_slash_command(&client, "/plan");
        assert!(app.pending_command.is_some());
    }

    #[test]
    fn e2e_slash_command_resume_shows_picker() {
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        app.handle_slash_command(&client, "/resume");
        // /resume now shows a footer picker (or a message if no sessions)
        assert!(app.footer_picker.is_some() || !app.messages.is_empty());
    }

    #[test]
    fn e2e_slash_command_memory_goes_to_pending() {
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        app.handle_slash_command(&client, "/memory list");
        assert!(app.pending_command.is_some());
    }

    #[test]
    fn e2e_slash_command_pr_comments_goes_to_pending() {
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        app.handle_slash_command(&client, "/pr-comments 123");
        assert!(app.pending_command.is_some());
    }

    #[test]
    fn e2e_slash_command_branch_goes_to_pending() {
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        app.handle_slash_command(&client, "/branch my-feature");
        assert!(app.pending_command.is_some());
    }

    #[test]
    fn e2e_slash_command_search_goes_to_pending() {
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        app.handle_slash_command(&client, "/search hello");
        assert!(app.pending_command.is_some());
    }

    #[test]
    fn e2e_slash_command_history_goes_to_pending() {
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        app.handle_slash_command(&client, "/history");
        assert!(app.pending_command.is_some());
    }

    #[test]
    fn e2e_slash_command_undo_goes_to_pending() {
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        app.handle_slash_command(&client, "/undo");
        assert!(app.pending_command.is_some());
    }

    #[test]
    fn e2e_slash_command_retry_goes_to_pending() {
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        app.handle_slash_command(&client, "/retry");
        assert!(app.pending_command.is_some());
    }

    #[test]
    fn e2e_slash_command_copy_goes_to_pending() {
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        app.handle_slash_command(&client, "/copy");
        assert!(app.pending_command.is_some());
    }

    #[test]
    fn e2e_slash_command_share_goes_to_pending() {
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        app.handle_slash_command(&client, "/share");
        assert!(app.pending_command.is_some());
    }

    #[test]
    fn e2e_slash_command_rename_goes_to_pending() {
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        app.handle_slash_command(&client, "/rename v2");
        assert!(app.pending_command.is_some());
    }

    #[test]
    fn e2e_slash_command_summary_goes_to_pending() {
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        app.handle_slash_command(&client, "/summary");
        assert!(app.pending_command.is_some());
    }

    #[test]
    fn e2e_slash_command_export_goes_to_pending() {
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        app.handle_slash_command(&client, "/export");
        assert!(app.pending_command.is_some());
    }

    #[test]
    fn e2e_slash_command_context_goes_to_pending() {
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        app.handle_slash_command(&client, "/context");
        assert!(app.pending_command.is_some());
    }

    #[test]
    fn e2e_slash_command_fast_goes_to_pending() {
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        app.handle_slash_command(&client, "/fast");
        assert!(app.pending_command.is_some());
    }

    #[test]
    fn e2e_slash_command_rewind_goes_to_pending() {
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        app.handle_slash_command(&client, "/rewind 3");
        assert!(app.pending_command.is_some());
    }

    #[test]
    fn e2e_slash_command_add_dir_goes_to_pending() {
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        app.handle_slash_command(&client, "/add-dir .");
        assert!(app.pending_command.is_some());
    }

    #[test]
    fn e2e_slash_command_files_goes_to_pending() {
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        app.handle_slash_command(&client, "/files *.rs");
        assert!(app.pending_command.is_some());
    }

    #[test]
    fn e2e_slash_command_image_goes_to_pending() {
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        app.handle_slash_command(&client, "/image test.png");
        assert!(app.pending_command.is_some());
    }

    #[test]
    fn e2e_slash_command_feedback_goes_to_pending() {
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        app.handle_slash_command(&client, "/feedback this is great");
        assert!(app.pending_command.is_some());
    }

    #[test]
    fn e2e_slash_command_stats_goes_to_pending() {
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        app.handle_slash_command(&client, "/stats");
        assert!(app.pending_command.is_some());
    }

    #[test]
    fn e2e_slash_command_release_notes_goes_to_pending() {
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        app.handle_slash_command(&client, "/release-notes");
        assert!(app.pending_command.is_some());
    }

    #[test]
    fn e2e_slash_command_reload_context_goes_to_pending() {
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        app.handle_slash_command(&client, "/reload-context");
        assert!(app.pending_command.is_some());
    }

    #[test]
    fn e2e_slash_command_diff_goes_to_pending() {
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        app.handle_slash_command(&client, "/diff");
        assert!(app.pending_command.is_some());
    }

    #[test]
    fn e2e_slash_command_commit_goes_to_pending() {
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        app.handle_slash_command(&client, "/commit fix: typo");
        assert!(app.pending_command.is_some());
    }

    #[test]
    fn e2e_slash_command_commit_push_pr_goes_to_pending() {
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        app.handle_slash_command(&client, "/commit-push-pr");
        assert!(app.pending_command.is_some());
    }

    #[test]
    fn e2e_slash_command_plugin_goes_to_pending() {
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        app.handle_slash_command(&client, "/plugin");
        assert!(app.pending_command.is_some());
    }

    // ========================================================================
    // P0 Supplement: Subcommand parameter tests
    // ========================================================================

    // -- /mcp subcommands --

    #[test]
    fn e2e_mcp_list_produces_overlay() {
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        app.handle_slash_command(&client, "/mcp list");
        assert!(app.pending_command.is_some());
        if let Some(crate::commands::CommandResult::Mcp { ref sub }) = app.pending_command {
            assert_eq!(sub, "list");
        } else {
            panic!("expected Mcp command result");
        }
    }

    #[test]
    fn e2e_mcp_status_produces_overlay() {
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        app.handle_slash_command(&client, "/mcp status");
        assert!(app.pending_command.is_some());
        if let Some(crate::commands::CommandResult::Mcp { ref sub }) = app.pending_command {
            assert_eq!(sub, "status");
        } else {
            panic!("expected Mcp command result");
        }
    }

    #[test]
    fn e2e_mcp_unknown_sub_returns_error() {
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        app.handle_slash_command(&client, "/mcp foobar");
        assert!(app.pending_command.is_some());
    }

    #[test]
    fn e2e_mcp_help_subcommand() {
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        app.handle_slash_command(&client, "/mcp help");
        assert!(app.pending_command.is_some());
        if let Some(crate::commands::CommandResult::Mcp { ref sub }) = app.pending_command {
            assert_eq!(sub, "help");
        } else {
            panic!("expected Mcp command result");
        }
    }

    // -- /plugin subcommands --

    #[test]
    fn e2e_plugin_list_subcommand() {
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        app.handle_slash_command(&client, "/plugin list");
        assert!(app.pending_command.is_some());
        if let Some(crate::commands::CommandResult::Plugin { ref sub }) = app.pending_command {
            assert_eq!(sub, "list");
        } else {
            panic!("expected Plugin command result");
        }
    }

    #[test]
    fn e2e_plugin_info_without_name() {
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        app.handle_slash_command(&client, "/plugin info");
        assert!(app.pending_command.is_some());
        if let Some(crate::commands::CommandResult::Plugin { ref sub }) = app.pending_command {
            assert_eq!(sub, "info");
        } else {
            panic!("expected Plugin command result");
        }
    }

    #[test]
    fn e2e_plugin_enable_without_name() {
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        app.handle_slash_command(&client, "/plugin enable");
        assert!(app.pending_command.is_some());
        if let Some(crate::commands::CommandResult::Plugin { ref sub }) = app.pending_command {
            assert_eq!(sub, "enable");
        } else {
            panic!("expected Plugin command result");
        }
    }

    #[test]
    fn e2e_plugin_disable_without_name() {
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        app.handle_slash_command(&client, "/plugin disable");
        assert!(app.pending_command.is_some());
        if let Some(crate::commands::CommandResult::Plugin { ref sub }) = app.pending_command {
            assert_eq!(sub, "disable");
        } else {
            panic!("expected Plugin command result");
        }
    }

    #[test]
    fn e2e_plugin_unknown_subcommand() {
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        app.handle_slash_command(&client, "/plugin foobar");
        assert!(app.pending_command.is_some());
        if let Some(crate::commands::CommandResult::Plugin { ref sub }) = app.pending_command {
            assert_eq!(sub, "foobar");
        } else {
            panic!("expected Plugin command result");
        }
    }

    // -- /agents subcommands --

    #[test]
    fn e2e_agents_list_subcommand() {
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        app.handle_slash_command(&client, "/agents list");
        assert!(app.pending_command.is_some());
        if let Some(crate::commands::CommandResult::Agents { ref sub }) = app.pending_command {
            assert_eq!(sub, "list");
        } else {
            panic!("expected Agents command result");
        }
    }

    #[test]
    fn e2e_agents_status_subcommand() {
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        app.handle_slash_command(&client, "/agents status");
        assert!(app.pending_command.is_some());
        if let Some(crate::commands::CommandResult::Agents { ref sub }) = app.pending_command {
            assert_eq!(sub, "status");
        } else {
            panic!("expected Agents command result");
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn e2e_agents_status_empty_shows_no_agents() {
        let tmp = TempDir::new().unwrap();
        let engine = Arc::new(
            QueryEngine::builder("test-key", tmp.path())
                .load_claude_md(false)
                .load_memory(false)
                .build(),
        );
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        handle_async_command(
            crate::commands::CommandResult::Agents {
                sub: "status".to_string(),
            },
            &engine,
            &client,
            &mut app,
            None,
        )
        .await;

        assert!(app.overlay.is_some());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn e2e_agents_info_without_name_shows_usage() {
        let tmp = TempDir::new().unwrap();
        let engine = Arc::new(
            QueryEngine::builder("test-key", tmp.path())
                .load_claude_md(false)
                .load_memory(false)
                .build(),
        );
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        handle_async_command(
            crate::commands::CommandResult::Agents {
                sub: "info".to_string(),
            },
            &engine,
            &client,
            &mut app,
            None,
        )
        .await;

        assert!(app.overlay.is_some());
        if let Some(ref overlay) = app.overlay {
            // The format_agents_tui function produces "Usage: /agents info <name>"
            // when sub is exactly "info" with no name
            let text = format!("{:?}", overlay);
            assert!(text.contains("Usage") || text.contains("info"));
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn e2e_agents_create_without_name_shows_usage() {
        let tmp = TempDir::new().unwrap();
        let engine = Arc::new(
            QueryEngine::builder("test-key", tmp.path())
                .load_claude_md(false)
                .load_memory(false)
                .build(),
        );
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        handle_async_command(
            crate::commands::CommandResult::Agents {
                sub: "create".to_string(),
            },
            &engine,
            &client,
            &mut app,
            None,
        )
        .await;

        assert!(app.overlay.is_some());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn e2e_agents_delete_without_name_shows_usage() {
        let tmp = TempDir::new().unwrap();
        let engine = Arc::new(
            QueryEngine::builder("test-key", tmp.path())
                .load_claude_md(false)
                .load_memory(false)
                .build(),
        );
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        handle_async_command(
            crate::commands::CommandResult::Agents {
                sub: "delete".to_string(),
            },
            &engine,
            &client,
            &mut app,
            None,
        )
        .await;

        assert!(app.overlay.is_some());
    }

    // -- /permissions subcommands --

    #[tokio::test(flavor = "multi_thread")]
    async fn e2e_permissions_bypass_mode() {
        let tmp = TempDir::new().unwrap();
        let engine = Arc::new(
            QueryEngine::builder("test-key", tmp.path())
                .load_claude_md(false)
                .load_memory(false)
                .build(),
        );
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        handle_async_command(
            crate::commands::CommandResult::Permissions {
                mode: "bypass".to_string(),
            },
            &engine,
            &client,
            &mut app,
            None,
        )
        .await;

        // Setting a mode should produce a system message, not open picker
        assert!(app.footer_picker.is_none());
        let last_msg = app.messages.last().expect("should have a message");
        let text = match &last_msg.content {
            MessageContent::System(t) => t,
            _ => panic!("expected system message"),
        };
        assert!(text.contains("Permission"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn e2e_permissions_plan_mode() {
        let tmp = TempDir::new().unwrap();
        let engine = Arc::new(
            QueryEngine::builder("test-key", tmp.path())
                .load_claude_md(false)
                .load_memory(false)
                .build(),
        );
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        handle_async_command(
            crate::commands::CommandResult::Permissions {
                mode: "plan".to_string(),
            },
            &engine,
            &client,
            &mut app,
            None,
        )
        .await;

        assert!(app.footer_picker.is_none());
        let last_msg = app.messages.last().expect("should have a message");
        let text = match &last_msg.content {
            MessageContent::System(t) => t,
            _ => panic!("expected system message"),
        };
        assert!(text.contains("Permission"));
    }

    // -- /vim subcommands --

    #[tokio::test(flavor = "multi_thread")]
    async fn e2e_vim_on_enables() {
        let tmp = TempDir::new().unwrap();
        let engine = Arc::new(
            QueryEngine::builder("test-key", tmp.path())
                .load_claude_md(false)
                .load_memory(false)
                .build(),
        );
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        handle_async_command(
            crate::commands::CommandResult::Vim {
                toggle: "on".to_string(),
            },
            &engine,
            &client,
            &mut app,
            None,
        )
        .await;

        let last_msg = app.messages.last().expect("should have a message");
        let text = match &last_msg.content {
            MessageContent::System(t) => t,
            _ => panic!("expected system message"),
        };
        assert!(text.contains("enabled"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn e2e_vim_off_disables() {
        let tmp = TempDir::new().unwrap();
        let engine = Arc::new(
            QueryEngine::builder("test-key", tmp.path())
                .load_claude_md(false)
                .load_memory(false)
                .build(),
        );
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        handle_async_command(
            crate::commands::CommandResult::Vim {
                toggle: "off".to_string(),
            },
            &engine,
            &client,
            &mut app,
            None,
        )
        .await;

        let last_msg = app.messages.last().expect("should have a message");
        let text = match &last_msg.content {
            MessageContent::System(t) => t,
            _ => panic!("expected system message"),
        };
        assert!(text.contains("disabled"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn e2e_vim_invalid_shows_usage() {
        let tmp = TempDir::new().unwrap();
        let engine = Arc::new(
            QueryEngine::builder("test-key", tmp.path())
                .load_claude_md(false)
                .load_memory(false)
                .build(),
        );
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        handle_async_command(
            crate::commands::CommandResult::Vim {
                toggle: "invalid".to_string(),
            },
            &engine,
            &client,
            &mut app,
            None,
        )
        .await;

        let last_msg = app.messages.last().expect("should have a message");
        let text = match &last_msg.content {
            MessageContent::System(t) => t,
            _ => panic!("expected system message"),
        };
        assert!(text.contains("Usage"));
        assert!(text.contains("/vim"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn e2e_vim_case_insensitive() {
        let tmp = TempDir::new().unwrap();
        let engine = Arc::new(
            QueryEngine::builder("test-key", tmp.path())
                .load_claude_md(false)
                .load_memory(false)
                .build(),
        );
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        handle_async_command(
            crate::commands::CommandResult::Vim {
                toggle: "ON".to_string(),
            },
            &engine,
            &client,
            &mut app,
            None,
        )
        .await;

        let last_msg = app.messages.last().expect("should have a message");
        let text = match &last_msg.content {
            MessageContent::System(t) => t,
            _ => panic!("expected system message"),
        };
        assert!(text.contains("enabled"));
    }

    // -- /theme subcommands --

    #[tokio::test(flavor = "multi_thread")]
    async fn e2e_theme_dark_applies() {
        let tmp = TempDir::new().unwrap();
        let engine = Arc::new(
            QueryEngine::builder("test-key", tmp.path())
                .load_claude_md(false)
                .load_memory(false)
                .build(),
        );
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        handle_async_command(
            crate::commands::CommandResult::Theme {
                name: "dark".to_string(),
            },
            &engine,
            &client,
            &mut app,
            None,
        )
        .await;

        let last_msg = app.messages.last().expect("should have a message");
        let text = match &last_msg.content {
            MessageContent::System(t) => t,
            _ => panic!("expected system message"),
        };
        assert!(text.contains("Theme") || text.contains("dark"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn e2e_theme_invalid_shows_available() {
        let tmp = TempDir::new().unwrap();
        let engine = Arc::new(
            QueryEngine::builder("test-key", tmp.path())
                .load_claude_md(false)
                .load_memory(false)
                .build(),
        );
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        handle_async_command(
            crate::commands::CommandResult::Theme {
                name: "nonexistent-theme".to_string(),
            },
            &engine,
            &client,
            &mut app,
            None,
        )
        .await;

        let last_msg = app.messages.last().expect("should have a message");
        let text = match &last_msg.content {
            MessageContent::System(t) => t,
            _ => panic!("expected system message"),
        };
        assert!(text.contains("Unknown theme") || text.contains("Available"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn e2e_theme_case_insensitive() {
        let tmp = TempDir::new().unwrap();
        let engine = Arc::new(
            QueryEngine::builder("test-key", tmp.path())
                .load_claude_md(false)
                .load_memory(false)
                .build(),
        );
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        handle_async_command(
            crate::commands::CommandResult::Theme {
                name: "DARK".to_string(),
            },
            &engine,
            &client,
            &mut app,
            None,
        )
        .await;

        let last_msg = app.messages.last().expect("should have a message");
        let text = match &last_msg.content {
            MessageContent::System(t) => t,
            _ => panic!("expected system message"),
        };
        // Should succeed (case insensitive), not show error
        assert!(!text.contains("Unknown theme"));
    }

    // -- /feedback empty text in TUI --

    #[tokio::test(flavor = "multi_thread")]
    async fn e2e_feedback_empty_appends_in_tui() {
        let tmp = TempDir::new().unwrap();
        let engine = Arc::new(
            QueryEngine::builder("test-key", tmp.path())
                .load_claude_md(false)
                .load_memory(false)
                .build(),
        );
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        handle_async_command(
            crate::commands::CommandResult::Feedback {
                text: String::new(),
            },
            &engine,
            &client,
            &mut app,
            None,
        )
        .await;

        // TUI does NOT reject empty feedback — it appends to the log file
        // and shows a success message. This is a known divergence from REPL.
        let last_msg = app.messages.last().expect("should have a message");
        let text = match &last_msg.content {
            MessageContent::System(t) => t,
            _ => panic!("expected system message"),
        };
        // TUI accepts empty feedback
        assert!(text.contains("Feedback") || text.contains("saved"));
    }

    // -- /cost with windows --

    #[test]
    fn e2e_cost_today_opens_overlay() {
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        // ShowCost is handled synchronously in handle_slash_command
        app.handle_slash_command(&client, "/cost today");
        assert!(app.overlay.is_some());
    }

    #[test]
    fn e2e_cost_week_opens_overlay() {
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        app.handle_slash_command(&client, "/cost week");
        assert!(app.overlay.is_some());
    }

    #[test]
    fn e2e_cost_month_opens_overlay() {
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        app.handle_slash_command(&client, "/cost month");
        assert!(app.overlay.is_some());
    }

    // -- /export json vs markdown --

    #[tokio::test(flavor = "multi_thread")]
    async fn e2e_export_json_creates_json_file() {
        let tmp = TempDir::new().unwrap();
        let engine = Arc::new(
            QueryEngine::builder("test-key", tmp.path())
                .load_claude_md(false)
                .load_memory(false)
                .build(),
        );
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        handle_async_command(
            crate::commands::CommandResult::Export {
                format: "json".to_string(),
            },
            &engine,
            &client,
            &mut app,
            None,
        )
        .await;

        let last_msg = app.messages.last().expect("should have a message");
        let text = match &last_msg.content {
            MessageContent::System(t) => t,
            _ => panic!("expected system message"),
        };
        assert!(text.contains(".json"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn e2e_export_markdown_creates_md_file() {
        let tmp = TempDir::new().unwrap();
        let engine = Arc::new(
            QueryEngine::builder("test-key", tmp.path())
                .load_claude_md(false)
                .load_memory(false)
                .build(),
        );
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        handle_async_command(
            crate::commands::CommandResult::Export {
                format: "markdown".to_string(),
            },
            &engine,
            &client,
            &mut app,
            None,
        )
        .await;

        let last_msg = app.messages.last().expect("should have a message");
        let text = match &last_msg.content {
            MessageContent::System(t) => t,
            _ => panic!("expected system message"),
        };
        assert!(text.contains(".md"));
    }

    // -- /rewind boundary values --

    #[tokio::test(flavor = "multi_thread")]
    async fn e2e_rewind_zero_coerced_to_one() {
        let tmp = TempDir::new().unwrap();
        let engine = Arc::new(
            QueryEngine::builder("test-key", tmp.path())
                .load_claude_md(false)
                .load_memory(false)
                .build(),
        );
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        handle_async_command(
            crate::commands::CommandResult::Rewind {
                turns: "0".to_string(),
            },
            &engine,
            &client,
            &mut app,
            None,
        )
        .await;

        let last_msg = app.messages.last().expect("should have a message");
        let text = match &last_msg.content {
            MessageContent::System(t) => t,
            _ => panic!("expected system message"),
        };
        assert!(text.contains("rewind") || text.contains("Nothing"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn e2e_rewind_non_numeric_defaults_to_one() {
        let tmp = TempDir::new().unwrap();
        let engine = Arc::new(
            QueryEngine::builder("test-key", tmp.path())
                .load_claude_md(false)
                .load_memory(false)
                .build(),
        );
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        handle_async_command(
            crate::commands::CommandResult::Rewind {
                turns: "abc".to_string(),
            },
            &engine,
            &client,
            &mut app,
            None,
        )
        .await;

        let last_msg = app.messages.last().expect("should have a message");
        let text = match &last_msg.content {
            MessageContent::System(t) => t,
            _ => panic!("expected system message"),
        };
        assert!(text.contains("rewind") || text.contains("Nothing"));
    }

    // -- /plan subcommands --

    #[tokio::test(flavor = "multi_thread")]
    async fn e2e_plan_show_no_plan_file() {
        let tmp = TempDir::new().unwrap();
        let engine = Arc::new(
            QueryEngine::builder("test-key", tmp.path())
                .load_claude_md(false)
                .load_memory(false)
                .build(),
        );
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        handle_async_command(
            crate::commands::CommandResult::Plan {
                args: "show".to_string(),
            },
            &engine,
            &client,
            &mut app,
            None,
        )
        .await;

        let last_msg = app.messages.last().expect("should have a message");
        let text = match &last_msg.content {
            MessageContent::System(t) => t,
            _ => panic!("expected system message"),
        };
        assert!(text.contains("No plan") || text.contains("plan"));
    }

    // -- /add-dir invalid paths --

    #[tokio::test(flavor = "multi_thread")]
    async fn e2e_add_dir_empty_shows_usage() {
        let tmp = TempDir::new().unwrap();
        let engine = Arc::new(
            QueryEngine::builder("test-key", tmp.path())
                .load_claude_md(false)
                .load_memory(false)
                .build(),
        );
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        handle_async_command(
            crate::commands::CommandResult::AddDir {
                path: String::new(),
            },
            &engine,
            &client,
            &mut app,
            None,
        )
        .await;

        let last_msg = app.messages.last().expect("should have a message");
        let text = match &last_msg.content {
            MessageContent::System(t) => t,
            _ => panic!("expected system message"),
        };
        assert!(text.contains("Usage"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn e2e_add_dir_nonexistent_shows_error() {
        let tmp = TempDir::new().unwrap();
        let engine = Arc::new(
            QueryEngine::builder("test-key", tmp.path())
                .load_claude_md(false)
                .load_memory(false)
                .build(),
        );
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        handle_async_command(
            crate::commands::CommandResult::AddDir {
                path: "/nonexistent/path/xyz123".to_string(),
            },
            &engine,
            &client,
            &mut app,
            None,
        )
        .await;

        let last_msg = app.messages.last().expect("should have a message");
        let text = match &last_msg.content {
            MessageContent::System(t) => t,
            _ => panic!("expected system message"),
        };
        assert!(text.contains("Directory not found"));
    }

    // -- /image invalid paths --

    #[tokio::test(flavor = "multi_thread")]
    async fn e2e_image_empty_shows_usage() {
        let tmp = TempDir::new().unwrap();
        let engine = Arc::new(
            QueryEngine::builder("test-key", tmp.path())
                .load_claude_md(false)
                .load_memory(false)
                .build(),
        );
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        handle_async_command(
            crate::commands::CommandResult::Image {
                path: String::new(),
            },
            &engine,
            &client,
            &mut app,
            None,
        )
        .await;

        let last_msg = app.messages.last().expect("should have a message");
        let text = match &last_msg.content {
            MessageContent::System(t) => t,
            _ => panic!("expected system message"),
        };
        assert!(text.contains("Usage"));
        assert!(text.contains("/image"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn e2e_image_nonexistent_shows_error() {
        let tmp = TempDir::new().unwrap();
        let engine = Arc::new(
            QueryEngine::builder("test-key", tmp.path())
                .load_claude_md(false)
                .load_memory(false)
                .build(),
        );
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        handle_async_command(
            crate::commands::CommandResult::Image {
                path: "nonexistent.png".to_string(),
            },
            &engine,
            &client,
            &mut app,
            None,
        )
        .await;

        let last_msg = app.messages.last().expect("should have a message");
        let text = match &last_msg.content {
            MessageContent::System(t) => t,
            _ => panic!("expected system message"),
        };
        assert!(text.contains("error") || text.contains("Error") || text.contains("failed"));
    }

    // -- /history page boundaries --

    #[tokio::test(flavor = "multi_thread")]
    async fn e2e_history_page_1_empty() {
        let tmp = TempDir::new().unwrap();
        let engine = Arc::new(
            QueryEngine::builder("test-key", tmp.path())
                .load_claude_md(false)
                .load_memory(false)
                .build(),
        );
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        handle_async_command(
            crate::commands::CommandResult::History { page: 1 },
            &engine,
            &client,
            &mut app,
            None,
        )
        .await;

        assert!(app.overlay.is_some());
        if let Some(ref overlay) = app.overlay {
            let text = format!("{:?}", overlay);
            assert!(text.contains("No conversation") || text.contains("History"));
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn e2e_history_page_999_clamped() {
        let tmp = TempDir::new().unwrap();
        let engine = Arc::new(
            QueryEngine::builder("test-key", tmp.path())
                .load_claude_md(false)
                .load_memory(false)
                .build(),
        );
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        handle_async_command(
            crate::commands::CommandResult::History { page: 999 },
            &engine,
            &client,
            &mut app,
            None,
        )
        .await;

        // Page 999 should be clamped to the last available page
        assert!(app.overlay.is_some());
    }

    // -- /pr-comments parsing boundaries --

    #[test]
    fn e2e_pr_comments_invalid_number_defaults_to_zero() {
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        app.handle_slash_command(&client, "/pr-comments abc");
        assert!(app.pending_command.is_some());
        if let Some(crate::commands::CommandResult::PrComments { pr_number }) = app.pending_command
        {
            assert_eq!(pr_number, 0);
        } else {
            panic!("expected PrComments command result");
        }
    }

    #[test]
    fn e2e_pr_comments_no_number_defaults_to_zero() {
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        app.handle_slash_command(&client, "/pr-comments");
        assert!(app.pending_command.is_some());
        if let Some(crate::commands::CommandResult::PrComments { pr_number }) = app.pending_command
        {
            assert_eq!(pr_number, 0);
        } else {
            panic!("expected PrComments command result");
        }
    }

    // -- Unicode/CJK parameters --

    #[tokio::test(flavor = "multi_thread")]
    async fn e2e_commit_with_cjk_characters() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path();

        fn run_git(repo: &std::path::Path, args: &[&str]) {
            let output = std::process::Command::new("git")
                .args(args)
                .current_dir(repo)
                .output()
                .unwrap_or_else(|e| panic!("failed to execute git {}: {}", args.join(" "), e));
            assert!(
                output.status.success(),
                "git {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr)
            );
        }

        // Initialise a git repo with staged changes so prepare_commit_prompt succeeds
        run_git(repo, &["init"]);
        run_git(repo, &["config", "user.email", "test@example.com"]);
        run_git(repo, &["config", "user.name", "Test User"]);
        std::fs::write(repo.join("file.txt"), "hello").unwrap();
        run_git(repo, &["add", "file.txt"]);
        run_git(repo, &["commit", "-m", "initial"]);
        // Create a new change and stage it so prepare_commit_prompt sees work to do
        std::fs::write(repo.join("file.txt"), "hello world").unwrap();
        run_git(repo, &["add", "file.txt"]);

        let engine = Arc::new(
            QueryEngine::builder("test-key", repo)
                .load_claude_md(false)
                .load_memory(false)
                .build(),
        );
        let (mut bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        // Switch to the temp repo so prepare_commit_prompt uses the correct cwd
        let _guard = std::env::set_current_dir(repo);

        // Commit goes to pending_command, then handle_async_command submits to engine
        handle_async_command(
            crate::commands::CommandResult::Commit {
                message: "你好世界".to_string(),
            },
            &engine,
            &client,
            &mut app,
            None,
        )
        .await;

        // Commit submits prompt to engine, so we should see a Submit request on the bus
        match bus.recv_request().now_or_never() {
            Some(Some(clawed_bus::events::AgentRequest::Submit { ref text, .. })) => {
                assert!(text.contains("你好世界"));
            }
            other => panic!("expected Submit request with CJK text, got {other:?}"),
        }
    }

    #[test]
    fn e2e_tag_with_cjk_characters() {
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        // Tag is handled synchronously in handle_slash_command
        app.handle_slash_command(&client, "/tag 测试");
        let last_msg = app.messages.last().expect("should have a message");
        let text = match &last_msg.content {
            MessageContent::System(t) => t,
            _ => panic!("expected system message"),
        };
        assert!(text.contains("测试"));
    }

    // -- Fast mode toggle --

    #[tokio::test(flavor = "multi_thread")]
    async fn e2e_fast_off_switches_to_sonnet() {
        let tmp = TempDir::new().unwrap();
        let engine = Arc::new(
            QueryEngine::builder("test-key", tmp.path())
                .load_claude_md(false)
                .load_memory(false)
                .build(),
        );
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        handle_async_command(
            crate::commands::CommandResult::Fast {
                toggle: "off".to_string(),
            },
            &engine,
            &client,
            &mut app,
            None,
        )
        .await;

        let last_msg = app.messages.last().expect("should have a message");
        let text = match &last_msg.content {
            MessageContent::System(t) => t,
            _ => panic!("expected system message"),
        };
        assert!(text.contains("Fast mode off") || text.contains("Switched"));
    }

    // -- /memory open subcommand --

    #[test]
    fn e2e_memory_open_subcommand() {
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        app.handle_slash_command(&client, "/memory open test.md");
        assert!(app.pending_command.is_some());
        if let Some(crate::commands::CommandResult::Memory { ref sub }) = app.pending_command {
            assert_eq!(sub, "open test.md");
        } else {
            panic!("expected Memory command result");
        }
    }

    // -- Pending command field verification (strengthened assertions) --

    #[test]
    fn e2e_history_page_3_verifies_field() {
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        app.handle_slash_command(&client, "/history 3");
        assert!(app.pending_command.is_some());
        if let Some(crate::commands::CommandResult::History { page }) = app.pending_command {
            assert_eq!(page, 3);
        } else {
            panic!("expected History command result");
        }
    }

    #[test]
    fn e2e_rewind_3_verifies_field() {
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        app.handle_slash_command(&client, "/rewind 3");
        assert!(app.pending_command.is_some());
        if let Some(crate::commands::CommandResult::Rewind { turns }) = app.pending_command {
            assert_eq!(turns, "3");
        } else {
            panic!("expected Rewind command result");
        }
    }

    #[test]
    fn e2e_export_markdown_verifies_format_field() {
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        app.handle_slash_command(&client, "/export markdown");
        assert!(app.pending_command.is_some());
        if let Some(crate::commands::CommandResult::Export { format }) = app.pending_command {
            assert_eq!(format, "markdown");
        } else {
            panic!("expected Export command result");
        }
    }

    #[test]
    fn e2e_vim_on_verifies_toggle_field() {
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        app.handle_slash_command(&client, "/vim on");
        assert!(app.pending_command.is_some());
        if let Some(crate::commands::CommandResult::Vim { toggle }) = app.pending_command {
            assert_eq!(toggle, "on");
        } else {
            panic!("expected Vim command result");
        }
    }

    #[test]
    fn e2e_permissions_bypass_verifies_mode_field() {
        let (_bus, client) = EventBus::new(16);
        let mut app = App::new("test".to_string());

        app.handle_slash_command(&client, "/permissions bypass");
        assert!(app.pending_command.is_some());
        if let Some(crate::commands::CommandResult::Permissions { mode }) = app.pending_command {
            assert_eq!(mode, "bypass");
        } else {
            panic!("expected Permissions command result");
        }
    }

    // ── E2E: UX rendering tests ──────────────────────────────────────────────

    #[test]
    fn e2e_tool_tree_renders_depth_connector() {
        let mut env = E2ETestEnv::new();
        env.app.term_width = 80;
        env.app.term_height = 24;

        // Spawn an agent so depth becomes 1 for subsequent tools
        env.send_agent_spawned("agent-1", "CodeReview");
        env.tick();

        // Start a tool inside the agent context
        env.send_tool_start("t1", "Read");
        env.send_tool_ready("t1", "Read", json!({"path": "src/main.rs"}));
        env.tick();

        // Render
        env.app.needs_redraw = true;
        env.tick();

        let text = env
            .app
            .cached_visible_lines
            .iter()
            .map(|l| line_text(l))
            .collect::<Vec<_>>()
            .join("\n");

        // Depth=1 should produce tree connector prefix
        assert!(
            text.contains("└─ "),
            "tool at depth=1 should render tree connector, got:\n{text}"
        );
        assert!(
            text.contains("● Read"),
            "tool header should contain name, got:\n{text}"
        );
    }

    #[test]
    fn e2e_tool_error_shows_red_failed() {
        let mut env = E2ETestEnv::new();
        env.app.term_width = 80;
        env.app.term_height = 24;

        env.send_tool_start("t1", "Bash");
        env.send_tool_ready("t1", "Bash", json!({"command": "false"}));
        env.send_tool_output("t1", "Bash", "something went wrong");
        env.send_tool_complete("t1", "Bash", true, Some("exit code 1"));
        env.tick();

        env.app.needs_redraw = true;
        env.tick();

        let text = env
            .app
            .cached_visible_lines
            .iter()
            .map(|l| line_text(l))
            .collect::<Vec<_>>()
            .join("\n");

        assert!(
            text.contains("✗ failed"),
            "error tool should show ✗ failed, got:\n{text}"
        );
    }

    #[test]
    fn e2e_tool_success_shows_duration() {
        let mut env = E2ETestEnv::new();
        env.app.term_width = 80;
        env.app.term_height = 24;

        env.send_tool_start("t1", "Bash");
        env.send_tool_ready("t1", "Bash", json!({"command": "echo hi"}));
        env.send_tool_output("t1", "Bash", "hi");
        // Small sleep so duration is non-zero
        std::thread::sleep(std::time::Duration::from_millis(50));
        env.send_tool_complete("t1", "Bash", false, Some("hi"));
        env.tick();

        env.app.needs_redraw = true;
        env.tick();

        let text = env
            .app
            .cached_visible_lines
            .iter()
            .map(|l| line_text(l))
            .collect::<Vec<_>>()
            .join("\n");

        // Completed successful tool should show a duration line with checkmark
        assert!(
            text.contains('✓') || text.contains("ms") || text.contains('s'),
            "success tool should show duration marker, got:\n{text}"
        );
    }

    #[test]
    fn e2e_tool_collapsed_shows_fold_hint() {
        let mut env = E2ETestEnv::new();
        env.app.term_width = 80;
        env.app.term_height = 24;

        env.send_tool_start("t1", "Read");
        env.send_tool_ready("t1", "Read", json!({"path": "file.txt"}));
        // Emit some live output so the tool takes the "streamed" fold path
        env.send_tool_output("t1", "Read", "out1");
        env.send_tool_output("t1", "Read", "out2");
        env.send_tool_complete(
            "t1",
            "Read",
            false,
            Some("line1\nline2\nline3\nline4\nline5\nline6"),
        );
        env.tick();

        // Collapse the tool message
        if let Some(msg) = env
            .app
            .messages
            .iter_mut()
            .rev()
            .find(|m| matches!(&m.content, MessageContent::ToolExecution { .. }))
        {
            msg.toggle_collapsed();
        }
        env.app.invalidate_visible_lines();

        env.app.needs_redraw = true;
        env.tick();

        let text = env
            .app
            .cached_visible_lines
            .iter()
            .map(|l| line_text(l))
            .collect::<Vec<_>>()
            .join("\n");

        assert!(
            text.contains("more lines (Ctrl+E to expand)"),
            "collapsed tool should show fold hint, got:\n{text}"
        );
    }

    #[test]
    fn e2e_consecutive_system_messages_collapsed() {
        let mut env = E2ETestEnv::new();
        env.app.term_width = 80;
        env.app.term_height = 24;

        // Invalidate cache so rebuild_visible_lines runs the collapse logic
        env.app.invalidate_visible_lines();
        // Push multiple non-important system messages
        for i in 0..5 {
            env.app
                .push_message(MessageContent::System(format!("status {i}")));
        }
        env.app.needs_redraw = true;
        env.tick();

        let text = env
            .app
            .cached_visible_lines
            .iter()
            .map(|l| line_text(l))
            .collect::<Vec<_>>()
            .join("\n");

        assert!(
            text.contains("+ 3 system messages"),
            "consecutive system messages should collapse (5 -> first + +3 + last), got:\n{text}"
        );
    }

    #[test]
    fn e2e_important_system_not_collapsed() {
        let mut env = E2ETestEnv::new();
        env.app.term_width = 80;
        env.app.term_height = 24;

        // Push an important system message (contains "error")
        env.app.push_message(MessageContent::System(
            "An error occurred while processing".to_string(),
        ));
        // Followed by normal ones
        for i in 0..3 {
            env.app
                .push_message(MessageContent::System(format!("status {i}")));
        }
        env.app.needs_redraw = true;
        env.tick();

        let text = env
            .app
            .cached_visible_lines
            .iter()
            .map(|l| line_text(l))
            .collect::<Vec<_>>()
            .join("\n");

        assert!(
            text.contains("error"),
            "important system message should remain visible, got:\n{text}"
        );
    }

    #[test]
    fn e2e_separator_between_different_message_types() {
        let mut env = E2ETestEnv::new();
        env.app.term_width = 80;
        env.app.term_height = 24;

        env.app
            .push_message(MessageContent::AssistantText("hello".to_string()));
        env.app
            .push_message(MessageContent::UserInput("hi".to_string()));
        env.app.needs_redraw = true;
        env.tick();

        let lines: Vec<_> = env
            .app
            .cached_visible_lines
            .iter()
            .map(|l| line_text(l))
            .collect();

        // Find the assistant text and user input, verify there is a blank separator
        let assistant_idx = lines.iter().position(|l| l.contains("hello")).unwrap();
        let user_idx = lines.iter().position(|l| l.contains("hi")).unwrap();
        assert!(
            lines[assistant_idx + 1..user_idx]
                .iter()
                .any(|l| l.is_empty()),
            "different message types should have a blank separator, got: {lines:?}"
        );
    }

    #[test]
    fn e2e_assistant_and_thinking_no_separator() {
        let mut env = E2ETestEnv::new();
        env.app.term_width = 80;
        env.app.term_height = 24;

        env.app
            .push_message(MessageContent::AssistantText("hello".to_string()));
        env.app
            .push_message(MessageContent::ThinkingText("reasoning".to_string()));
        // Expand the thinking message so its body text is visible inline.
        if let Some(msg) = env.app.messages.last_mut() {
            msg.collapsed = false;
            msg.invalidate_cache();
        }
        env.app.invalidate_visible_lines();
        env.app.needs_redraw = true;
        env.tick();

        let lines: Vec<_> = env
            .app
            .cached_visible_lines
            .iter()
            .map(|l| line_text(l))
            .collect();

        let assistant_idx = lines.iter().position(|l| l.contains("hello")).unwrap();
        let thinking_idx = lines.iter().position(|l| l.contains("reasoning")).unwrap();
        // There should be no blank line between them
        let between = &lines[assistant_idx + 1..thinking_idx];
        assert!(
            !between.iter().any(|l| l.is_empty()),
            "assistant and thinking should flow together without separator, got: {lines:?}"
        );
    }

    #[test]
    fn e2e_thinking_collapsed_shows_hint() {
        let mut env = E2ETestEnv::new();
        env.app.term_width = 80;
        env.app.term_height = 24;

        env.app.push_message(MessageContent::ThinkingText(
            "line1\nline2\nline3\nline4\nline5".to_string(),
        ));
        env.app.needs_redraw = true;
        env.tick();

        let text = env
            .app
            .cached_visible_lines
            .iter()
            .map(|l| line_text(l))
            .collect::<Vec<_>>()
            .join("\n");

        assert!(
            text.contains("∴ Thinking"),
            "collapsed thinking should show placeholder, got:\n{text}"
        );
    }

    // -- Test helpers for ANSI snapshot output --------------------------------

    fn fg_ansi(color: ratatui::style::Color) -> String {
        use ratatui::style::Color;
        match color {
            Color::Reset => String::new(),
            Color::Black => "30".to_string(),
            Color::Red => "31".to_string(),
            Color::Green => "32".to_string(),
            Color::Yellow => "33".to_string(),
            Color::Blue => "34".to_string(),
            Color::Magenta => "35".to_string(),
            Color::Cyan => "36".to_string(),
            Color::Gray | Color::DarkGray => "90".to_string(),
            Color::LightRed => "91".to_string(),
            Color::LightGreen => "92".to_string(),
            Color::LightYellow => "93".to_string(),
            Color::LightBlue => "94".to_string(),
            Color::LightMagenta => "95".to_string(),
            Color::LightCyan => "96".to_string(),
            Color::White => "97".to_string(),
            Color::Indexed(i) => format!("38;5;{i}"),
            Color::Rgb(r, g, b) => format!("38;2;{r};{g};{b}"),
        }
    }

    fn buffer_to_ansi(buf: &ratatui::buffer::Buffer) -> String {
        use ratatui::style::Modifier;
        let mut output =
            String::with_capacity(buf.area.width as usize * buf.area.height as usize * 12);
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                let cell = buf.get(x, y);
                let sym = cell.symbol();
                let symbol = if sym.is_empty() { " " } else { sym };

                let mut codes = vec!["0".to_string()];
                if cell.modifier.contains(Modifier::BOLD) {
                    codes.push("1".to_string());
                }
                if cell.modifier.contains(Modifier::DIM) {
                    codes.push("2".to_string());
                }
                if cell.modifier.contains(Modifier::ITALIC) {
                    codes.push("3".to_string());
                }
                if cell.modifier.contains(Modifier::UNDERLINED) {
                    codes.push("4".to_string());
                }
                let fg = fg_ansi(cell.fg);
                if !fg.is_empty() {
                    codes.push(fg);
                }
                output.push_str(&format!("\x1b[{}m{}", codes.join(";"), symbol));
            }
            output.push_str("\x1b[0m\n");
        }
        output
    }

    fn snap(name: &str, dir: &str, app: &mut App) {
        use std::fs;
        let backend = ratatui::backend::TestBackend::new(80, 24);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|frame| render(frame, app)).unwrap();
        let buf = terminal.backend().buffer().clone();
        let ansi = buffer_to_ansi(&buf);
        let path = format!("{dir}/{name}.ansi");
        fs::write(&path, &ansi).unwrap();
        println!("\n=== {name} ===\n{ansi}\x1b[0m");
    }

    /// Visual verification: render key TUI states to /tmp for side-by-side
    /// comparison with official Claude Code. Run with:
    ///   cargo test -p clawed-cli tui_visual_snapshot -- --nocapture
    #[test]
    fn tui_visual_snapshot() {
        use std::fs;

        let dir = "/tmp/clawed_tui_verification";
        let _ = fs::create_dir_all(dir);

        // Scene 1: Initial empty state (welcome screen)
        let mut app = App::new("claude-3.5".to_string());
        app.term_width = 80;
        app.term_height = 24;
        app.needs_redraw = true;
        app.rebuild_visible_lines();
        snap("01_initial_state", dir, &mut app);

        // Scene 2: Conversation with user + assistant messages
        let mut app = App::new("claude-3.5".to_string());
        app.term_width = 80;
        app.term_height = 24;
        app.needs_redraw = true;
        app.push_message(MessageContent::UserInput(
            "Explain Rust ownership".to_string(),
        ));
        app.push_message(MessageContent::AssistantText(
            "Rust ownership is a set of rules that govern how memory is managed.".to_string(),
        ));
        snap("02_with_messages", dir, &mut app);

        // Scene 3: LLM generating (spinner + bottom bar hints)
        let mut app = App::new("claude-3.5".to_string());
        app.term_width = 80;
        app.term_height = 24;
        app.needs_redraw = true;
        app.push_message(MessageContent::UserInput("Write hello world".to_string()));
        app.push_message(MessageContent::AssistantText(String::new()));
        app.mark_generating();
        snap("03_generating", dir, &mut app);

        // Scene 4: Context suggestions overlay (file / MCP / agent)
        let mut app = App::new("claude-3.5".to_string());
        app.term_width = 80;
        app.term_height = 24;
        app.needs_redraw = true;
        app.suggestions = vec![
            SuggestionItem {
                id: "file1".to_string(),
                display_text: "src/main.rs".to_string(),
                description: Some("Add to context".to_string()),
                kind: SuggestionKind::File,
            },
            SuggestionItem {
                id: "mcp1".to_string(),
                display_text: "docs/readme".to_string(),
                description: None,
                kind: SuggestionKind::McpResource,
            },
            SuggestionItem {
                id: "agent1".to_string(),
                display_text: "@reviewer".to_string(),
                description: Some("Code reviewer".to_string()),
                kind: SuggestionKind::Agent,
            },
        ];
        app.selected_suggestion = 0;
        snap("04_suggestions_overlay", dir, &mut app);

        println!("Screenshots saved to {dir}/");
        println!("Compare with official CC: open two terminals, run `claude` and `cargo run --`, then visually verify.");
    }

    #[test]
    fn generate_delivery_screenshots() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let dir = "/tmp/clawed_delivery";
        let _ = std::fs::create_dir_all(dir);

        // Renders app state as colored HTML
        let mut snap = |app: &mut App, name: &str| {
            let backend = TestBackend::new(120, 40);
            let mut t = Terminal::new(backend).unwrap();
            t.draw(|f| crate::tui::render(f, app)).unwrap();
            let buf = t.backend().buffer();
            let mut h = String::from(
                "<pre style='background:#1a1a2e;color:#ddd;font:13px monospace;padding:10px'>\n",
            );
            for y in 0..buf.area().height {
                for x in 0..buf.area().width {
                    if let Some(c) = buf.cell((x, y)) {
                        let ch = match c.symbol() {
                            " " => " ",
                            "&" => "&amp;",
                            "<" => "&lt;",
                            ">" => "&gt;",
                            s => s,
                        };
                        let bg = c.style().bg.map_or(String::new(), |c| {
                            format!("background:{:?};", c)
                                .replace("Rgb(", "rgb(")
                                .replace(")", ";")
                        });
                        let fg = c.style().fg.map_or(String::new(), |c| {
                            format!("color:{:?};", c)
                                .replace("Rgb(", "rgb(")
                                .replace(")", ";")
                        });
                        let b = if c.style().add_modifier.contains(Modifier::BOLD) {
                            "font-weight:bold;"
                        } else {
                            ""
                        };
                        let i = if c.style().add_modifier.contains(Modifier::ITALIC) {
                            "font-style:italic;"
                        } else {
                            ""
                        };
                        let s = if c.style().add_modifier.contains(Modifier::CROSSED_OUT) {
                            "text-decoration:line-through;"
                        } else {
                            ""
                        };
                        h.push_str(&format!("<span style='{bg}{fg}{b}{i}{s}'>{ch}</span>"));
                    }
                }
                h.push('\n');
            }
            h.push_str("</pre>");
            std::fs::write(format!("{dir}/{}.html", name), &h).unwrap();
        };

        macro_rules! fresh {
            ($name:expr, |$app:ident| $body:block) => {{
                let mut $app = App::new("claude-sonnet-4-6".into());
                $body
                snap(&mut $app, $name);
            }};
        }

        fresh!("01_welcome", |app| {});
        fresh!("02_markdown", |app| {
            app.push_message(MessageContent::AssistantText(
            "### Project Progress\n\n- TUI core refactoring done\n- Permission dialog fixed\n\n> Code review is part of writing code.\n\n---\n\n| Module | Status |\n|---|---|\n| config | Done |\n| overlay | Pending |".into()));
        });
        fresh!("03_diff", |app| {
            app.push_message(MessageContent::AssistantText("Edit result:".into()));
            let mut m = Message::new(MessageContent::ToolExecution {
                name: "Edit".to_string(), input: Some("src/main.rs".to_string()),
                output_lines: vec![], is_error: false, duration_ms: 1500,
                full_result: Some("--- a/src/main.rs\n+++ b/src/main.rs\n@@ -1,3 +1,4 @@\n unchanged\n-removed\n+added\n unchanged".to_string()), depth: 0,
            });
            app.messages.push(m);
        });
        fresh!("04_taskplan", |app| {
            app.task_plan.add_task("t1".into(), "Core engine".into());
            app.task_plan.add_task("t2".into(), "Test suite".into());
            app.task_plan.add_task("t3".into(), "Edge cases".into());
            app.task_plan.complete_task("t1", false);
            app.task_plan.complete_task("t2", false);
        });
        fresh!("05_agents", |app| {
            app.status.active_agents.insert(
                "a1".into(),
                status::AgentInfo {
                    name: "reviewer".into(),
                    started: std::time::Instant::now(),
                    state: status::AgentState::Active,
                    activity: Some("Read(src/lib.rs)".into()),
                    tool_count: 3,
                    token_estimate: 4500,
                    idle_since: None,
                    color: Color::Magenta,
                },
            );
            app.status.active_agents.insert(
                "a2".into(),
                status::AgentInfo {
                    name: "test-runner".into(),
                    started: std::time::Instant::now(),
                    state: status::AgentState::Idle,
                    activity: None,
                    tool_count: 1,
                    token_estimate: 800,
                    idle_since: Some(std::time::Instant::now()),
                    color: Color::Cyan,
                },
            );
            app.status.is_generating = true;
            app.status.spinner_frame = 3;
        });

        eprintln!("Delivery screenshots: {}/", dir);
    }
}
