//! Full-screen TUI with ratatui double-buffered rendering.
//!
//! Layout:
//! ```text
//! ┌────────────────────────────────────┬───────────┐
//! │  Messages (scrollable)             │  Tasks    │ ← right top panel
//! │                                    ├───────────┤
//! │                                    │  Tools    │ ← tool call history
//! │                                    ├───────────┤
//! │                                    │  Stats    │ ← model / turn / tokens / cost / ctx%
//! ├────────────────────────────────────┴───────────┤
//! │  ────────────────────────────────────────────  │ ← input separator
//! │  > user input here_                            │ ← input area (full width)
//! │  ────────────────────────────────────────────  │ ← input separator
//! ├────────────────────────────────────────────────┤
//! │  Tab: complete  Ctrl+J: newline  Ctrl+C: abort │ ← status bar (full width)
//! └────────────────────────────────────────────────┘
//! ```

mod bash_mode;
mod bottombar;
mod commands;
pub(crate) mod diff_style;
mod handlers;
mod helpers;
mod input;
mod markdown;
mod messages;
mod overlay;
mod permission;
mod rendering;
mod run;
mod state;
mod status;
mod statusline;
mod tasklist;
mod taskplan;
mod textarea;
mod tool_monitor;
pub(crate) mod verbs;
use commands::*;
use helpers::*;
use rendering::*;
pub use run::*;

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
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event, KeyCode, KeyEventKind, KeyModifiers, MouseEventKind,
};
use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Clear, List, ListItem, Paragraph, Wrap},
    Frame,
};
use tokio::sync::mpsc;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::goal::{
    goal_status_message, judge_goal_progress, prepare_goal_iteration, GoalDecisionAction,
    GoalState, GoalStatus,
};
use crate::input::command_description;

use self::messages::{Message, MessageContent};
use self::overlay::{Overlay, OverlayAction, SelectionItem};
use self::permission::PendingPermission;
use self::status::{ToolInfo, TuiStatusState};
use self::tool_monitor::ToolHistoryEntry;

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
const MAX_COMPLETION_POPUP_ITEMS: usize = 10;

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
    /// Right panel width (0 when hidden).
    panel_width: u16,
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

/// Which section of the right panel has keyboard focus.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RightPanelFocus {
    Tasks,
    ToolHistory,
    Stats,
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
    /// Tool call history for the right-side monitor panel.
    tool_history: Vec<ToolHistoryEntry>,
    /// Scroll offset for the tool history panel.
    tool_history_scroll: usize,
    /// Current width of the right side panel (adjustable).
    panel_width: u16,
    /// Which sub-panel of the right panel has focus (if any).
    right_panel_focus: Option<RightPanelFocus>,
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
    goal_state: Option<GoalState>,
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
    /// Height cache: estimated visual lines per message at current width.
    /// Rebuilt when `cached_visible_lines_dirty` is set or width changes.
    message_line_counts: Vec<Option<u16>>,
    /// The width at which message_line_counts were measured.
    message_line_counts_width: u16,
    last_spinner_tick: Instant,
    /// Instant of the last render. Used to throttle render rate during
    /// active streaming so the event loop has time to process input events.
    last_render_at: Instant,
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
    /// Cached render state for the tool monitor panel.
    tool_monitor_cache: tool_monitor::ToolMonitorCache,
    /// Maximum context window size for the current model.
    max_context_tokens: u64,
    /// X coordinate of the right panel boundary from last render. Used to
    /// determine which panel the mouse wheel scrolls. 0 when panel hidden.
    last_right_panel_x: u16,
    /// Y ranges of the right sub-panels from last render.
    right_tasks_rect: Rect,
    right_tools_rect: Rect,
    right_stats_rect: Rect,
    /// Scroll offset for the stats panel.
    stats_scroll_offset: usize,
    /// Scrollbar interaction state.
    scrollbar_rect: Rect,
    scrollbar_total: usize,
    scrollbar_viewport: usize,
    scrollbar_dragging: bool,
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

#[cfg(test)]
mod tests;
