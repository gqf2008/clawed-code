//! Full-screen TUI with ratatui double-buffered rendering.
//!
//! Layout:
//! ```text
//! Messages (scrollable)
//! -- separator --
//! 00:05  thinking  bash (00:02)  model  (status, when active)
//! > user input here_
//! Tab: complete  Up/Down: history  Esc: quit  (hint bar, toggleable)
//! ```

mod bottombar;
mod input;
mod markdown;
mod messages;
mod permission;
mod status;

pub use input::InputWidget;

use std::sync::Arc;
use std::time::Instant;

use clawed_agent::engine::QueryEngine;
use clawed_bus::bus::ClientHandle;
use clawed_bus::events::{AgentNotification, ImageAttachment, PermissionRequest};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap},
    Frame,
};
use tokio::sync::mpsc;
use unicode_width::UnicodeWidthStr;

use crate::input::command_description;

use self::messages::{Message, MessageContent};
use self::permission::PendingPermission;
use self::status::{ToolInfo, TuiStatusState};

// -- App State ----------------------------------------------------------------

struct App {
    messages: Vec<Message>,
    scroll_offset: usize,
    auto_scroll: bool,
    input: InputWidget,
    status: TuiStatusState,
    permission: Option<PendingPermission>,
    bottom_bar_hidden: bool,
    thinking_collapsed: bool,
    running: bool,
    total_turns: u32,
    total_input_tokens: u64,
    total_output_tokens: u64,
    model: String,
    pending_images: Vec<ImageAttachment>,
}

impl App {
    fn new(model: String) -> Self {
        Self {
            messages: Vec::new(),
            scroll_offset: 0,
            auto_scroll: true,
            input: InputWidget::new(),
            status: TuiStatusState::new(),
            permission: None,
            bottom_bar_hidden: false,
            thinking_collapsed: true,
            running: true,
            total_turns: 0,
            total_input_tokens: 0,
            total_output_tokens: 0,
            model,
            pending_images: Vec::new(),
        }
    }

    fn push_message(&mut self, content: MessageContent) {
        self.messages.push(Message::new(content));
        if self.auto_scroll {
            self.scroll_offset = 0;
        }
    }

    /// Append text to the last AssistantText message, or create one.
    fn append_assistant_text(&mut self, text: &str) {
        if let Some(msg) = self.messages.last_mut() {
            if let MessageContent::AssistantText(ref mut buf) = msg.content {
                buf.push_str(text);
                msg.invalidate_cache();
                if self.auto_scroll {
                    self.scroll_offset = 0;
                }
                return;
            }
        }
        self.push_message(MessageContent::AssistantText(text.to_string()));
    }

    /// Append text to the last ThinkingText message, or create one.
    fn append_thinking_text(&mut self, text: &str) {
        if let Some(msg) = self.messages.last_mut() {
            if let MessageContent::ThinkingText(ref mut buf) = msg.content {
                buf.push_str(text);
                msg.invalidate_cache();
                if self.auto_scroll {
                    self.scroll_offset = 0;
                }
                return;
            }
        }
        self.push_message(MessageContent::ThinkingText(text.to_string()));
    }

