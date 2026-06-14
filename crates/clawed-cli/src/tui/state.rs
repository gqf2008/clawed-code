use super::*;

impl App {
    pub(super) fn new(model: String) -> Self {
        let ide_kind = detect_ide();
        let mut status = TuiStatusState::new();
        status.ide_kind = ide_kind.clone();
        Self {
            messages: Vec::new(),
            scroll_offset: 0,
            new_messages_count: 0,
            auto_scroll: true,
            message_line_counts: Vec::new(),
            message_line_counts_width: 0,
            input: InputWidget::new(),
            footer_picker: None,
            status,
            task_plan: taskplan::TaskPlan::new(),
            task_list: tasklist::TaskListState::new(),
            tool_history: Vec::new(),
            tool_history_scroll: 0,
            panel_width: 0, // 0 = auto (1/6 of terminal width)
            right_panel_focus: None,
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
            goal_state: None,
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
            tool_monitor_cache: tool_monitor::ToolMonitorCache::new(),
            max_context_tokens: 0,
            last_right_panel_x: 0,
            right_tasks_rect: Rect::default(),
            right_tools_rect: Rect::default(),
            right_stats_rect: Rect::default(),
            stats_scroll_offset: 0,
            scrollbar_rect: Rect::default(),
            scrollbar_total: 0,
            scrollbar_viewport: 0,
            scrollbar_dragging: false,
        }
    }

    pub(super) fn request_redraw(&mut self) {
        self.needs_redraw = true;
    }

    pub(super) fn effective_panel_width(&self) -> u16 {
        if self.panel_width > 0 {
            self.panel_width
        } else {
            (self.term_width / 6).clamp(30, 60)
        }
    }

    pub(super) fn set_footer_picker(&mut self, picker: FooterPicker) {
        self.footer_picker = Some(picker);
        self.request_redraw();
    }

    pub(super) fn clear_footer_picker(&mut self) {
        if self.footer_picker.take().is_some() {
            self.request_redraw();
        }
    }

