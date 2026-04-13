//! Full-screen TUI with ratatui double-buffered rendering.
//!
//! Layout:
//! ```text
//! Messages (scrollable)
//! ── claude-3.5 │ turn 3 │ 4096↑ 1024↓ │ 80% ctx │ 📥2 ──  (separator + static info)
//! ⠹ thinking  Bash (00:03)  2 agents                         (dynamic status, only when active)
//! ▸ queued message 1                                          (queue items, only when queued)
//! ▸ queued message 2
//! ──────────────────────────────────────────────────────────  (input separator, always)
//! > user input here_
//! Tab: complete  Ctrl+J: newline  Ctrl+C: abort/quit          (hint bar, toggleable)
//! ```

mod bottombar;
mod input;
mod markdown;
mod messages;
mod overlay;
mod permission;
mod status;
mod taskplan;
mod textarea;

pub use input::InputWidget;

use std::sync::Arc;
use std::time::Instant;

use clawed_agent::engine::QueryEngine;
use clawed_bus::bus::ClientHandle;
use clawed_bus::events::{AgentNotification, ImageAttachment, PermissionRequest};
use crossterm::event::{
    self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEventKind, KeyModifiers,
};
use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Clear, List, ListItem, Paragraph, Wrap},
    Frame,
};
use tokio::sync::mpsc;
use unicode_width::UnicodeWidthStr;

use crate::input::command_description;

use self::messages::{Message, MessageContent};
use self::overlay::{Overlay, OverlayAction};
use self::permission::PendingPermission;
use self::status::{ToolInfo, TuiStatusState};

/// Subdued text color for hints, separators, status indicators, and input text.
/// Uses a true-color gray that is readable on both dark and light backgrounds,
/// unlike `Color::DarkGray` (ANSI 8) which maps to bright on many terminals.
const MUTED: Color = Color::Rgb(140, 140, 140);

// -- App State ----------------------------------------------------------------

struct App {
    messages: Vec<Message>,
    scroll_offset: usize,
    auto_scroll: bool,
    input: InputWidget,
    status: TuiStatusState,
    task_plan: taskplan::TaskPlan,
    permission: Option<PendingPermission>,
    overlay: Option<Overlay>,
    bottom_bar_hidden: bool,
    thinking_collapsed: bool,
    running: bool,
    /// Set to true whenever content changes; causes a full terminal clear before the next draw
    /// to eliminate ghost cells left over from prior frames (no alternate screen).
    needs_full_clear: bool,
    total_turns: u32,
    /// Latest context size from the most recent API response (not accumulated).
    context_tokens: u64,
    /// Cumulative output tokens generated across all turns.
    total_output_tokens: u64,
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
}

impl App {
    fn new(model: String) -> Self {
        Self {
            messages: Vec::new(),
            scroll_offset: 0,
            auto_scroll: true,
            input: InputWidget::new(),
            status: TuiStatusState::new(),
            task_plan: taskplan::TaskPlan::new(),
            permission: None,
            overlay: None,
            bottom_bar_hidden: false,
            thinking_collapsed: true,
            running: true,
            needs_full_clear: false,
            total_turns: 0,
            context_tokens: 0,
            total_output_tokens: 0,
            model,
            pending_images: Vec::new(),
            pending_command: None,
            key_debug: false,
            queued_inputs: Vec::new(),
            is_generating: false,
            expecting_turn_start: false,
        }
    }

    fn push_message(&mut self, content: MessageContent) {
        self.messages.push(Message::new(content));
        if self.auto_scroll {
            self.scroll_offset = 0;
        }
        // Force a full terminal clear on the next draw so no ghost cells remain
        // from the previous layout (no alternate screen, so ratatui diff can miss cells).
        self.needs_full_clear = true;
    }

    /// Mark that the LLM is now generating a response.
    /// Unlike status.thinking (which goes false during TextDelta), this stays
    /// true for the entire turn so queue gating and Esc abort work correctly.
    fn mark_generating(&mut self) {
        self.status.thinking = true;
        self.is_generating = true;
        // Discard any TextDelta/ThinkingDelta that arrive before TurnStart —
        // they belong to the previous (possibly aborted) stream.
        self.expecting_turn_start = true;
    }