    fn handle_notification(&mut self, notification: AgentNotification) {
        match notification {
            AgentNotification::TextDelta { text } => {
                self.status.thinking = false;
                self.append_assistant_text(&text);
            }
            AgentNotification::ThinkingDelta { text } => {
                self.status.thinking = true;
                self.append_thinking_text(&text);
            }
            AgentNotification::ToolUseStart { tool_name, .. } => {
                self.status.active_tools.insert(
                    tool_name.clone(),
                    ToolInfo {
                        name: tool_name.clone(),
                        started: Instant::now(),
                    },
                );
                self.push_message(MessageContent::ToolUseStart {
                    name: tool_name,
                });
            }
            AgentNotification::ToolUseComplete {
                tool_name,
                is_error,
                result_preview,
                ..
            } => {
                self.status.active_tools.remove(&tool_name);
                let preview = result_preview.unwrap_or_default();
                // Store full_result only when preview is substantial enough to warrant collapsing
                let full_result = if preview.len() > 200 || preview.lines().count() > 3 {
                    Some(preview.clone())
                } else {
                    None
                };
                self.push_message(MessageContent::ToolResult {
                    name: tool_name,
                    preview,
                    full_result,
                    is_error,
                });
            }
            AgentNotification::TurnComplete { turn, usage, .. } => {
                self.total_turns = turn;
                self.total_input_tokens += usage.input_tokens;
                self.total_output_tokens += usage.output_tokens;
                self.status.thinking = false;
                self.push_message(MessageContent::TurnDivider {
                    turn,
                    input_tokens: usage.input_tokens,
                    output_tokens: usage.output_tokens,
                });
            }
            AgentNotification::TurnStart { turn } => {
                self.status.thinking = true;
                self.push_message(MessageContent::System(format!("\u{2500}\u{2500} turn {turn} \u{2500}\u{2500}")));
            }
            AgentNotification::AgentSpawned { agent_id, name, .. } => {
                let label = name.unwrap_or_else(|| {
                    agent_id.chars().take(8).collect::<String>()
                });
                self.push_message(MessageContent::System(
                    format!("\u{1F916} Agent spawned: {label}"),
                ));
                self.status.active_agents.insert(agent_id, label);
            }
            AgentNotification::AgentComplete {
                agent_id,
                result,
                is_error,
            } => {
                self.status.active_agents.remove(&agent_id);
                let icon = if is_error { "\u{2717}" } else { "\u{2713}" };
                self.push_message(MessageContent::System(
                    format!("{icon} Agent finished: {result}"),
                ));
            }
            AgentNotification::AgentTerminated { agent_id, reason } => {
                self.status.active_agents.remove(&agent_id);
                self.push_message(MessageContent::System(
                    format!("\u{26A0} Agent terminated: {reason}"),
                ));
            }
            AgentNotification::SessionEnd { reason } => {
                self.push_message(MessageContent::System(
                    format!("Session ended: {reason}"),
                ));
            }
            AgentNotification::CompactStart => {
                self.push_message(MessageContent::System(
                    "\u{27F3} Compacting context...".to_string(),
                ));
            }
            AgentNotification::CompactComplete { .. } => {
                self.push_message(MessageContent::System(
                    "Context compacted".to_string(),
                ));
            }
            AgentNotification::Error { message, .. } => {
                self.push_message(MessageContent::System(
                    format!("\u{2717} Error: {message}"),
                ));
            }
            AgentNotification::ModelChanged {
                model,
                display_name,
            } => {
                self.model = model;
                self.push_message(MessageContent::System(
                    format!("Model: {display_name}"),
                ));
            }
            // Notifications that don't produce visible output
            AgentNotification::ToolUseReady { .. }
            | AgentNotification::ToolSelected { .. }
            | AgentNotification::AssistantMessage { .. }
            | AgentNotification::SessionStart { .. }
            | AgentNotification::SessionStatus { .. }
            | AgentNotification::SessionSaved { .. }
            | AgentNotification::HistoryCleared
            | AgentNotification::ContextWarning { .. }
            | AgentNotification::MemoryExtracted { .. }
            | AgentNotification::AgentProgress { .. }
            | AgentNotification::McpServerConnected { .. }
            | AgentNotification::McpServerDisconnected { .. }
            | AgentNotification::McpServerError { .. }
            | AgentNotification::McpServerList { .. }
            | AgentNotification::ModelList { .. }
            | AgentNotification::ToolList { .. }
            | AgentNotification::ThinkingChanged { .. }
            | AgentNotification::CacheBreakSet
            | AgentNotification::SwarmTeamCreated { .. }
            | AgentNotification::SwarmTeamDeleted { .. }
            | AgentNotification::SwarmAgentSpawned { .. }
            | AgentNotification::SwarmAgentTerminated { .. }
            | AgentNotification::SwarmAgentQuery { .. }
            | AgentNotification::SwarmAgentReply { .. }
            | AgentNotification::ConflictDetected { .. } => {}
        }
    }