    #[allow(dead_code)]
    pub(super) fn view_teammate(&mut self, #[allow(dead_code)] agent_id: String, name: String) {
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

    pub(super) fn exit_teammate_view(&mut self) {
        if self.viewed_teammate.take().is_some() {
            self.invalidate_visible_lines();
            self.request_redraw();
        }
    }

    // ── Teammate selection (pointer + tab/enter keyboard navigation) ──────────

    pub(super) fn enter_teammate_selection(&mut self) {
        if !self.status.active_agents.is_empty() {
            self.teammate_selection = Some(0);
            self.request_redraw();
        }
    }

    pub(super) fn exit_teammate_selection(&mut self) {
        if self.teammate_selection.take().is_some() {
            self.request_redraw();
        }
    }

    pub(super) fn cycle_teammate_selection(&mut self, delta: isize) {
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

    pub(super) fn confirm_teammate_selection(&mut self) {
        let Some(sel) = self.teammate_selection else {
            return;
        };
        self.exit_teammate_selection();
        let sorted = status::sorted_agent_entries(&self.status.active_agents);
        if let Some((agent_id, info)) = sorted.get(sel) {
            self.view_teammate((*agent_id).clone(), info.name.clone());
        }
    }

    pub(super) fn spinner_active(&self) -> bool {
        self.status.is_generating || !self.status.active_tools.is_empty()
    }

    pub(super) fn advance_spinner_if_due(&mut self, now: Instant) {
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

    pub(super) fn visible_message_lines_at(&self, index: usize) -> Vec<Line<'static>> {
        let msg = &self.messages[index];

        if self.is_generating && index + 1 == self.messages.len() {
            if let MessageContent::AssistantText(text) = &msg.content {
                if markdown::likely_markdown(text) {
                    let dim = muted();
                    let prefix = Span::styled("\u{25CF} ", dim);
                    let blank_prefix = Span::raw("   ");
                    return markdown::render_markdown(text)
                        .into_iter()
                        .enumerate()
                        .map(|(i, mut line)| {
                            if i == 0 {
                                line.spans.insert(0, prefix.clone());
                            } else {
                                line.spans.insert(0, blank_prefix.clone());
                            }
                            line
                        })
                        .collect();
                }
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

    pub(super) fn invalidate_visible_lines(&mut self) {
        self.cached_visible_lines_dirty = true;
        self.cached_visible_line_count = None;
        self.message_line_counts_width = 0; // force height cache rebuild
        self.request_redraw();
    }

    pub(super) fn replace_cached_tail(&mut self, old_len: usize, new_lines: Vec<Line<'static>>) {
        let new_start = self.cached_visible_lines.len().saturating_sub(old_len);
        self.cached_visible_lines.truncate(new_start);
        self.cached_visible_lines.extend(new_lines);
        // Extend height cache for new messages (added after the truncation point).
        // The exact number of new messages is unknown here; mark for rebuild.
        self.cached_visible_line_count = None;
        self.message_line_counts_width = 0; // force height cache rebuild
        self.request_redraw();
    }

    pub(super) fn rebuild_visible_lines(&mut self) {
        if !self.cached_visible_lines_dirty {
            return;
        }

        // Ensure height cache length matches messages
        if self.message_line_counts.len() != self.messages.len() {
            self.message_line_counts.resize(self.messages.len(), None);
        }

        let mut lines = Vec::new();
        let mut index = 0;
        while index < self.messages.len() {
            self.append_message_lines(&mut lines, &mut index);
        }
        self.cached_visible_lines = lines;
        self.cached_visible_lines_dirty = false;
        self.cached_visible_line_count = None;
        self.refresh_search_matches();
        self.apply_search_highlight();
    }

    /// Append lines for one logical message group, advancing `index`.
    pub(super) fn append_message_lines(&self, lines: &mut Vec<Line<'static>>, index: &mut usize) {
        if *index >= self.messages.len() {
            return;
        }
        if matches!(self.messages[*index].content, MessageContent::System(_)) {
            let start = *index;
            while *index < self.messages.len()
                && matches!(self.messages[*index].content, MessageContent::System(_))
            {
                *index += 1;
            }
            let count = *index - start;
            let has_important =
                (start..*index).any(|i| Self::system_msg_is_important(&self.messages[i].content));
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
                    lines.extend(self.visible_message_lines_at(*index - 1));
                }
                return;
            }
            for j in start..*index {
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
            return;
        }

        if *index > 0
            && Self::needs_separator(
                &self.messages[*index - 1].content,
                &self.messages[*index].content,
            )
        {
            lines.push(Line::from(""));
        }
        lines.extend(self.visible_message_lines_at(*index));
        *index += 1;
    }

    // ── Search helpers ──────────────────────────────────────────────────

    /// Recompute match positions from the current query against cached lines.
    pub(super) fn refresh_search_matches(&mut self) {
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
    pub(super) fn apply_search_highlight(&mut self) {
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
    pub(super) fn scroll_to_match(&mut self, line_idx: usize) {
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

    /// Build the height cache by wrapping all cached lines and measuring.
    pub(super) fn build_height_cache(&mut self, width: u16) {
        if self.messages.is_empty() {
            return;
        }
        markdown::set_render_width(width);

        self.message_line_counts.resize(self.messages.len(), None);

        let mut index = 0;
        while index < self.messages.len() {
            let start = index;
            let mut group_lines = Vec::new();
            self.append_message_lines(&mut group_lines, &mut index);
            let height = if group_lines.is_empty() {
                1u16
            } else {
                Paragraph::new(group_lines)
                    .wrap(Wrap { trim: false })
                    .line_count(width)
                    .max(1) as u16
            };

            self.message_line_counts[start] = Some(height);
            for slot in &mut self.message_line_counts[start + 1..index] {
                *slot = Some(0);
            }
        }
    }

    pub(super) fn clear_search(&mut self) {
        if self.search_state.take().is_some() {
            self.invalidate_visible_lines();
        }
    }

    /// Returns true when two consecutive messages should be visually separated
    /// by a blank line in the TUI message list.
    pub(super) fn needs_separator(prev: &MessageContent, curr: &MessageContent) -> bool {
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
    pub(super) fn system_msg_is_important(content: &MessageContent) -> bool {
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

    pub(super) fn clear_messages(&mut self) {
        self.messages.clear();
        self.scroll_offset = 0;
        self.new_messages_count = 0;
        self.cached_visible_lines.clear();
        self.cached_visible_lines_dirty = false;
        self.cached_visible_line_count = None;
        self.message_line_counts.clear();
        self.message_line_counts_width = 0;
        self.last_rendered_message_visual_count = None;
        self.footer_picker = None;
        self.agent_progress.clear();
        self.request_redraw();
    }

    pub(super) fn push_message(&mut self, content: MessageContent) {
        let msg = Message::new(content);
        let prev_content = self.messages.last().map(|m| &m.content);
        let needs_sep = prev_content
            .map(|prev| Self::needs_separator(prev, &msg.content))
            .unwrap_or(false);
        let affects_system_grouping = matches!(prev_content, Some(MessageContent::System(_)))
            || matches!(msg.content, MessageContent::System(_));
        self.messages.push(msg);
        self.message_line_counts.push(None);
        if !self.cached_visible_lines_dirty {
            if affects_system_grouping {
                self.invalidate_visible_lines();
            } else {
                if needs_sep {
                    self.cached_visible_lines.push(Line::from(""));
                }
                let last_index = self.messages.len().saturating_sub(1);
                self.cached_visible_lines
                    .extend(self.visible_message_lines_at(last_index));
                self.cached_visible_line_count = None;
            }
        }
        if self.auto_scroll {
            self.scroll_offset = 0;
            self.new_messages_count = 0;
        } else {
            self.new_messages_count += 1;
        }
        self.request_redraw();
    }

    pub(super) fn layout_signature(&self) -> LayoutSignature {
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

        let panel_width = self.task_list.panel_width(self.panel_width);

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
            panel_width,
        }
    }

    /// Mark that the LLM is now generating a response.
    /// Unlike status.thinking (which goes false during TextDelta), this stays
    /// true for the entire turn so queue gating and Esc abort work correctly.
    pub(super) fn mark_generating(&mut self) {
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
    pub(super) fn mark_done(&mut self) {
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
        // Reset scroll so the completed content is visible at the bottom.
        self.auto_scroll = true;
        self.scroll_offset = 0;
        self.new_messages_count = 0;
    }

    /// Hard-reset tool state. Called on error / abort / timeout when the normal
    /// ToolUseComplete lifecycle may not arrive.
    pub(super) fn clear_tool_state(&mut self) {
        self.status.active_tools.clear();
        self.status.active_shells = 0;
        self.task_plan.set_shells(0);
        self.tool_monitor_cache.dirty = true;
    }

    pub(super) fn take_queued_inputs(&mut self) -> Option<String> {
        if self.queued_inputs.is_empty() {
            None
        } else {
            let merged = self.queued_inputs.join("\n\n");
            self.queued_inputs.clear();
            Some(merged)
        }
    }

    /// Append text to the last AssistantText message, or create one.
    pub(super) fn append_assistant_text(&mut self, text: &str) {
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
    pub(super) fn append_thinking_text(&mut self, text: &str) {
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
}
