use super::*;

impl App {
    /// Returns Some(merged_text) if queued inputs should be submitted after this notification.
    pub(super) fn handle_notification(
        &mut self,
        notification: AgentNotification,
    ) -> Option<String> {
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
                self.tool_monitor_cache.dirty = true;
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
                let mut tool_input: Option<String> = None;
                if let Some(msg) = msg {
                    tool_input = match &msg.content {
                        MessageContent::ToolExecution { input, .. } => input.clone(),
                        _ => None,
                    };
                    msg.update_tool_result(is_error, duration_ms, &result);
                    self.invalidate_visible_lines();
                }
                self.tool_history.push(ToolHistoryEntry {
                    tool_name: tool_name.clone(),
                    input_summary: tool_input.unwrap_or_default(),
                    duration_ms,
                    is_error,
                    timestamp: Instant::now(),
                });
                if self.tool_history.len() > 50 {
                    self.tool_history.remove(0);
                }
                self.tool_monitor_cache.dirty = true;
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
                    self.task_list.side_panel_visible = true;
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
                    "{marker} Compacting context\u{2026}\n{bar} 0%",
                    marker = verbs::THINKING_MARKER,
                    bar = "\u{25B1}".repeat(40),
                )));
            }
            AgentNotification::CompactComplete { .. } => {
                self.push_message(MessageContent::System(format!(
                    "{marker} Conversation compacted (Ctrl+O for history)\n{bar} 100%",
                    marker = verbs::TURN_COMPLETION_MARKER,
                    bar = "\u{25B0}".repeat(40),
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
                            .push_str(&format!("  {status} {} ({} tools)\n", s.name, s.tool_count));
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

    pub(super) fn handle_slash_command(&mut self, client: &ClientHandle, cmd: &str) {
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
            Some(crate::commands::SlashCommand::Hooks) => {
                let loaded = clawed_core::config::Settings::load_merged(&cwd);
                let hooks = &loaded.settings.hooks;
                let mut out = String::from("Configured Hooks:\n");

                macro_rules! tui_count_event {
                    ($name:expr, $field:expr) => {{
                        let n = $field.len();
                        if n > 0 {
                            out.push_str(&format!("  {}: {} rule(s)\n", $name, n));
                        }
                    }};
                }

                tui_count_event!("PreToolUse", hooks.pre_tool_use);
                tui_count_event!("PostToolUse", hooks.post_tool_use);
                tui_count_event!("PostToolUseFailure", hooks.post_tool_use_failure);
                tui_count_event!("Stop", hooks.stop);
                tui_count_event!("UserPromptSubmit", hooks.user_prompt_submit);
                tui_count_event!("SessionStart", hooks.session_start);
                tui_count_event!("PreCompact", hooks.pre_compact);
                tui_count_event!("Notification", hooks.notification);
                tui_count_event!("PermissionRequest", hooks.permission_request);
                tui_count_event!("FileChanged", hooks.file_changed);
                tui_count_event!("ConfigChange", hooks.config_change);
                tui_count_event!("TaskCreated", hooks.task_created);
                tui_count_event!("TaskCompleted", hooks.task_completed);
                tui_count_event!("StopFailure", hooks.stop_failure);
                tui_count_event!("SessionEnd", hooks.session_end);
                tui_count_event!("Setup", hooks.setup);
                tui_count_event!("PostCompact", hooks.post_compact);
                tui_count_event!("SubagentStart", hooks.subagent_start);
                tui_count_event!("SubagentStop", hooks.subagent_stop);
                tui_count_event!("PostSampling", hooks.post_sampling);
                tui_count_event!("PermissionDenied", hooks.permission_denied);
                tui_count_event!("InstructionsLoaded", hooks.instructions_loaded);
                tui_count_event!("CwdChanged", hooks.cwd_changed);
                tui_count_event!("TeammateIdle", hooks.teammate_idle);
                tui_count_event!("Elicitation", hooks.elicitation);
                tui_count_event!("ElicitationResult", hooks.elicitation_result);
                tui_count_event!("WorktreeCreate", hooks.worktree_create);
                tui_count_event!("WorktreeRemove", hooks.worktree_remove);

                if out == "Configured Hooks:\n" {
                    out = "No hooks configured.".to_string();
                }
                self.overlay = Some(overlay::build_info_overlay("Hooks", &out));
                self.request_redraw();
                return;
            }
            Some(crate::commands::SlashCommand::Color { .. }) => {
                self.push_message(MessageContent::System(
                    "Use /color in REPL mode (prompt color). In TUI mode, use /theme.".to_string(),
                ));
                return;
            }
            Some(crate::commands::SlashCommand::Tasks { sub }) => {
                let task_dir = dirs::home_dir()
                    .unwrap_or_default()
                    .join(".claude")
                    .join("tasks");
                if sub.starts_with("show") {
                    let id = sub.trim_start_matches("show").trim();
                    let path = task_dir.join(format!("{id}.json"));
                    match std::fs::read_to_string(&path) {
                        Ok(content) => {
                            self.overlay =
                                Some(overlay::build_info_overlay(&format!("Task {id}"), &content));
                            self.request_redraw();
                        }
                        Err(_) => {
                            self.push_message(MessageContent::System(format!(
                                "Task '{id}' not found."
                            )));
                        }
                    }
                } else {
                    match std::fs::read_dir(&task_dir) {
                        Ok(entries) => {
                            let mut tasks: Vec<_> = entries
                                .flatten()
                                .filter(|e| e.path().extension().is_some_and(|ext| ext == "json"))
                                .collect();
                            tasks.sort_by_key(|e| e.file_name());
                            if tasks.is_empty() {
                                self.push_message(MessageContent::System(
                                    "No background tasks.".to_string(),
                                ));
                            } else {
                                let mut list =
                                    format!("Background Tasks ({} total):\n", tasks.len());
                                for entry in &tasks {
                                    let name = entry.file_name();
                                    let name_str = name.to_string_lossy();
                                    let id = name_str.trim_end_matches(".json");
                                    list.push_str(&format!("  {id}\n"));
                                }
                                self.overlay = Some(overlay::build_info_overlay("Tasks", &list));
                                self.request_redraw();
                            }
                        }
                        Err(_) => {
                            self.push_message(MessageContent::System(
                                "No background tasks.".to_string(),
                            ));
                        }
                    }
                }
                return;
            }
            Some(crate::commands::SlashCommand::SecurityReview) => {
                // Defer to pending_command for engine access
                let result = crate::commands::CommandResult::SecurityReview;
                self.pending_command = Some(result);
                return;
            }
            Some(crate::commands::SlashCommand::Advisor { sub }) => {
                if sub.is_empty() {
                    let model = self.model.clone();
                    let msg = format!("Advisor: not set\nUse /advisor <model> to enable (e.g. /advisor opus)\nMain model: {model}");
                    self.push_message(MessageContent::System(msg));
                } else if sub.eq_ignore_ascii_case("off") || sub.eq_ignore_ascii_case("unset") {
                    self.push_message(MessageContent::System("Advisor disabled.".to_string()));
                } else {
                    let resolved = clawed_core::model::resolve_model_string(&sub);
                    if resolved.is_empty() {
                        self.push_message(MessageContent::System(format!(
                            "Unknown model: '{sub}'"
                        )));
                    } else {
                        let display = clawed_core::model::display_name_any(&resolved);
                        self.push_message(MessageContent::System(format!(
                            "Advisor set to: {display} ({resolved})"
                        )));
                    }
                }
                return;
            }
            Some(crate::commands::SlashCommand::Sandbox { sub }) => {
                let msg = if sub.starts_with("exclude") {
                    let pattern = sub.trim_start_matches("exclude").trim();
                    if pattern.is_empty() {
                        "Usage: /sandbox exclude <pattern>".to_string()
                    } else {
                        format!("Excluded pattern: {pattern}")
                    }
                } else {
                    "Sandbox: not available (requires Docker or sandboxing environment)".to_string()
                };
                self.push_message(MessageContent::System(msg));
                return;
            }
            Some(crate::commands::SlashCommand::Ide { sub }) => {
                if sub.eq_ignore_ascii_case("open") {
                    let result = std::process::Command::new("code").arg(".").output();
                    match result {
                        Ok(_) => self
                            .push_message(MessageContent::System("VS Code launched.".to_string())),
                        Err(_) => self.push_message(MessageContent::System(
                            "VS Code not found in PATH.\nOpen your IDE manually.".to_string(),
                        )),
                    }
                } else {
                    self.push_message(MessageContent::System(
                        "IDE Integration: not connected\nUse /ide open to launch VS Code."
                            .to_string(),
                    ));
                }
                return;
            }
            Some(crate::commands::SlashCommand::Keybindings) => {
                let keybindings_dir = dirs::home_dir()
                    .map(|h| h.join(".claude"))
                    .unwrap_or_default();
                let path = keybindings_dir.join("keybindings.json");
                if !path.exists() {
                    let template = r#"{
  // Claude Code Keyboard Shortcuts
  "version": "1.0",
  "bindings": []
}
"#;
                    if std::fs::create_dir_all(&keybindings_dir).is_err() {
                        self.push_message(MessageContent::System(
                            "Could not create ~/.claude/ directory.".to_string(),
                        ));
                    } else if std::fs::write(&path, template).is_ok() {
                        self.push_message(MessageContent::System(format!(
                            "Created keybindings config at:\n{}",
                            path.display()
                        )));
                    } else {
                        self.push_message(MessageContent::System(
                            "Failed to create keybindings file.".to_string(),
                        ));
                    }
                } else {
                    self.push_message(MessageContent::System(format!(
                        "Keybindings config at:\n{}",
                        path.display()
                    )));
                }
                return;
            }
            Some(crate::commands::SlashCommand::Session) => {
                let is_remote = std::env::var("CLAUDE_CODE_REMOTE").is_ok();
                let m = if is_remote {
                    "Remote Session: connected"
                } else {
                    "Remote Session: local mode\nStart with --remote for remote mode."
                };
                self.push_message(MessageContent::System(m.to_string()));
                return;
            }
            Some(crate::commands::SlashCommand::Statusline { .. }) => {
                // Defer to pending_command for engine access
                let result = crate::commands::CommandResult::Statusline {
                    prompt: String::new(),
                };
                self.pending_command = Some(result);
                return;
            }
            Some(crate::commands::SlashCommand::TerminalSetup) => {
                let term = std::env::var("TERM_PROGRAM")
                    .or_else(|_| std::env::var("TERM"))
                    .unwrap_or_else(|_| "unknown".to_string());
                let native_terms = ["ghostty", "kitty", "iterm2", "wezterm", "warp"];
                let msg = if native_terms.iter().any(|t| term.to_lowercase().contains(t)) {
                    format!("Terminal: {term}\nYour terminal natively supports multi-line input.\nNo additional setup needed. Use Shift+Enter or Alt+Enter for newlines.")
                } else {
                    format!("Terminal: {term}\nTo enable multi-line input:\n  • Use \\ (backslash) at end of line to continue on next line\n  • Or configure a keybinding for sending Ctrl+J / Shift+Enter")
                };
                self.push_message(MessageContent::System(msg));
                return;
            }
            Some(crate::commands::SlashCommand::Desktop) => {
                let url = "claude://handoff";
                match opener::open(url) {
                    Ok(_) => self.push_message(MessageContent::System(
                        "Claude Desktop: opened.".to_string(),
                    )),
                    Err(e) => self.push_message(MessageContent::System(format!(
                        "Claude Desktop: failed to open ({e})"
                    ))),
                }
                return;
            }
            Some(crate::commands::SlashCommand::Mobile) => {
                self.push_message(MessageContent::System(
                    "Claude Mobile App:\n  iOS: https://apps.apple.com/app/claude-by-anthropic/id6473753684\n  Android: https://play.google.com/store/apps/details?id=com.anthropic.claude".to_string(),
                ));
                return;
            }
            Some(crate::commands::SlashCommand::Install { args }) => {
                let force = if args.contains("--force") {
                    " (force)"
                } else {
                    ""
                };
                self.push_message(MessageContent::System(format!(
                    "Claude Code v{}{force}\nBinary: {}",
                    env!("CARGO_PKG_VERSION"),
                    std::env::current_exe()
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|_| "unknown".to_string())
                )));
                return;
            }
            Some(crate::commands::SlashCommand::Upgrade) => {
                let url = "https://claude.ai/upgrade/max";
                match opener::open(url) {
                    Ok(_) => self.push_message(MessageContent::System(
                        "Upgrade page opened in browser.".to_string(),
                    )),
                    Err(e) => self.push_message(MessageContent::System(format!(
                        "Could not open browser: {e}\nVisit: {url}"
                    ))),
                }
                return;
            }
            Some(crate::commands::SlashCommand::PrivacySettings) => {
                let url = "https://claude.ai/settings/data-privacy-controls";
                match opener::open(url) {
                    Ok(_) => self.push_message(MessageContent::System(
                        "Privacy settings opened in browser.".to_string(),
                    )),
                    Err(e) => self.push_message(MessageContent::System(format!(
                        "Could not open browser: {e}\nVisit: {url}"
                    ))),
                }
                return;
            }
            Some(crate::commands::SlashCommand::Onboarding) => {
                self.push_message(MessageContent::System(
                    "Welcome to Clawed Code!\nType /help for available commands.\nQuick start: /init, /model, /doctor".to_string(),
                ));
                return;
            }
            Some(crate::commands::SlashCommand::Passes) => {
                self.push_message(MessageContent::System(
                    "Referral Passes:\n  https://claude.ai/passes\n  Share this link to give friends a free week of Claude Code.".to_string(),
                ));
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
                    self.push_message(MessageContent::System(format!("✓ Tagged session: {name}")));
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
            | crate::commands::CommandResult::Goal { .. }
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
            | crate::commands::CommandResult::Resume { .. }
            | crate::commands::CommandResult::Hooks
            | crate::commands::CommandResult::Tasks { .. }
            | crate::commands::CommandResult::Color { .. }
            | crate::commands::CommandResult::SecurityReview
            | crate::commands::CommandResult::Advisor { .. }
            | crate::commands::CommandResult::Sandbox { .. }
            | crate::commands::CommandResult::Ide { .. }
            | crate::commands::CommandResult::Keybindings
            | crate::commands::CommandResult::Session
            | crate::commands::CommandResult::Statusline { .. }
            | crate::commands::CommandResult::TerminalSetup
            | crate::commands::CommandResult::Desktop
            | crate::commands::CommandResult::Mobile
            | crate::commands::CommandResult::Install { .. }
            | crate::commands::CommandResult::Upgrade
            | crate::commands::CommandResult::PrivacySettings
            | crate::commands::CommandResult::Onboarding
            | crate::commands::CommandResult::Passes => {
                self.pending_command = Some(result);
            }
        }
    }
}