    fn handle_slash_command(&mut self, client: &ClientHandle, cmd: &str) {
        match cmd {
            "/help" => {
                self.push_message(MessageContent::System(
                    concat!(
                        "Available commands:\n",
                        "  /help        Show this help\n",
                        "  /clear       Clear the output\n",
                        "  /compact     Compact the context\n",
                        "  /status      Show session info\n",
                        "  /model       Show current model\n",
                        "  /sessions    List saved sessions\n",
                        "  /resume      Resume latest session\n",
                        "  /resume <id> Resume specific session\n",
                        "  /exit        Quit TUI\n",
                        "  /abort       Abort current operation\n",
                        "\n",
                        "  Key bindings:\n",
                        "  Tab          Command completion\n",
                        "  Shift+Enter  Insert newline\n",
                        "  Up/Down      History / cursor navigation\n",
                        "  Shift+Up/Dn  Scroll back/forward\n",
                        "  PageUp/Dn    Scroll 10 lines\n",
                        "  Ctrl+V       Paste clipboard image\n",
                        "  Ctrl+O       Toggle thinking blocks\n",
                        "  Ctrl+E       Expand/collapse tool result\n",
                        "  Ctrl+T       Toggle bottom bar\n",
                        "  Ctrl+L       Clear output\n",
                        "  Ctrl+C       Abort / Quit\n",
                        "  Esc          Abort / Quit"
                    ).to_string(),
                ));
            }
            "/clear" | "/history" => {
                self.messages.clear();
                self.scroll_offset = 0;
            }
            "/compact" => {
                let _ = client
                    .send_request(clawed_bus::events::AgentRequest::Compact { instructions: None });
            }
            "/status" => {
                let elapsed = self.status.session_start.elapsed();
                let secs = elapsed.as_secs();
                self.push_message(MessageContent::System(format!(
                    "Model: {} | Turns: {} | Tokens: {}\u{2191} {}\u{2193} | Elapsed: {}s",
                    self.model,
                    self.total_turns,
                    self.total_input_tokens,
                    self.total_output_tokens,
                    secs,
                )));
            }
            "/model" => {
                self.push_message(MessageContent::System(
                    format!("Current model: {}", self.model),
                ));
            }
            "/abort" => {
                let _ = client.abort();
                self.status.thinking = false;
                self.push_message(MessageContent::System("[Aborted]".to_string()));
            }
            "/exit" | "/quit" => {
                self.running = false;
            }
            other => {
                self.push_message(MessageContent::System(
                    format!("Unknown command: {other}. Type /help for commands."),
                ));
            }
        }
    }
}

// -- Rendering ----------------------------------------------------------------

fn render(frame: &mut Frame, app: &App) {
    let area = frame.area();

    let has_permission = app.permission.is_some();

    // Build vertical layout constraints
    let bottom_bar_rows = if has_permission {
        0
    } else {
        u16::from(!app.bottom_bar_hidden)
    };
    let status_rows = u16::from(
        app.status.should_show() || app.total_input_tokens + app.total_output_tokens > 0,
    );

    // When permission is active, the input row + hint bar are replaced by
    // a 3-row permission prompt (description + buttons + hints).
    let input_rows = app.input.visible_rows();
    let footer_rows = if has_permission {
        permission::PERM_ROWS
    } else {
        input_rows + bottom_bar_rows
    };

    let constraints = [
        Constraint::Min(1),                 // messages
        Constraint::Length(1),              // separator
        Constraint::Length(status_rows),    // status (0 or 1)
        Constraint::Length(footer_rows),    // input/permission footer
    ];

    let chunks = Layout::vertical(constraints).split(area);
    let msg_area = chunks[0];
    let sep_area = chunks[1];
    let status_area = chunks[2];
    let footer_area = chunks[3];

    render_messages(frame, msg_area, app);
    render_separator(frame, sep_area, app.scroll_offset);

    if status_rows > 0 {
        status::render(
            frame,
            status_area,
            &app.status,
            &app.model,
            app.total_input_tokens,
            app.total_output_tokens,
        );
    }

    if let Some(ref perm) = app.permission {
        // Permission prompt: split footer into 3 rows
        let perm_chunks = Layout::vertical([
            Constraint::Length(1), // description
            Constraint::Length(1), // buttons
            Constraint::Length(1), // hints
        ])
        .split(footer_area);
        permission::render(frame, perm_chunks[0], perm_chunks[1], perm_chunks[2], perm);
    } else {
        // Normal: input + optional hint bar
        let input_chunks = Layout::vertical([
            Constraint::Length(input_rows),      // input (1–5 rows)
            Constraint::Length(bottom_bar_rows), // hint bar
        ])
        .split(footer_area);

        render_input(frame, input_chunks[0], app);
        if bottom_bar_rows > 0 {
            bottombar::render(frame, input_chunks[1]);
        }

        // Completion popup (rendered last so it draws on top)
        if app.input.in_completion() {
            render_completion_popup(frame, input_chunks[0], app);
        }
    }
}