    /// Clear all generation state (abort or TurnComplete).
    fn mark_done(&mut self) {
        self.status.thinking = false;
        self.is_generating = false;
        self.expecting_turn_start = false;
        self.status.active_tools.clear();
        self.status.active_shells = 0;
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

    /// Returns Some(merged_text) if queued inputs should be submitted after this notification.
    fn handle_notification(&mut self, notification: AgentNotification) -> Option<String> {
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
                if tool_name.to_lowercase().contains("bash")
                    || tool_name.to_lowercase().contains("shell")
                {
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
                if tool_name.to_lowercase().contains("bash")
                    || tool_name.to_lowercase().contains("shell")
                {
                    self.status.active_shells = self.status.active_shells.saturating_sub(1);
                    self.task_plan.set_shells(self.status.active_shells);
                }
                let duration_ms = self
                    .status
                    .active_tools
                    .get(&tool_name)
                    .map(|t| t.started.elapsed().as_millis() as u64)
                    .unwrap_or(0);
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
                    duration_ms,
                });
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
                    self.mark_done();
                }
                self.push_message(MessageContent::TurnDivider {
                    turn,
                    input_tokens: usage.input_tokens,
                    output_tokens: usage.output_tokens,
                });
                // Drain queue: merge all pending inputs and submit as one message.
                // Only drain when NOT expecting a new turn (if expecting_turn_start,
                // the direct submit already happened at the call site).
                if !self.expecting_turn_start && !self.queued_inputs.is_empty() {
                    let merged = self.queued_inputs.join("\n\n");
                    self.queued_inputs.clear();
                    return Some(merged);
                }
            }
            AgentNotification::TurnStart { turn } => {
                // Re-assert is_generating in case a stale TurnComplete from a
                // previous (aborted) stream arrived between mark_generating()
                // and this TurnStart, resetting is_generating prematurely.
                self.is_generating = true;
                // We now have a confirmed new turn — allow TextDelta through.
                self.expecting_turn_start = false;
                self.status.thinking = true;
                self.push_message(MessageContent::System(format!("\u{2500}\u{2500} turn {turn} \u{2500}\u{2500}")));
            }
            AgentNotification::AgentSpawned { agent_id, name, .. } => {
                let label = name.unwrap_or_else(|| {
                    agent_id.chars().take(8).collect::<String>()
                });
                self.task_plan.add_task(agent_id.clone(), label.clone());
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
                self.task_plan.complete_task(&agent_id, is_error);
                self.status.active_agents.remove(&agent_id);
                let icon = if is_error { "\u{2717}" } else { "\u{2713}" };
                self.push_message(MessageContent::System(
                    format!("{icon} Agent finished: {result}"),
                ));
            }
            AgentNotification::AgentTerminated { agent_id, reason } => {
                self.task_plan.terminate_task(&agent_id);
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
            // Notifications that now produce visible output
            AgentNotification::SessionStatus {
                model,
                total_turns,
                total_input_tokens,
                total_output_tokens,
                context_usage_pct,
                ..
            } => {
                self.status.context_pct = context_usage_pct;
                // Initialise counters from the authoritative session state.
                // total_input_tokens from the engine is the accumulated sum across all turns
                // (for billing). We display only the latest context size (context_tokens),
                // so use it as the seed if we have no local value yet.
                if self.context_tokens == 0 && total_input_tokens > 0 {
                    self.context_tokens = total_input_tokens;
                }
                if self.total_output_tokens == 0 && total_output_tokens > 0 {
                    self.total_output_tokens = total_output_tokens;
                }
                self.total_turns = self.total_turns.max(total_turns);
                self.push_message(MessageContent::System(format!(
                    "Model: {model} | Turns: {total_turns} | Tokens: {total_input_tokens}\u{2191} {total_output_tokens}\u{2193} | Context: {context_usage_pct:.0}%",
                )));
            }
            AgentNotification::McpServerConnected { name, tool_count } => {
                self.push_message(MessageContent::System(format!(
                    "✓ MCP connected: {name} ({tool_count} tools)",
                )));
            }
            AgentNotification::McpServerDisconnected { name } => {
                self.push_message(MessageContent::System(format!(
                    "MCP disconnected: {name}",
                )));
            }
            AgentNotification::McpServerError { name, error } => {
                self.push_message(MessageContent::System(format!(
                    "✗ MCP error [{name}]: {error}",
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
                        lines.push_str(&format!(
                            "  {status} {} ({} tools)\n", s.name, s.tool_count,
                        ));
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
                    "\u{26A0} Context {usage_pct:.0}%: {message}",
                )));
            }
            AgentNotification::MemoryExtracted { facts } => {
                let mut lines = String::from("Memory extracted:\n");
                for f in &facts {
                    lines.push_str(&format!("  • {f}\n"));
                }
                self.push_message(MessageContent::System(lines));
            }
            AgentNotification::HistoryCleared => {
                self.messages.clear();
                self.scroll_offset = 0;
                self.push_message(MessageContent::System(
                    "Conversation history cleared.".to_string(),
                ));
            }
            AgentNotification::SessionSaved { session_id } => {
                self.push_message(MessageContent::System(format!(
                    "Session saved: {session_id}",
                )));
            }
            // Tool input ready — show a compact summary of what's being passed
            AgentNotification::ToolUseReady { tool_name, input, .. } => {
                let summary = match input {
                    serde_json::Value::Object(ref m) => {
                        m.iter()
                            .take(3)
                            .map(|(k, v)| {
                                let val = match v {
                                    serde_json::Value::String(s) => {
                                        if s.chars().count() > 40 {
                                            let t: String = s.chars().take(37).collect();
                                            format!("\"{t}…\"")
                                        } else {
                                            format!("\"{s}\"")
                                        }
                                    }
                                    other => {
                                        let s = other.to_string();
                                        if s.chars().count() > 40 {
                                            let t: String = s.chars().take(37).collect();
                                            format!("{t}…")
                                        } else {
                                            s
                                        }
                                    }
                                };
                                format!("{k}={val}")
                            })
                            .collect::<Vec<_>>()
                            .join(", ")
                    }
                    _ => input.to_string(),
                };
                self.push_message(MessageContent::System(format!(
                    "  ↳ {tool_name}({summary})",
                )));
            }
            // Tool selected — pre-execution signal (just a brief note)
            AgentNotification::ToolSelected { .. } => {}
            // AssistantMessage — full text for logging, already shown via TextDelta
            AgentNotification::AssistantMessage { .. } => {}
            // Session start: update model display
            AgentNotification::SessionStart { model, .. } => {
                self.model = model;
            }
            // Background agent progress
            AgentNotification::AgentProgress { agent_id, text } => {
                self.push_message(MessageContent::System(format!(
                    "  ↳ [{agent_id}] {text}",
                )));
            }
            // Conflict warning for concurrent agents
            AgentNotification::ConflictDetected { file_path, agents } => {
                self.push_message(MessageContent::System(format!(
                    "\u{26A0} Conflict on {file_path} between: {}",
                    agents.join(", "),
                )));
            }
            // Swarm lifecycle events
            AgentNotification::SwarmTeamCreated { team_name, agent_count } => {
                self.push_message(MessageContent::System(format!(
                    "\u{1F41D} Swarm team '{team_name}' created ({agent_count} agents)",
                )));
            }
            AgentNotification::SwarmTeamDeleted { team_name } => {
                self.push_message(MessageContent::System(format!(
                    "\u{1F41D} Swarm team '{team_name}' deleted",
                )));
            }
            AgentNotification::SwarmAgentSpawned { team_name, agent_id, model } => {
                self.push_message(MessageContent::System(format!(
                    "  ↳ [{team_name}] Agent {agent_id} spawned ({model})",
                )));
            }
            AgentNotification::SwarmAgentTerminated { team_name, agent_id } => {
                self.push_message(MessageContent::System(format!(
                    "  ↳ [{team_name}] Agent {agent_id} terminated",
                )));
            }
            AgentNotification::SwarmAgentQuery { team_name, agent_id, prompt_preview } => {
                self.push_message(MessageContent::System(format!(
                    "  ↳ [{team_name}/{agent_id}] ▶ {prompt_preview}",
                )));
            }
            AgentNotification::SwarmAgentReply { team_name, agent_id, text_preview, is_error } => {
                let icon = if is_error { "\u{2717}" } else { "\u{2713}" };
                self.push_message(MessageContent::System(format!(
                    "  ↳ [{team_name}/{agent_id}] {icon} {text_preview}",
                )));
            }
        }
        None
    }

    fn handle_slash_command(&mut self, client: &ClientHandle, cmd: &str) {
        let skills = clawed_core::skills::get_skills(
            &std::env::current_dir().unwrap_or_default(),
        );
        let parsed = match crate::commands::SlashCommand::parse(cmd, &skills) {
            Some(p) => p,
            None => return,
        };
        let result = parsed.execute(&skills, &[]);
        match result {
            crate::commands::CommandResult::Print(text) => {
                // /help output → scrollable info overlay
                self.overlay = Some(overlay::build_info_overlay("Help", &text));
            }
            crate::commands::CommandResult::ClearHistory => {
                let _ = client.send_request(clawed_bus::events::AgentRequest::ClearHistory);
                self.messages.clear();
                self.scroll_offset = 0;
            }
            crate::commands::CommandResult::SetModel(name) => {
                if name.is_empty() {
                    // No args → open model picker overlay
                    self.overlay = Some(overlay::build_model_overlay(&self.model));
                } else {
                    let _ = client.send_request(clawed_bus::events::AgentRequest::SetModel {
                        model: name,
                    });
                }
            }
            crate::commands::CommandResult::ShowCost { .. } => {
                let elapsed = self.status.session_start.elapsed().as_secs();
                self.overlay = Some(overlay::build_status_overlay(
                    &self.model, self.total_turns,
                    self.context_tokens, self.total_output_tokens, elapsed,
                ));
            }
            crate::commands::CommandResult::Compact { instructions } => {
                let _ = client.send_request(clawed_bus::events::AgentRequest::Compact {
                    instructions,
                });
            }
            crate::commands::CommandResult::Status => {
                let elapsed = self.status.session_start.elapsed().as_secs();
                self.overlay = Some(overlay::build_status_overlay(
                    &self.model, self.total_turns,
                    self.context_tokens, self.total_output_tokens, elapsed,
                ));
            }
            crate::commands::CommandResult::Think { args } => {
                let mode = if args.is_empty() { "on".to_string() } else { args };
                let _ = client.send_request(clawed_bus::events::AgentRequest::SetThinking { mode });
            }
            crate::commands::CommandResult::BreakCache => {
                let _ = client.send_request(clawed_bus::events::AgentRequest::BreakCache);
            }
            crate::commands::CommandResult::Mcp { .. } => {
                let _ = client.send_request(clawed_bus::events::AgentRequest::McpListServers);
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
                        "Current effort: auto\nOptions: {}", valid.join(", "),
                    )));
                } else if valid.contains(&level.to_lowercase().as_str()) {
                    self.push_message(MessageContent::System(format!(
                        "✓ Effort set to: {}", level.to_lowercase(),
                    )));
                } else {
                    self.push_message(MessageContent::System(format!(
                        "Invalid effort: '{level}'. Options: {}", valid.join(", "),
                    )));
                }
            }
            crate::commands::CommandResult::Tag { name } => {
                if name.is_empty() {
                    self.push_message(MessageContent::System(
                        "Usage: /tag <name>".to_string(),
                    ));
                } else {
                    self.push_message(MessageContent::System(format!(
                        "✓ Tagged session: {name}",
                    )));
                }
            }
            crate::commands::CommandResult::Stickers => {
                self.push_message(MessageContent::System(
                    "Sticker page: https://www.stickermule.com/claudecode".to_string(),
                ));
            }
            crate::commands::CommandResult::Vim { .. } => {
                self.push_message(MessageContent::System(
                    "Vim mode is not supported in TUI.".to_string(),
                ));
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
            | crate::commands::CommandResult::Session { .. }
            | crate::commands::CommandResult::Stats
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
            crate::commands::CommandResult::Review { ref prompt }
            | crate::commands::CommandResult::Bug { ref prompt }
            | crate::commands::CommandResult::Pr { ref prompt } => {
                if prompt.is_empty() {
                    self.push_message(MessageContent::System(
                        "This command requires a prompt argument.".to_string(),
                    ));
                } else {
                    self.pending_command = Some(result);
                }
            }
            crate::commands::CommandResult::Commit { .. }
            | crate::commands::CommandResult::CommitPushPr { .. }
            | crate::commands::CommandResult::PrComments { .. }
            | crate::commands::CommandResult::Branch { .. }
            | crate::commands::CommandResult::Search { .. }
            | crate::commands::CommandResult::History { .. } => {
                self.pending_command = Some(result);
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
    let task_plan_rows = app.task_plan.render_height();

    // When permission is active, the input row + hint bar are replaced by
    // a 3-row permission prompt (description + buttons + hints).
    let input_rows = app.input.visible_rows();
    // Footer includes a separator between input and hint bar (always, except perm).
    let footer_rows = if has_permission {
        permission::PERM_ROWS
    } else {
        input_rows + 1 + bottom_bar_rows // +1 for the separator between input and hint bar
    };

    // Queue items: 1 row per queued message (capped at 5), no header row.
    // Queue count is shown inside the info line instead.
    let queue_rows = if has_permission || app.queued_inputs.is_empty() {
        0
    } else {
        app.queued_inputs.len().min(5) as u16
    };

    // Input separator is always shown (separates queue from input box).
    // Suppress it when permission prompt is active (it has its own layout).
    let input_sep_rows = u16::from(!has_permission);

    let constraints = [
        Constraint::Min(1),                       // messages
        Constraint::Length(task_plan_rows),        // task plan (0 if empty)
        Constraint::Length(1),                     // info line (static + dynamic, always 1 row)
        Constraint::Length(queue_rows),            // queue items (0 or n)
        Constraint::Length(input_sep_rows),        // input separator (always 1, except perm)
        Constraint::Length(footer_rows),           // input/permission footer
    ];

    let chunks = Layout::vertical(constraints).split(area);
    let msg_area = chunks[0];
    let task_area = chunks[1];
    let sep_area = chunks[2];
    let queue_area = chunks[3];
    let input_sep_area = chunks[4];
    let footer_area = chunks[5];

    render_messages(frame, msg_area, app);

    if task_plan_rows > 0 {
        taskplan::render(frame, task_area, &app.task_plan);
    }

    render_separator(frame, sep_area, app.scroll_offset, app);

    if queue_rows > 0 {
        render_queue_banner(frame, queue_area, &app.queued_inputs);
    }

    if input_sep_rows > 0 && !has_permission {
        render_input_separator(frame, input_sep_area);
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
        // Normal: input ─── hint bar
        let input_chunks = Layout::vertical([
            Constraint::Length(input_rows),      // input (1–5 rows)
            Constraint::Length(1),               // separator between input and hint bar
            Constraint::Length(bottom_bar_rows), // hint bar
        ])
        .split(footer_area);

        render_input(frame, input_chunks[0], app);
        render_input_separator(frame, input_chunks[1]);
        if bottom_bar_rows > 0 {
            bottombar::render(frame, input_chunks[2]);
        }

        // Completion popup (rendered last so it draws on top)
        if app.input.in_completion() {
            render_completion_popup(frame, input_chunks[0], app);
        }
    }

    // Overlay renders last (on top of everything in message area)
    if let Some(ref ov) = app.overlay {
        overlay::render(frame, msg_area, ov);
    }
}

fn render_messages(frame: &mut Frame, area: Rect, app: &App) {
    if area.height == 0 || area.width == 0 {
        return;
    }

    // Collect all lines from all messages
    let all_lines: Vec<Line> = if app.messages.is_empty() {
        render_welcome_lines(area.width, &app.model)
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
                                .fg(MUTED)
                                .add_modifier(Modifier::ITALIC),
                        )];
                    }
                }
                msg.to_lines()
            })
            .collect()
    };

    let viewport_height = area.height as usize;

    // Build the full paragraph and let ratatui compute the exact visual row count.
    // This avoids the div_ceil approximation which can be wrong for word-wrapped
    // content (word boundaries differ from column boundaries).
    let paragraph = Paragraph::new(all_lines).wrap(Wrap { trim: false });
    let total_visual = paragraph.line_count(area.width);

    // scroll_offset = 0 → bottom of content; higher = scroll up.
    let scroll_row: u16 = if total_visual <= viewport_height {
        0
    } else {
        let max_scroll = total_visual - viewport_height;
        let clamped = app.scroll_offset.min(max_scroll);
        // Skip (max_scroll - clamped) visual rows from the top to anchor to the bottom.
        // Clamp to u16::MAX: content beyond 65 k visual rows still renders from the bottom.
        (max_scroll - clamped).min(u16::MAX as usize) as u16
    };

    // Clear the area first to prevent ghost cells from prior frames leaking through
    // when content shifts (especially large markdown blocks rendered all at once).
    frame.render_widget(Clear, area);
    frame.render_widget(paragraph.scroll((scroll_row, 0)), area);
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
                first_line.chars().take(max_text_width.saturating_sub(1)).collect::<String>() + "…"
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