fn render_messages(frame: &mut Frame, area: Rect, app: &App) {
    if area.height == 0 || area.width == 0 {
        return;
    }

    // Collect all lines from all messages
    let all_lines: Vec<Line> = if app.messages.is_empty() {
        render_welcome_lines(area.width)
    } else {
        app.messages
            .iter()
            .flat_map(|msg| {
                if app.thinking_collapsed {
                    if let MessageContent::ThinkingText(text) = &msg.content {
                        if text.is_empty() {
                            return vec![];
                        }
                        let line_count = text.lines().count();
                        return vec![Line::styled(
                            format!("\u{25B6} thinking ({line_count} lines, Ctrl+O to expand)"),
                            Style::default()
                                .fg(Color::DarkGray)
                                .add_modifier(Modifier::ITALIC),
                        )];
                    }
                }
                msg.to_lines()
            })
            .collect()
    };

    let viewport_height = area.height as usize;
    let total_lines = all_lines.len();

    // Calculate scroll position (scroll_offset=0 means at bottom)
    let start = if total_lines <= viewport_height {
        0
    } else {
        let max_scroll = total_lines - viewport_height;
        let offset = app.scroll_offset.min(max_scroll);
        max_scroll - offset
    };

    let visible: Vec<Line> = all_lines
        .into_iter()
        .skip(start)
        .take(viewport_height)
        .collect();

    let paragraph = Paragraph::new(visible).wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
}

fn render_separator(frame: &mut Frame, area: Rect, scroll_offset: usize) {
    if scroll_offset > 0 {
        // Show scroll indicator
        let indicator = format!(" \u{2191} {scroll_offset} lines up ");
        let pad_len = (area.width as usize).saturating_sub(indicator.len());
        let left = pad_len / 2;
        let right = pad_len - left;
        let line = Line::from(vec![
            Span::styled(
                "\u{2500}".repeat(left),
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled(
                indicator,
                Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "\u{2500}".repeat(right),
                Style::default().fg(Color::DarkGray),
            ),
        ]);
        frame.render_widget(Paragraph::new(line), area);
    } else {
        let sep = "\u{2500}".repeat(area.width as usize);
        let line = Line::styled(sep, Style::default().fg(Color::DarkGray));
        frame.render_widget(Paragraph::new(line), area);
    }
}

fn render_input(frame: &mut Frame, area: Rect, app: &App) {
    let prompt_style = Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD);
    let text_style = Style::default();
    let image_style = Style::default().fg(Color::Magenta);

    let display_lines = app.input.display_lines();
    let img_count = app.pending_images.len();
    let lines: Vec<Line> = display_lines
        .iter()
        .enumerate()
        .map(|(i, line_text)| {
            if i == 0 {
                let mut spans = vec![
                    Span::styled("> ", prompt_style),
                    Span::styled((*line_text).to_string(), text_style),
                ];
                if img_count > 0 {
                    spans.push(Span::styled(
                        format!(" 📎{img_count}"),
                        image_style,
                    ));
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

    // Position cursor
    let (cursor_row, cursor_col) = app.input.cursor_position();
    let x = area.x + 2 + (cursor_col as u16).min(area.width.saturating_sub(3));
    let y = area.y + (cursor_row as u16).min(area.height.saturating_sub(1));
    frame.set_cursor_position((x, y));
}

fn render_completion_popup(frame: &mut Frame, input_area: Rect, app: &App) {
    let matches = app.input.completion_matches();
    if matches.len() <= 1 {
        return;
    }

    let selected = app.input.completion_selected();
    let max_items = 8.min(matches.len());

    // Calculate popup dimensions
    let max_cmd_width = matches.iter().map(|c| c.width()).max().unwrap_or(4);
    let popup_width = (max_cmd_width + 30).min(input_area.width as usize);
    let popup_height = max_items as u16 + 2; // +2 for borders

    // Position popup above input line
    let popup_y = input_area.y.saturating_sub(popup_height);
    let popup_x = input_area.x + 2; // Align with text after "> "
    let popup_area = Rect::new(
        popup_x.min(input_area.right().saturating_sub(popup_width as u16)),
        popup_y,
        popup_width as u16,
        popup_height,
    );

    // Build list items
    let items: Vec<ListItem> = matches
        .iter()
        .enumerate()
        .take(max_items)
        .map(|(i, cmd)| {
            let desc = command_description(cmd);
            let is_selected = i == selected;
            let style = if is_selected {
                Style::default()
                    .fg(Color::White)
                    .bg(Color::Blue)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Cyan)
            };
            let desc_style = if is_selected {
                Style::default().fg(Color::LightCyan).bg(Color::Blue)
            } else {
                Style::default().fg(Color::DarkGray)
            };
            let padding = " ".repeat(max_cmd_width.saturating_sub(cmd.width()));
            ListItem::new(Line::from(vec![
                Span::styled((*cmd).to_string(), style),
                Span::raw(padding),
                Span::styled(format!("  {desc}"), desc_style),
            ]))
        })
        .collect();

    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).border_style(
            Style::default().fg(Color::DarkGray),
        ));

    // Clear the area first, then render
    frame.render_widget(Clear, popup_area);
    frame.render_widget(list, popup_area);
}

fn render_welcome_lines(width: u16) -> Vec<Line<'static>> {
    let model_text = "Clawed Code TUI";
    let hints = "Tab: complete  \u{2191}\u{2193}: history  Ctrl+C: abort/quit  /help: commands";

    let border_style = Style::default().fg(Color::Cyan);
    let text_style = Style::default().fg(Color::White).add_modifier(Modifier::BOLD);
    let hint_style = Style::default().fg(Color::DarkGray);

    let inner_width = model_text
        .width()
        .max(hints.width())
        .min((width as usize).saturating_sub(4));
    let top = format!("\u{250C}{}\u{2510}", "\u{2500}".repeat(inner_width + 2));
    let bot = format!("\u{2514}{}\u{2518}", "\u{2500}".repeat(inner_width + 2));

    let center = |s: &str| -> String {
        let sw = s.width().min(inner_width);
        let left = (inner_width - sw) / 2;
        let right = inner_width - sw - left;
        format!("{}{}{}", " ".repeat(left), s, " ".repeat(right))
    };

    vec![
        Line::from(""),
        Line::styled(top, border_style),
        Line::from(vec![
            Span::styled("\u{2502} ", border_style),
            Span::styled(center(model_text), text_style),
            Span::styled(" \u{2502}", border_style),
        ]),
        Line::from(vec![
            Span::styled("\u{2502} ", border_style),
            Span::styled(center(hints), hint_style),
            Span::styled(" \u{2502}", border_style),
        ]),
        Line::styled(bot, border_style),
        Line::from(""),
    ]
}

// -- Public entry point -------------------------------------------------------

/// Run the full-screen TUI.
pub async fn run_tui(
    client: ClientHandle,
    engine: Arc<QueryEngine>,
    _cwd: std::path::PathBuf,
) -> anyhow::Result<()> {
    let model = { engine.state().read().await.model.clone() };
    let mut app = App::new(model);

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

    // Spawn notification forwarder: async recv from broadcast -> sync mpsc
    let mut notify_sub = client.subscribe_notifications();
    let (notify_tx, mut notify_rx) = mpsc::channel::<AgentNotification>(256);
    let forwarder = tokio::spawn(async move {
        while let Ok(notification) = notify_sub.recv().await {
            if notify_tx.send(notification).await.is_err() {
                break;
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

    // Initialize ratatui terminal (handles raw mode + alternate screen)
    let mut terminal = ratatui::init();

    // Main event loop
    while app.running {
        // Render
        terminal.draw(|frame| render(frame, &app))?;

        // Non-blocking input poll
        if event::poll(std::time::Duration::from_millis(100))? {
            match event::read()? {
                Event::Key(key) => {
                    if key.kind != KeyEventKind::Press && key.kind != KeyEventKind::Repeat {
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
                                        if resp.remember { "Allowed (always)" } else { "Allowed" }
                                    } else {
                                        "Denied"
                                    };
                                    app.push_message(MessageContent::System(
                                        format!("{label}: {} — {}", perm.request.tool_name, perm.request.description),
                                    ));
                                    let _ = client.send_permission_response(resp);
                                }
                            }
                            KeyCode::Esc => {
                                if let Some(perm) = app.permission.take() {
                                    let resp = perm.deny_response();
                                    app.push_message(MessageContent::System(
                                        format!("Denied: {} — {}", perm.request.tool_name, perm.request.description),
                                    ));
                                    let _ = client.send_permission_response(resp);
                                }
                            }
                            _ => {} // ignore other keys during permission prompt
                        }
                        continue;
                    }

                    // Global shortcuts
                    match (key.code, key.modifiers) {
                        (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                            if app.status.thinking {
                                let _ = client.abort();
                                app.status.thinking = false;
                                app.push_message(MessageContent::System(
                                    "[Aborted]".to_string(),
                                ));
                            } else {
                                app.running = false;
                            }
                            continue;
                        }
                        (KeyCode::Char('t'), KeyModifiers::CONTROL) => {
                            app.bottom_bar_hidden = !app.bottom_bar_hidden;
                            continue;
                        }
                        (KeyCode::Char('o'), KeyModifiers::CONTROL) => {
                            app.thinking_collapsed = !app.thinking_collapsed;
                            // Invalidate caches of all thinking messages
                            for msg in &app.messages {
                                if matches!(msg.content, MessageContent::ThinkingText(_)) {
                                    msg.invalidate_cache();
                                }
                            }
                            continue;
                        }
                        (KeyCode::Char('e'), KeyModifiers::CONTROL) => {
                            // Toggle expand/collapse on the last collapsible tool result
                            if let Some(msg) = app
                                .messages
                                .iter_mut()
                                .rev()
                                .find(|m| m.is_collapsible())
                            {
                                msg.toggle_collapsed();
                            }
                            continue;
                        }
                        (KeyCode::Char('l'), KeyModifiers::CONTROL) => {
                            app.messages.clear();
                            app.scroll_offset = 0;
                            continue;
                        }
                        // Scroll back
                        (KeyCode::PageUp, _) | (KeyCode::Up, KeyModifiers::SHIFT) => {
                            let step = if key.code == KeyCode::PageUp { 10 } else { 1 };
                            app.scroll_offset = app.scroll_offset.saturating_add(step);
                            app.auto_scroll = false;
                            continue;
                        }
                        (KeyCode::PageDown, _) | (KeyCode::Down, KeyModifiers::SHIFT) => {
                            let step = if key.code == KeyCode::PageDown { 10 } else { 1 };
                            if app.scroll_offset > 0 {
                                app.scroll_offset = app.scroll_offset.saturating_sub(step);
                                if app.scroll_offset == 0 {
                                    app.auto_scroll = true;
                                }
                            }
                            continue;
                        }
                        (KeyCode::Char('v'), KeyModifiers::CONTROL) => {
                            match read_clipboard_image() {
                                Ok(attachment) => {
                                    app.pending_images.push(attachment);
                                    app.push_message(MessageContent::System(
                                        format!("📎 Image attached ({} total)", app.pending_images.len()),
                                    ));
                                }
                                Err(e) => {
                                    app.push_message(MessageContent::System(
                                        format!("Clipboard: {e}"),
                                    ));
                                }
                            }
                            continue;
                        }
                        _ => {}
                    }

                    let action = app.input.handle_key(key);
                    match action {
                        input::InputAction::Submit => {
                            let text = app.input.take_text();
                            if !text.is_empty() || !app.pending_images.is_empty() {
                                let display = if app.pending_images.is_empty() {
                                    text.clone()
                                } else {
                                    format!("{text} [+{} image(s)]", app.pending_images.len())
                                };
                                app.push_message(MessageContent::UserInput(display));

                                if text.starts_with('/') {
                                    if text == "/sessions" {
                                        let sessions = clawed_core::session::list_sessions();
                                        if sessions.is_empty() {
                                            app.push_message(MessageContent::System(
                                                "No saved sessions.".to_string(),
                                            ));
                                        } else {
                                            let mut lines = String::from("Recent sessions:\n");
                                            for (i, s) in sessions.iter().take(10).enumerate() {
                                                let title = s.custom_title.as_deref()
                                                    .or(s.last_prompt.as_deref())
                                                    .unwrap_or(&s.title);
                                                let age = chrono::Utc::now()
                                                    .signed_duration_since(s.updated_at);
                                                let age_str = format_duration(age);
                                                lines.push_str(&format!(
                                                    "  {}: {} ({}, {} turns, {})\n",
                                                    i + 1, title, s.id, s.turn_count, age_str,
                                                ));
                                            }
                                            lines.push_str("\nUse /resume <id> to restore.");
                                            app.push_message(MessageContent::System(lines));
                                        }
                                    } else if text == "/resume" {
                                        // Resume latest session
                                        let sessions = clawed_core::session::list_sessions();
                                        if let Some(latest) = sessions.first() {
                                            let sid = latest.id.clone();
                                            match engine.restore_session(&sid).await {
                                                Ok(title) => {
                                                    app.push_message(MessageContent::System(
                                                        format!("Resumed session: {title}"),
                                                    ));
                                                    replay_session_messages(&engine, &mut app).await;
                                                }
                                                Err(e) => {
                                                    app.push_message(MessageContent::System(
                                                        format!("Resume failed: {e}"),
                                                    ));
                                                }
                                            }
                                        } else {
                                            app.push_message(MessageContent::System(
                                                "No sessions to resume.".to_string(),
                                            ));
                                        }
                                    } else if let Some(sid) = text.strip_prefix("/resume ") {
                                        let sid = sid.trim();
                                        match engine.restore_session(sid).await {
                                            Ok(title) => {
                                                app.push_message(MessageContent::System(
                                                    format!("Resumed session: {title}"),
                                                ));
                                                replay_session_messages(&engine, &mut app).await;
                                            }
                                            Err(e) => {
                                                app.push_message(MessageContent::System(
                                                    format!("Resume failed: {e}"),
                                                ));
                                            }
                                        }
                                    } else {
                                        let client_ref = &client;
                                        app.handle_slash_command(client_ref, &text);
                                    }
                                    app.pending_images.clear();
                                } else {
                                    let images = std::mem::take(&mut app.pending_images);
                                    if images.is_empty() {
                                        let _ = client.submit(&text);
                                    } else {
                                        let _ = client.submit_with_images(&text, images);
                                    }
                                    app.status.thinking = true;
                                }
                            }
                        }
                        input::InputAction::Abort => {
                            let _ = client.abort();
                            app.status.thinking = false;
                            app.push_message(MessageContent::System(
                                "[Aborted]".to_string(),
                            ));
                        }
                        input::InputAction::Quit => {
                            app.running = false;
                        }
                        input::InputAction::Changed | input::InputAction::None => {}
                    }
                }
                Event::Resize(_, _) => {
                    // ratatui handles resize automatically on next draw
                }
                _ => {} // Mouse, Focus, Paste -- ignored
            }
        }

        // Drain notification channel
        while let Ok(notification) = notify_rx.try_recv() {
            app.handle_notification(notification);
        }

        // Check for incoming permission requests
        while let Ok(req) = perm_rx.try_recv() {
            app.push_message(MessageContent::System(format!(
                "\u{1F512} Permission required: {} — {}",
                req.tool_name, req.description,
            )));
            app.permission = Some(PendingPermission::new(req));
        }
    }

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

    // Restore terminal (ratatui handles raw mode + alternate screen)
    ratatui::restore();
    Ok(())
}