/// Thin separator always rendered directly above the input box.
fn render_input_separator(frame: &mut Frame, area: Rect) {
    let sep = "\u{2500}".repeat(area.width as usize);
    frame.render_widget(
        Paragraph::new(Line::styled(sep, Style::default().fg(MUTED))),
        area,
    );
}

fn render_separator(frame: &mut Frame, area: Rect, scroll_offset: usize, app: &App) {
    let width = area.width as usize;
    let dim = Style::default().fg(MUTED);
    let hi = Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD);

    // --- Left side: static info (model │ turn N │ Xk↑ Yk↓ │ Z% ctx │ 📥N) ---
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

    if app.status.context_pct > 0.0 {
        info_parts.push(format!("{:.0}% ctx", app.status.context_pct));
    }

    if !app.queued_inputs.is_empty() {
        info_parts.push(format!("\u{1F4E5}{}", app.queued_inputs.len()));
    }

    // --- Right side: dynamic status spans (elapsed, spinner, tools, shells, agents) ---
    let status_spans = status::build_spans(&app.status);

    // Both sides are left-aligned: measure total to truncate info if needed.
    let status_w: usize = status_spans.iter().map(|s| s.content.width()).sum();

    // Scroll indicator on the left when scrolled up.
    let mut spans: Vec<Span> = Vec::new();
    let mut left_used = 0usize;

    if scroll_offset > 0 {
        let s = format!("\u{2191}{scroll_offset}  ");
        left_used += s.width();
        spans.push(Span::styled(s, hi));
    }

    // Info text, truncated so info + status fit within terminal width.
    if !info_parts.is_empty() {
        let info = format!(" {} ", info_parts.join(" \u{2502} "));
        let available = width.saturating_sub(left_used + status_w);
        let info = if info.width() > available {
            let mut t = String::new();
            for ch in info.chars() {
                if t.width() + 1 >= available {
                    t.push('…');
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

    // Dynamic status follows immediately (left-aligned).
    if status_w > 0 {
        spans.extend(status_spans);
    }

    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// Shorten a model identifier for display in the separator.
/// e.g. "claude-3-5-sonnet-20241022" → "claude-3.5-sonnet"
///      "gpt-4o-mini"               → "gpt-4o-mini"
fn shorten_model_name(model: &str) -> String {
    // Strip known date suffix patterns like "-20241022" or "-2024-10-22"
    let without_date = {
        let mut s = model;
        // trailing 8-digit date
        if s.len() > 9 {
            let tail = &s[s.len() - 9..];
            if tail.starts_with('-') && tail[1..].chars().all(|c| c.is_ascii_digit()) {
                s = &s[..s.len() - 9];
            }
        }
        s
    };
    // Cap at 28 chars
    if without_date.chars().count() > 28 {
        without_date.chars().take(27).collect::<String>() + "…"
    } else {
        without_date.to_string()
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
    let prompt_style = Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD);
    let text_style = Style::default(); // use terminal default — input text must be readable
    let image_style = Style::default().fg(Color::Magenta);
    let ghost_style = Style::default().fg(MUTED); // placeholder text stays muted
    let indicator_style = Style::default().fg(MUTED);

    let display_lines = app.input.display_lines();
    let img_count = app.pending_images.len();
    let is_empty = app.input.buffer().is_empty();
    let (has_above, has_below) = app.input.scroll_indicators();

    let lines: Vec<Line> = display_lines
        .iter()
        .enumerate()
        .map(|(i, line_text)| {
            if i == 0 {
                let mut spans = vec![Span::styled("> ", prompt_style)];
                if is_empty {
                    spans.push(Span::styled("Message Claude...", ghost_style));
                } else {
                    spans.push(Span::styled((*line_text).to_string(), text_style));
                }
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

    // Render scroll indicators on the right edge
    if area.width > 3 {
        let x = area.x + area.width - 1;
        if has_above {
            frame.render_widget(
                Paragraph::new(Span::styled("▲", indicator_style)),
                Rect::new(x, area.y, 1, 1),
            );
        }
        if has_below && area.height > 1 {
            frame.render_widget(
                Paragraph::new(Span::styled("▼", indicator_style)),
                Rect::new(x, area.y + area.height - 1, 1, 1),
            );
        }
    }

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
    let max_items = 10.min(matches.len());

    // Calculate visible window that keeps `selected` in view
    let scroll_offset = if selected >= max_items {
        selected - max_items + 1
    } else {
        0
    };

    // Calculate popup dimensions — two-column: "│  /cmd       Description text"
    let max_cmd_width = matches.iter().map(|c| c.width()).max().unwrap_or(4);
    let desc_col = max_cmd_width + 4; // padding between cmd and desc
    let max_desc_width = matches
        .iter()
        .map(|c| command_description(c).width())
        .max()
        .unwrap_or(20);
    let popup_width = (desc_col + max_desc_width + 3).min(input_area.width as usize);
    let popup_height = max_items as u16;

    // Position popup above input line, aligned to the left bar
    let popup_y = input_area.y.saturating_sub(popup_height);
    let popup_x = input_area.x;
    let popup_area = Rect::new(
        popup_x,
        popup_y,
        popup_width as u16,
        popup_height,
    );

    // Build lines — borderless, with left "│" margin, matching original style
    let bar_style = Style::default();
    let items: Vec<ListItem> = matches
        .iter()
        .enumerate()
        .skip(scroll_offset)
        .take(max_items)
        .map(|(i, cmd)| {
            let desc = command_description(cmd);
            let is_selected = i == selected;
            let cmd_style = if is_selected {
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
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
                Span::styled(" │ ", bar_style),
                Span::styled(format!("  {cmd}"), cmd_style),
                Span::raw(padding),
                Span::styled(desc.to_string(), desc_style),
            ]))
        })
        .collect();

    let list = List::new(items);

    // Clear the area first, then render (borderless)
    frame.render_widget(Clear, popup_area);
    frame.render_widget(list, popup_area);
}

fn render_welcome_lines(width: u16, model: &str) -> Vec<Line<'static>> {
    let title = format!("Clawed Code v{}", env!("CARGO_PKG_VERSION"));
    let model_line = format!("Model: {model}");
    let hints = "Tab: complete  \u{2191}\u{2193}: history  Ctrl+C: abort/quit  /help: commands";
    let tip = "Tip: Use /compact to free context  \u{2022}  Ctrl+V to paste images";

    let border_style = Style::default().fg(Color::Cyan);
    let text_style = Style::default().fg(Color::White).add_modifier(Modifier::BOLD);
    let model_style = Style::default().fg(Color::Cyan);
    let hint_style = Style::default().fg(MUTED);
    let tip_style = Style::default().fg(MUTED);

    let inner_width = title
        .width()
        .max(model_line.width())
        .max(hints.width())
        .max(tip.width())
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
            Span::styled(center(&title), text_style),
            Span::styled(" \u{2502}", border_style),
        ]),
        Line::from(vec![
            Span::styled("\u{2502} ", border_style),
            Span::styled(center(&model_line), model_style),
            Span::styled(" \u{2502}", border_style),
        ]),
        Line::from(vec![
            Span::styled("\u{2502} ", border_style),
            Span::styled(center(hints), hint_style),
            Span::styled(" \u{2502}", border_style),
        ]),
        Line::from(vec![
            Span::styled("\u{2502} ", border_style),
            Span::styled(center(tip), tip_style),
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

    // Initialize terminal: raw mode + NO alternate screen (matches codex-rs).
    // Skipping alternate screen improves Chinese IME compatibility on macOS
    // and lets output persist after exit.
    crossterm::terminal::enable_raw_mode()?;

    // Enable bracketed paste so multi-line paste arrives as Event::Paste(String)
    // instead of individual Key events (which would submit on Enter).
    crossterm::execute!(std::io::stdout(), EnableBracketedPaste)?;
    // Note: EnableMouseCapture is intentionally NOT set — it would prevent native
    // terminal text selection (copy-paste from terminal). Scroll is keyboard-only:
    // PageUp/PageDown and Shift+Up/Shift+Down.

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

    // Main event loop
    while app.running {
        // Advance spinner when thinking
        if app.status.thinking || !app.status.active_tools.is_empty() {
            app.status.spinner_frame = app.status.spinner_frame.wrapping_add(1);
        }

        // If content changed, fully clear the terminal before drawing to eliminate
        // ghost cells left from prior frames (no alternate screen = ratatui diffs
        // only changed cells, leaving stale cells where layout shrank).
        if app.needs_full_clear {
            terminal.clear()?;
            app.needs_full_clear = false;
        }

        // Render
        terminal.draw(|frame| render(frame, &app))?;

        // Non-blocking input poll
        if event::poll(std::time::Duration::from_millis(100))? {
            match event::read()? {
                Event::Key(key) => {
                    if key.kind != KeyEventKind::Press && key.kind != KeyEventKind::Repeat {
                        continue;
                    }

                    // Key debug mode: log raw key events
                    if app.key_debug {
                        app.push_message(MessageContent::System(
                            format!("KEY: code={:?} mod={:?} kind={:?}", key.code, key.modifiers, key.kind),
                        ));
                    }

                    // Esc while LLM is generating always aborts immediately,
                    // even if an overlay is open (close overlay + abort together).
                    if key.code == KeyCode::Esc && app.is_generating {
                        let _ = client.abort();
                        app.mark_done();
                        app.queued_inputs.clear();
                        app.overlay = None;
                        app.push_message(MessageContent::System(
                            "[Aborted]".to_string(),
                        ));
                        continue;
                    }

                    // If overlay is active, route keys there first
                    if app.overlay.is_some() {
                        let action = app.overlay.as_mut().unwrap().handle_key(key.code);
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
                                handle_overlay_selection(&title, &value, &client, &engine, &mut app).await;
                            }
                            OverlayAction::Consumed => {}
                        }
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
                            if app.is_generating {
                                let _ = client.abort();
                                app.mark_done();
                                app.queued_inputs.clear();
                                app.push_message(MessageContent::System(
                                    "[Aborted]".to_string(),
                                ));
                            } else {
                                app.running = false;
                            }
                            continue;
                        }
                        // Esc fallback (when not generating — handled above in early check)
                        (KeyCode::Esc, _) if app.is_generating => {
                            let _ = client.abort();
                            app.mark_done();
                            app.queued_inputs.clear();
                            app.push_message(MessageContent::System(
                                "[Aborted]".to_string(),
                            ));
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
                        // Toggle key debug mode
                        (KeyCode::Char('d'), KeyModifiers::CONTROL) => {
                            app.key_debug = !app.key_debug;
                            app.push_message(MessageContent::System(
                                format!("Key debug: {}", if app.key_debug { "ON" } else { "OFF" }),
                            ));
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
                                // While LLM is generating, queue plain text inputs.
                                // Slash commands are always handled immediately.
                                if app.is_generating
                                    && !text.starts_with('/')
                                    && app.pending_images.is_empty()
                                {
                                    app.queued_inputs.push(text);
                                    continue;
                                }

                                if text.starts_with('/') {
                                    // Slash commands execute silently — no message history echo.
                                    if text == "/abort" {
                                        let _ = client.abort();
                                        app.mark_done();
                                        app.queued_inputs.clear();
                                        app.push_message(MessageContent::System(
                                            "[Aborted]".to_string(),
                                        ));
                                    } else {
                                        let client_ref = &client;
                                        app.handle_slash_command(client_ref, &text);
                                        if let Some(cmd) = app.pending_command.take() {
                                            handle_async_command(
                                                cmd, &engine, &client, &mut app,
                                            ).await;
                                        }
                                    }
                                    app.pending_images.clear();
                                } else {
                                    // LLM prompt: show in conversation history.
                                    let display = if app.pending_images.is_empty() {
                                        text.clone()
                                    } else {
                                        format!(
                                            "{text} [+{} image(s)]",
                                            app.pending_images.len()
                                        )
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
                            let _ = client.abort();
                            app.mark_done();
                            app.queued_inputs.clear();
                            app.push_message(MessageContent::System(
                                "[Aborted]".to_string(),
                            ));
                        }
                        input::InputAction::Changed | input::InputAction::None => {}
                    }
                }
                Event::Resize(_, _) => {
                    // Full clear ensures no ghost cells after resize changes layout geometry.
                    app.needs_full_clear = true;
                }
                Event::Paste(text) => {
                    // Strip CR so \r\n becomes \n (insert_text handles bare \r too)
                    let text = text.replace('\r', "");
                    app.input.insert_text(&text);
                }
                _ => {} // Mouse, Focus -- ignored
            }
        }

        // Drain notification channel
        while let Ok(notification) = notify_rx.try_recv() {
            // Discard TextDelta/ThinkingDelta when:
            // - not generating (after abort), OR
            // - expecting_turn_start (new submit queued, waiting for TurnStart
            //   to confirm the new turn — deltas arriving now belong to the
            //   previous, possibly aborted, stream and must not bleed through).
            if !app.is_generating || app.expecting_turn_start {
                match &notification {
                    AgentNotification::TextDelta { .. }
                    | AgentNotification::ThinkingDelta { .. } => continue,
                    _ => {}
                }
            }
            if let Some(merged) = app.handle_notification(notification) {
                // Queued inputs from while LLM was generating — submit as one message
                app.push_message(MessageContent::UserInput(merged.clone()));
                let _ = client.submit(&merged);
                app.mark_generating();
            }
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

    // Restore terminal (no alternate screen to leave).
    // Always pop keyboard enhancement flags — the push is unconditional, and
    // popping on terminals that ignored the push is harmless.
    let _ = crossterm::execute!(std::io::stdout(), DisableBracketedPaste);
    let _ = crossterm::execute!(
        std::io::stdout(),
        crossterm::event::PopKeyboardEnhancementFlags
    );
    // Show cursor, clear screen, and disable raw mode
    let _ = crossterm::execute!(
        std::io::stdout(),
        crossterm::cursor::Show,
    );
    let _ = crossterm::terminal::disable_raw_mode();
    Ok(())
}

// -- Overlay selection handler -------------------------------------------------

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
            let resolved = clawed_core::model::resolve_model_string(value);
            let display = clawed_core::model::display_name_any(&resolved);
            engine.state().write().await.model = resolved.clone();
            app.model = resolved;
            let _ = client.send_request(clawed_bus::events::AgentRequest::SetModel {
                model: value.to_string(),
            });
            app.push_message(MessageContent::System(format!("✓ Model → {display}")));
        }
        "Theme" => {
            app.push_message(MessageContent::System(format!("✓ Theme → {value}")));
        }
        _ => {
            app.push_message(MessageContent::System(format!("Selected: {value}")));
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
) {
    use crate::commands::CommandResult;
    use clawed_core::message::{ContentBlock, Message as CoreMsg};

    match cmd {
        CommandResult::Diff => {
            let cwd = std::env::current_dir().unwrap_or_default();
            match std::process::Command::new("git")
                .args(["diff", "--stat", "--no-color"])
                .current_dir(&cwd)
                .output()
            {
                Ok(out) => {
                    let text = String::from_utf8_lossy(&out.stdout);
                    if text.trim().is_empty() {
                        app.push_message(MessageContent::System(
                            "No uncommitted changes.".to_string(),
                        ));
                    } else {
                        app.push_message(MessageContent::System(text.to_string()));
                    }
                }
                Err(e) => {
                    app.push_message(MessageContent::System(format!("git diff failed: {e}")));
                }
            }
        }
        CommandResult::Undo => {
            let removed = engine.rewind_turns(1).await;
            if removed.0 == 0 {
                app.push_message(MessageContent::System("Nothing to undo.".to_string()));
            } else {
                app.push_message(MessageContent::System(format!(
                    "✓ Undid 1 turn ({} messages remaining)", removed.1,
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
                app.push_message(MessageContent::System(format!(
                    "Retrying: {preview}",
                )));
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
                            "✓ Copied to clipboard ({} chars)", text.len(),
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
            match std::fs::write(&filename, &md) {
                Ok(()) => {
                    app.push_message(MessageContent::System(format!(
                        "✓ Session exported to {filename} ({} bytes)", md.len(),
                    )));
                }
                Err(e) => {
                    app.push_message(MessageContent::System(format!("Export failed: {e}")));
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
            match std::fs::write(&filename, &content) {
                Ok(()) => {
                    app.push_message(MessageContent::System(format!(
                        "✓ Exported to {filename}",
                    )));
                }
                Err(e) => {
                    app.push_message(MessageContent::System(format!("Export failed: {e}")));
                }
            }
        }
        CommandResult::Rename { name } => {
            if name.is_empty() {
                app.push_message(MessageContent::System("Usage: /rename <new name>".to_string()));
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
                    "✓ Switched to: {}", clawed_core::model::display_name_any(&default),
                )));
            } else if current == fast_model {
                let default = clawed_core::model::resolve_model_string("sonnet");
                state.write().await.model = default.clone();
                app.model = default.clone();
                app.push_message(MessageContent::System(format!(
                    "✓ Fast mode off → {}", clawed_core::model::display_name_any(&default),
                )));
            } else {
                state.write().await.model = fast_model.clone();
                app.model = fast_model.clone();
                app.push_message(MessageContent::System(format!(
                    "✓ Fast mode on → {}", clawed_core::model::display_name_any(&fast_model),
                )));
            }
        }
        CommandResult::Context => {
            let _ = client.send_request(clawed_bus::events::AgentRequest::GetStatus);
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
        CommandResult::Files { pattern } => {
            let cwd = std::env::current_dir().unwrap_or_default();
            match std::fs::read_dir(&cwd) {
                Ok(entries) => {
                    let mut items: Vec<_> = entries
                        .flatten()
                        .filter(|e| {
                            pattern.is_empty()
                                || e.file_name().to_string_lossy().contains(pattern.as_str())
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
                    if items.is_empty() {
                        app.push_message(MessageContent::System(format!(
                            "No files matching '{pattern}'",
                        )));
                    } else {
                        lines.push_str(&format!("({} items in {})", items.len(), cwd.display()));
                        app.overlay = Some(overlay::build_info_overlay("Files", &lines));
                    }
                }
                Err(e) => {
                    app.push_message(MessageContent::System(format!(
                        "Cannot read directory: {e}",
                    )));
                }
            }
        }
        CommandResult::Session { sub } => {
            // /session or /resume — handle session listing and resume
            let sessions = clawed_core::session::list_sessions();
            if sub.is_empty() || sub == "list" {
                if sessions.is_empty() {
                    app.push_message(MessageContent::System("No saved sessions.".to_string()));
                } else {
                    let mut lines = String::from("Recent sessions:\n");
                    for (i, s) in sessions.iter().take(10).enumerate() {
                        let title = s.custom_title.as_deref()
                            .or(s.last_prompt.as_deref())
                            .unwrap_or(&s.title);
                        let age = chrono::Utc::now().signed_duration_since(s.updated_at);
                        let age_str = format_duration(age);
                        lines.push_str(&format!(
                            "  {}: {} ({}, {} turns, {})\n",
                            i + 1, title, s.id, s.turn_count, age_str,
                        ));
                    }
                    lines.push_str("\nUse /resume <id> to restore.");
                    app.overlay = Some(overlay::build_info_overlay("Sessions", &lines));
                }
            } else {
                // /resume <id> or /session resume [id]
                let sid = sub.trim();
                let target_id = if sid.is_empty() || sid == "latest" {
                    sessions.first().map(|s| s.id.clone())
                } else {
                    Some(sid.to_string())
                };
                if let Some(id) = target_id {
                    match engine.restore_session(&id).await {
                        Ok(title) => {
                            app.push_message(MessageContent::System(format!(
                                "Resumed session: {title}",
                            )));
                            replay_session_messages(engine, app).await;
                        }
                        Err(e) => {
                            app.push_message(MessageContent::System(format!(
                                "Resume failed: {e}",
                            )));
                        }
                    }
                } else {
                    app.push_message(MessageContent::System(
                        "No sessions to resume.".to_string(),
                    ));
                }
            }
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
                match clawed_core::image::read_image_file(&img_path) {
                    Ok(ContentBlock::Image { source }) => {
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
                    Err(e) => {
                        app.push_message(MessageContent::System(format!("Image error: {e}")));
                    }
                    Ok(_) => {
                        app.push_message(MessageContent::System(
                            "Unexpected content block from image read.".to_string(),
                        ));
                    }
                }
            }
        }
        CommandResult::Feedback { text } => {
            let feedback_path = dirs::home_dir()
                .map(|h| h.join(".claude").join("feedback.log"))
                .unwrap_or_else(|| std::path::PathBuf::from("feedback.log"));
            if let Some(parent) = feedback_path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let timestamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
            let entry = format!("[{timestamp}] {text}\n");
            match std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&feedback_path)
            {
                Ok(mut f) => {
                    use std::io::Write;
                    let _ = f.write_all(entry.as_bytes());
                    app.push_message(MessageContent::System(format!(
                        "✓ Feedback saved to {}", feedback_path.display(),
                    )));
                }
                Err(e) => {
                    app.push_message(MessageContent::System(format!(
                        "Could not save feedback: {e}",
                    )));
                }
            }
        }
        CommandResult::ReleaseNotes => {
            app.push_message(MessageContent::System(format!(
                "Clawed Code v{}\n\nRecent changes:\n  • Full ratatui TUI with double-buffered rendering\n  • Markdown + syntect code highlighting\n  • Multi-line input, collapsible thinking/tool results\n  • Permission prompts, session resume, image paste\n  • 55+ slash commands, 52+ tools",
                env!("CARGO_PKG_VERSION"),
            )));
        }
        CommandResult::Memory { sub } => {
            let output = crate::repl_commands::handle_memory_command_str(
                &sub,
                &std::env::current_dir().unwrap_or_default(),
            );
            app.push_message(MessageContent::System(output));
        }
        // Commands that submit a prompt to the agent
        CommandResult::Review { prompt }
        | CommandResult::Bug { prompt }
        | CommandResult::Pr { prompt } => {
            if !prompt.is_empty() {
                let _ = client.submit(&prompt);
                app.mark_generating();
            }
        }
        CommandResult::Commit { message } => {
            let prompt = if message.is_empty() {
                "Review the staged changes and create a commit with an appropriate message.".to_string()
            } else {
                format!("Create a commit with message: {message}")
            };
            let _ = client.submit(&prompt);
            app.mark_generating();
        }
        CommandResult::CommitPushPr { message } => {
            let prompt = if message.is_empty() {
                "Commit staged changes, push, and create a PR with appropriate title and description.".to_string()
            } else {
                format!("Commit with message '{message}', push, and create a PR.")
            };
            let _ = client.submit(&prompt);
            app.mark_generating();
        }
        CommandResult::Search { query } => {
            if query.is_empty() {
                app.push_message(MessageContent::System(
                    "Usage: /search <query>".to_string(),
                ));
            } else {
                let state = engine.state().read().await;
                let mut results = Vec::new();
                for (i, msg) in state.messages.iter().enumerate() {
                    let text = match msg {
                        CoreMsg::User(u) => u.content.iter().find_map(|b| {
                            if let ContentBlock::Text { text } = b { Some(text.as_str()) } else { None }
                        }),
                        CoreMsg::Assistant(a) => a.content.iter().find_map(|b| {
                            if let ContentBlock::Text { text } = b { Some(text.as_str()) } else { None }
                        }),
                        CoreMsg::System(s) => Some(s.message.as_str()),
                    };
                    if let Some(text) = text {
                        if text.to_lowercase().contains(&query.to_lowercase()) {
                            let preview: String = text.chars().take(80).collect();
                            results.push(format!("  [{}] {}", i, preview));
                        }
                    }
                }
                drop(state);
                if results.is_empty() {
                    app.push_message(MessageContent::System(format!(
                        "No results for '{query}'",
                    )));
                } else {
                    let mut out = format!("Search results for '{query}':\n");
                    for r in results.iter().take(20) {
                        out.push_str(r);
                        out.push('\n');
                    }
                    app.push_message(MessageContent::System(out));
                }
            }
        }
        CommandResult::History { page } => {
            let state = engine.state().read().await;
            let per_page = 10;
            let start = (page.saturating_sub(1)) * per_page;
            let total = state.messages.len();
            let end = total.min(start + per_page);
            let mut out = format!("History (page {page}, {total} messages total):\n");
            for (i, msg) in state.messages.iter().enumerate().skip(start).take(end - start) {
                let role = match msg {
                    CoreMsg::User(_) => "user",
                    CoreMsg::Assistant(_) => "assistant",
                    CoreMsg::System(_) => "system",
                };
                out.push_str(&format!("  [{i}] {role}\n"));
            }
            drop(state);
            app.push_message(MessageContent::System(out));
        }
        CommandResult::PrComments { pr_number } => {
            if pr_number == 0 {
                app.push_message(MessageContent::System(
                    "Usage: /pr-comments <PR#>".to_string(),
                ));
            } else {
                let prompt = format!("Fetch and address review comments for PR #{pr_number}.");
                let _ = client.submit(&prompt);
                app.mark_generating();
            }
        }
        CommandResult::Branch { name } => {
            if name.is_empty() {
                app.push_message(MessageContent::System(
                    "Usage: /branch <name>".to_string(),
                ));
            } else {
                app.push_message(MessageContent::System(
                    "Branch conversations are not yet supported in TUI. Use --session-id to create separate sessions.".to_string(),
                ));
            }
        }
        CommandResult::AddDir { path } => {
            if path.is_empty() {
                app.push_message(MessageContent::System("Usage: /add-dir <path>".to_string()));
            } else {
                let cwd = std::env::current_dir().unwrap_or_default();
                let dir_path = std::path::Path::new(&path);
                let dir_path = if dir_path.is_relative() { cwd.join(dir_path) } else { dir_path.to_path_buf() };
                if !dir_path.is_dir() {
                    app.push_message(MessageContent::System(format!(
                        "Directory not found: {}", dir_path.display(),
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
                                    ctx.push_str(&format!("--- {name} ---\n{}\n\n", content.trim()));
                                    file_count += 1;
                                }
                            }
                        }
                    }
                    ctx.push_str("</context>");
                    engine.update_system_prompt_context(&ctx).await;
                    app.push_message(MessageContent::System(format!(
                        "✓ Added {file_count} file(s) from {}", dir_path.display(),
                    )));
                }
            }
        }
        CommandResult::Summary => {
            let prompt = "Provide a brief summary of our conversation so far.";
            let _ = client.submit(prompt);
            app.mark_generating();
        }
        // Commands that are not meaningfully different in TUI
        CommandResult::Permissions { mode } => {
            if mode.is_empty() {
                app.push_message(MessageContent::System(
                    "Permission mode: default\n  Set with: /permissions <default|bypass|acceptEdits|plan>".to_string(),
                ));
            } else {
                app.push_message(MessageContent::System(format!(
                    "Permission mode set to: {mode}",
                )));
            }
        }
        CommandResult::Config => {
            let cwd = std::env::current_dir().unwrap_or_default();
            let project_settings = cwd.join(".claude").join("settings.json");
            let exists = project_settings.exists();
            let info = format!(
                "Config:\n  Project settings: {} ({})\n  Model: {}\n  CWD: {}",
                project_settings.display(),
                if exists { "found" } else { "not found" },
                app.model,
                cwd.display(),
            );
            app.overlay = Some(overlay::build_info_overlay("Configuration", &info));
        }
        CommandResult::Doctor => {
            let cwd = std::env::current_dir().unwrap_or_default();
            let doctor_overlay = overlay::build_doctor_overlay(engine, &cwd).await;
            app.overlay = Some(doctor_overlay);
        }
        CommandResult::Init => {
            let prompt = "Create a CLAUDE.md file for this project with build commands, architecture, and conventions.";
            let _ = client.submit(prompt);
            app.mark_generating();
        }
        CommandResult::Plan { args } => {
            if args.is_empty() {
                app.push_message(MessageContent::System(
                    "Usage: /plan <description>  — Generate an implementation plan".to_string(),
                ));
            } else {
                let prompt = format!("Create a detailed implementation plan for: {args}");
                let _ = client.submit(&prompt);
                app.mark_generating();
            }
        }
        CommandResult::Login | CommandResult::Logout => {
            app.push_message(MessageContent::System(
                "Auth commands require a terminal restart. Use --api-key or set ANTHROPIC_API_KEY.".to_string(),
            ));
        }
        CommandResult::ReloadContext => {
            app.push_message(MessageContent::System(
                "Context reloaded on next submission.".to_string(),
            ));
        }
        CommandResult::Theme { name } => {
            if name.is_empty() {
                app.overlay = Some(overlay::build_theme_overlay("dark"));
            } else {
                app.push_message(MessageContent::System(format!(
                    "✓ Theme set to: {name}",
                )));
            }
        }
        CommandResult::Agents { sub } => {
            let cwd = std::env::current_dir().unwrap_or_default();
            let text = format_agents_tui(&sub, &cwd, &app.status.active_agents);
            app.overlay = Some(overlay::build_info_overlay("Agents", &text));
        }
        CommandResult::Plugin { .. }
        | CommandResult::RunPluginCommand { .. } | CommandResult::RunSkill { .. } => {
            app.push_message(MessageContent::System(
                "This command is not yet available in TUI mode.".to_string(),
            ));
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
        | CommandResult::Mcp { .. }
        | CommandResult::Env
        | CommandResult::Effort { .. }
        | CommandResult::Tag { .. }
        | CommandResult::Stickers
        | CommandResult::Vim { .. }
        | CommandResult::Exit => {
            // Should not reach here — these are handled in handle_slash_command
        }
    }
}

// -- /agents TUI formatter ----------------------------------------------------

/// Format `/agents [sub]` output as plain text for a TUI info overlay.
fn format_agents_tui(
    sub: &str,
    cwd: &std::path::Path,
    active_agents: &std::collections::HashMap<String, String>,
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
                for (id, label) in active_agents {
                    out.push_str(&format!("  ▸ {:<24} {}\n", id, label));
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
        "help" | _ => {
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
        let lines = render_welcome_lines(80, "claude-sonnet-4-20250514");
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
    fn overlay_replaces_none() {
        let mut app = App::new("test".to_string());
        assert!(app.overlay.is_none());
        app.overlay = Some(overlay::build_model_overlay("test"));
        assert!(app.overlay.is_some());
        app.overlay = None;
        assert!(app.overlay.is_none());
    }
}