// -- Clipboard image support --------------------------------------------------

/// Read an image from the system clipboard and return it as an `ImageAttachment`.
///
/// Uses `arboard` for cross-platform clipboard access. The image is encoded as
/// PNG and base64-encoded for the Anthropic API.
fn read_clipboard_image() -> anyhow::Result<ImageAttachment> {
    use anyhow::Context as _;
    use base64::Engine as _;

    let mut clip = arboard::Clipboard::new()
        .context("Cannot open clipboard")?;

    let img = clip.get_image()
        .context("No image in clipboard")?;

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

    app.messages.clear();
    app.scroll_offset = 0;

    let state = engine.state().read().await;
    app.model = state.model.clone();
    app.total_turns = state.turn_count;
    app.total_input_tokens = state.total_input_tokens;
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
                        ContentBlock::Thinking { thinking } => {
                            app.push_message(MessageContent::ThinkingText(thinking.clone()));
                        }
                        ContentBlock::ToolUse { name, .. } => {
                            app.push_message(MessageContent::ToolUseStart {
                                name: name.clone(),
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

/// Format a chrono Duration as a human-readable string.
fn format_duration(dur: chrono::Duration) -> String {
    let secs = dur.num_seconds();
    if secs < 60 {
        format!("{secs}s ago")
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86400 {
        format!("{}h ago", secs / 3600)
    } else {
        format!("{}d ago", secs / 86400)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn welcome_lines_are_nonempty() {
        let lines = render_welcome_lines(80);
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
}
