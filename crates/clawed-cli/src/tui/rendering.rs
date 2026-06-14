use super::*;

// -- Rendering ----------------------------------------------------------------

pub(super) fn render(frame: &mut Frame, app: &mut App) {
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

    // ── Row counts (shared between layout and rendering) ──
    let bottom_bar_rows = if has_permission {
        0
    } else {
        u16::from(!app.bottom_bar_hidden)
    };
    let task_plan_rows = app.task_plan.render_height();
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
    // Input area (framed by two separator lines), without the bottom bar.
    let input_area_rows = if let Some(layout) = perm_layout {
        layout.total_rows()
    } else {
        2 + completion_rows + input_rows
    };

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
        (app.suggestions.len().min(5) + 1) as u16
    };

    // ── Top-level vertical split: content + input + status bar ──
    let top_chunks = Layout::vertical([
        Constraint::Min(1),                  // content area
        Constraint::Length(input_area_rows), // input area (full width)
        Constraint::Length(bottom_bar_rows), // status bar (full width)
    ])
    .split(area);
    let content_area = top_chunks[0];
    let input_area = top_chunks[1];
    let status_bar_area = top_chunks[2];

    // ── Content area: horizontal split ──
    let effective_width = app.effective_panel_width();
    let task_panel_width = app.task_list.panel_width(effective_width);
    let h_chunks = Layout::horizontal([
        Constraint::Min(1),                   // left column
        Constraint::Length(task_panel_width), // right panel (0 if hidden)
    ])
    .split(content_area);
    let left_col = h_chunks[0];
    let right_panel = if task_panel_width > 0 {
        h_chunks[1]
    } else {
        Rect::default()
    };
    app.last_right_panel_x = if task_panel_width > 0 {
        right_panel.x
    } else {
        0
    };

    // ── Left column: vertical (messages + overlays) ──
    let left_constraints = [
        Constraint::Min(1),                  // messages
        Constraint::Length(task_plan_rows),  // task plan (0 if empty)
        Constraint::Length(bash_mode_rows),  // BashModeProgress panel
        Constraint::Length(1 + tip_rows),    // separator + optional tip
        Constraint::Length(queue_rows),      // queue items
        Constraint::Length(suggestion_rows), // context suggestion overlay
        Constraint::Length(search_rows),     // search box
    ];
    let chunks = Layout::vertical(left_constraints).split(left_col);
    let msg_area = chunks[0];
    let task_area = chunks[1];
    let bash_area = chunks[2];
    let sep_area = chunks[3];
    let queue_area = chunks[4];
    let suggestion_area = chunks[5];
    let search_area = chunks[6];

    // ── Render right panel (tasks + tool history + stats) ──
    if task_panel_width > 0 {
        let stats_rows = 8u16.min(right_panel.height);
        let remaining = right_panel.height.saturating_sub(stats_rows);
        let tool_rows = remaining / 2;
        let task_rows = remaining - tool_rows;
        let r_chunks = Layout::vertical([
            Constraint::Length(task_rows),  // tasks
            Constraint::Length(tool_rows),  // tool history
            Constraint::Length(stats_rows), // stats
        ])
        .split(right_panel);

        tasklist::render(frame, r_chunks[0], &mut app.task_list);

        app.right_tasks_rect = r_chunks[0];
        app.right_tools_rect = r_chunks[1];
        app.right_stats_rect = r_chunks[2];

        let active_names: Vec<String> = app.status.active_tools.keys().cloned().collect();
        let is_focused = app.right_panel_focus == Some(RightPanelFocus::ToolHistory);
        tool_monitor::render(
            frame,
            r_chunks[1],
            &app.tool_history,
            &active_names,
            &mut app.tool_history_scroll,
            is_focused,
            &mut app.tool_monitor_cache,
        );

        render_stats_panel(frame, r_chunks[2], app);
    }

    // Teammate view header: fixed 1 row above messages when viewing a teammate.
    let teammate_header_rows = u16::from(app.viewed_teammate.is_some());
    let msg_chunks =
        Layout::vertical([Constraint::Length(teammate_header_rows), Constraint::Min(1)])
            .split(msg_area);
    if teammate_header_rows > 0 {
        render_teammate_view_header(frame, msg_chunks[0], app);
    }
    render_messages(frame, msg_chunks[1], app);

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
        let layout = permission::layout_for(input_area.width, perm);
        let perm_chunks = Layout::vertical([
            Constraint::Length(layout.desc_rows),
            Constraint::Length(layout.detail_rows),
            Constraint::Length(layout.button_rows),
            Constraint::Length(layout.hint_rows),
        ])
        .split(input_area);
        permission::render(
            frame,
            perm_chunks[0],
            perm_chunks[1],
            perm_chunks[2],
            perm_chunks[3],
            perm,
        );
    } else {
        let input_chunks = Layout::vertical([
            Constraint::Length(1),               // top separator
            Constraint::Length(input_rows),      // input (1–5 rows)
            Constraint::Length(completion_rows), // completion popup / footer picker
            Constraint::Length(1),               // bottom separator
        ])
        .split(input_area);

        render_input_separator(frame, input_chunks[0]);
        render_input(frame, input_chunks[1], app);
        render_input_separator(frame, input_chunks[3]);

        if let Some(picker) = app.footer_picker.as_ref() {
            render_footer_picker(frame, input_chunks[2], input_chunks[1], picker);
        } else if completion_rows > 0 {
            render_completion_popup(frame, input_chunks[2], input_chunks[1], app);
        }
    }

    // ── Status bar (full width, very bottom) ──
    if bottom_bar_rows > 0 {
        bottombar::render(
            frame,
            status_bar_area,
            app.is_generating,
            &app.permission_mode,
            app.task_list.is_expanded(),
        );
    }

    // Overlay renders last (on top of everything in message area)
    if let Some(ref ov) = app.overlay {
        overlay::render(frame, msg_area, ov);
    }
}

/// Render the stats panel in the right column (model, turn, tokens, cost, ctx%).
pub(super) fn render_stats_panel(frame: &mut Frame, area: Rect, app: &mut App) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let dim = Style::default().fg(MUTED);
    let bold = Style::default()
        .fg(Color::White)
        .add_modifier(Modifier::BOLD);
    let warn = Style::default().fg(Color::Yellow);
    let danger = Style::default().fg(Color::Red).add_modifier(Modifier::BOLD);

    let mut lines: Vec<Line> = Vec::new();

    let short_model = shorten_model_name(&app.model);
    if !short_model.is_empty() {
        lines.push(Line::from(vec![Span::styled(short_model, bold)]));
    }

    lines.push(Line::from(vec![Span::styled(
        format!("turn {}", app.total_turns),
        dim,
    )]));

    lines.push(Line::from(vec![Span::styled(
        format!(
            "{}\u{2191} {}\u{2193}",
            fmt_tokens(app.context_tokens),
            fmt_tokens(app.total_output_tokens),
        ),
        dim,
    )]));

    if app.total_cost_usd > 0.0 {
        lines.push(Line::from(vec![Span::styled(
            clawed_core::model::format_cost(app.total_cost_usd),
            dim,
        )]));
    }

    let ctx_pct = if app.status.context_pct > 0.0 {
        app.status.context_pct
    } else if app.max_context_tokens > 0 && app.context_tokens > 0 {
        (app.context_tokens as f64 / app.max_context_tokens as f64 * 100.0).min(100.0)
    } else {
        0.0
    };
    let ctx_style = if ctx_pct >= 80.0 {
        danger
    } else if ctx_pct >= 60.0 {
        warn
    } else {
        dim
    };
    let max_str = if app.max_context_tokens > 0 {
        format!(" · max {}", fmt_tokens(app.max_context_tokens))
    } else {
        String::new()
    };
    lines.push(Line::from(vec![
        Span::styled(format!("{:.0}% ctx", ctx_pct), ctx_style),
        Span::styled(max_str, dim),
    ]));
    lines.push(build_context_bar(
        ctx_pct,
        area.width.saturating_sub(2) as usize,
    ));

    let inner_height = area.height.saturating_sub(2) as usize;
    let max_scroll = lines.len().saturating_sub(inner_height);
    app.stats_scroll_offset = app.stats_scroll_offset.min(max_scroll);
    let visible: Vec<Line> = if app.stats_scroll_offset == 0 {
        // Unscrolled: show the bottom content so the progress bar is always visible.
        let skip = lines.len().saturating_sub(inner_height);
        lines.into_iter().skip(skip).take(inner_height).collect()
    } else {
        lines
            .into_iter()
            .skip(app.stats_scroll_offset)
            .take(inner_height)
            .collect()
    };

    let title = if app.stats_scroll_offset > 0 {
        format!(" Stats \u{2191}{} ", app.stats_scroll_offset)
    } else {
        " Stats ".to_string()
    };

    let block = Block::bordered()
        .border_set(ratatui::symbols::border::PLAIN)
        .title(title)
        .title_style(dim);

    let para = Paragraph::new(visible)
        .block(block)
        .wrap(Wrap { trim: false });
    frame.render_widget(para, area);
}

/// Map a mouse row (absolute terminal coordinate) to a scroll position
/// and update scroll_offset accordingly.
pub(super) fn scroll_to_row(app: &mut App, row: u16) {
    if app.scrollbar_total == 0 || app.scrollbar_viewport == 0 {
        return;
    }
    let track_h = app.scrollbar_viewport;
    let rel_row = row.saturating_sub(app.scrollbar_rect.y) as usize;
    let max_scroll = app.scrollbar_total.saturating_sub(track_h);
    if max_scroll == 0 {
        app.scroll_offset = 0;
        app.auto_scroll = true;
        return;
    }
    // Map row to first_visible_line, then to scroll_offset.
    let first_vis = (rel_row * app.scrollbar_total / track_h).min(app.scrollbar_total - track_h);
    app.scroll_offset = max_scroll - first_vis;
    app.auto_scroll = app.scroll_offset == 0;
    if app.auto_scroll {
        app.new_messages_count = 0;
    }
    app.request_redraw();
}

/// Scroll a specific sub-panel of the right panel. Positive delta scrolls
/// up (back in history), negative scrolls down.
pub(super) fn scroll_right_sub(app: &mut App, sub: RightPanelFocus, delta: i32) {
    let step = delta.unsigned_abs() as usize;
    let field: &mut usize = match sub {
        RightPanelFocus::Tasks => &mut app.task_list.scroll_offset,
        RightPanelFocus::ToolHistory => &mut app.tool_history_scroll,
        RightPanelFocus::Stats => &mut app.stats_scroll_offset,
    };
    if delta > 0 {
        *field = field.saturating_add(step);
    } else {
        *field = field.saturating_sub(step);
    }
}

pub(super) fn poll_interval(app: &App) -> Duration {
    if app.spinner_active() || app.status.active_shells > 0 || !app.status.active_agents.is_empty()
    {
        ACTIVE_POLL_INTERVAL
    } else {
        IDLE_POLL_INTERVAL
    }
}

/// Render a proportional scrollbar on the right edge of the message area.
pub(super) fn render_scrollbar(
    frame: &mut Frame,
    area: Rect,
    total_visual: usize,
    viewport_height: usize,
    first_visible_line: usize,
) {
    if total_visual <= viewport_height {
        return;
    }
    if area.width == 0 || area.height == 0 {
        return;
    }

    let track_h = viewport_height;
    let thumb_h = (track_h * track_h / total_visual).max(1);
    let max_top = track_h - thumb_h;
    let thumb_top = (track_h * first_visible_line / total_visual).min(max_top);

    let track = Style::default().fg(MUTED);
    let thumb = Style::default().fg(Color::Rgb(140, 140, 140));
    let lines: Vec<Line> = (0..track_h)
        .map(|row| {
            let (ch, style) = if row >= thumb_top && row < thumb_top + thumb_h {
                ("\u{258C}", thumb)
            } else {
                ("\u{00B7}", track)
            };
            Line::from(Span::styled(ch, style))
        })
        .collect();

    let para = Paragraph::new(lines);
    frame.render_widget(para, area);
}

pub(super) fn render_messages(frame: &mut Frame, area: Rect, app: &mut App) {
    if area.height == 0 || area.width == 0 {
        return;
    }

    if app.messages.is_empty() {
        let welcome = render_welcome_lines(area.width, &app.model, &app.permission_mode);
        frame.render_widget(Paragraph::new(welcome).wrap(Wrap { trim: false }), area);
        return;
    }

    // Reserve the rightmost column for the scrollbar. Build the height
    // cache at the narrower width so message wrap does not overlap the bar.
    let sb_width: u16 = 1;
    let msg_width = area.width.saturating_sub(sb_width);
    markdown::set_render_width(msg_width);

    app.rebuild_visible_lines();

    let viewport_height = area.height as usize;

    if app.message_line_counts_width != msg_width
        || app.message_line_counts.iter().any(|c| c.is_none())
    {
        app.build_height_cache(msg_width);
        app.message_line_counts_width = msg_width;
    }
    let total_visual: usize = app
        .message_line_counts
        .iter()
        .map(|c| c.unwrap_or(1) as usize)
        .sum();
    let has_scrollbar = total_visual > viewport_height && msg_width > 0;
    let adjusted_area = Rect::new(area.x, area.y, msg_width, area.height);

    // --- Sticky header ---
    let sticky_rows = compute_sticky_rows(app, viewport_height);
    let msg_area = if sticky_rows > 0 {
        Rect::new(
            adjusted_area.x,
            adjusted_area.y + sticky_rows,
            adjusted_area.width,
            adjusted_area.height - sticky_rows,
        )
    } else {
        adjusted_area
    };
    let msg_viewport_height = msg_area.height as usize;

    // The first visible line counting from the top of the (wrapped) content.
    // Used by the scroll-up path and system message block adjustment.
    let first_visible_line = if total_visual > msg_viewport_height {
        let max_scroll = total_visual - msg_viewport_height;
        let clamped = app.scroll_offset.min(max_scroll);
        max_scroll - clamped
    } else {
        0
    };

    // Compute the first visible message by walking from the correct end.
    // When auto-scrolling to bottom (scroll_offset=0), walk backward from the
    // last message to find the one that falls at the top of the viewport.
    // When scrolled up (scroll_offset>0), walk forward from the beginning.
    let (mut msg_start, mut line_offset) = if app.scroll_offset == 0 {
        // Bottom-anchored: walk messages backward from the end
        let mut remaining = msg_viewport_height;
        let mut start = app.messages.len();
        let mut use_offset = 0u16;
        for i in (0..app.messages.len()).rev() {
            let h = app
                .message_line_counts
                .get(i)
                .copied()
                .flatten()
                .unwrap_or(1) as usize;
            if remaining > h {
                remaining -= h;
                start = i;
            } else {
                start = i;
                use_offset = (h - remaining) as u16;
                break;
            }
        }
        (start.min(app.messages.len()), use_offset)
    } else {
        // Top-offset: walk forward to find which message contains first_visible_line
        let mut acc = 0usize;
        let mut start = 0usize;
        let mut offset = 0u16;
        for (i, h) in app.message_line_counts.iter().enumerate() {
            let h = h.unwrap_or(1) as usize;
            if acc + h > first_visible_line {
                start = i;
                offset = (first_visible_line - acc) as u16;
                break;
            }
            acc += h;
        }
        (start, offset)
    };

    // Adjust msg_start to the start of a system message block so that
    // append_message_lines doesn't begin mid-group (which would lose the
    // folding header and potentially skip messages).
    if msg_start > 0
        && msg_start < app.messages.len()
        && matches!(app.messages[msg_start].content, MessageContent::System(_))
    {
        let mut block_start = msg_start;
        while block_start > 0
            && matches!(
                app.messages[block_start - 1].content,
                MessageContent::System(_)
            )
        {
            block_start -= 1;
        }
        // Recompute line_offset: subtract heights of messages before block_start
        let mut adj_acc = 0usize;
        for (_, h) in app.message_line_counts.iter().enumerate().take(block_start) {
            adj_acc += h.unwrap_or(1) as usize;
        }
        let first_vis = first_visible_line.saturating_sub(adj_acc);
        line_offset = first_vis as u16;
        msg_start = block_start;
    }

    // Render only the visible range with generous overscan for smooth scroll.
    let overscan = 1000usize;
    let mut lines = Vec::new();
    let mut idx = msg_start;
    let mut rendered = 0usize;
    let target = msg_viewport_height + overscan;

    // Render partial first message if line_offset > 0
    if line_offset > 0 && idx < app.messages.len() {
        let mut part_lines: Vec<Line<'static>> = Vec::new();
        app.append_message_lines(&mut part_lines, &mut idx);
        let skip = line_offset as usize;
        if skip < part_lines.len() {
            lines.extend(part_lines.iter().skip(skip).cloned());
            rendered += part_lines.len() - skip;
        }
    }

    // Render remaining messages
    while idx < app.messages.len() && rendered < target {
        app.append_message_lines(&mut lines, &mut idx);
        // Estimate visual count for height cache (before wrapping)
        // The exact count is corrected after Paragraph::line_count below
        let est = lines.len().saturating_sub(rendered).max(1);
        rendered += est;
    }

    // Apply search highlighting via line offset mapping
    if let Some(ref search) = app.search_state {
        if !search.matches.is_empty() && !lines.is_empty() && !app.cached_visible_lines.is_empty() {
            // Find the offset of the first local line within cached_visible_lines
            // by matching the first few lines' text content. This is O(1) when the
            // first lines are distinct, and still correct (just slower) when not.
            let local_first: Vec<String> =
                lines.iter().take(5).map(crate::tui::line_text).collect();
            let offset = app
                .cached_visible_lines
                .windows(local_first.len())
                .position(|w| {
                    w.iter()
                        .zip(&local_first)
                        .all(|(l, t)| &crate::tui::line_text(l) == t)
                })
                .unwrap_or(0);

            for (line_idx, _) in &search.matches {
                if *line_idx >= offset && *line_idx < offset + lines.len() {
                    if let Some(line) = lines.get_mut(*line_idx - offset) {
                        for span in line.spans.iter_mut() {
                            span.style = span.style.add_modifier(Modifier::REVERSED);
                        }
                    }
                }
            }
        }
    }

    // Wrap and render
    let paragraph = if lines.is_empty() {
        Paragraph::new(Line::from("")).wrap(Wrap { trim: false })
    } else {
        // Prune overscanned lines to viewport
        let excess = lines
            .len()
            .saturating_sub(msg_viewport_height + overscan / 2);
        if excess > 0 {
            lines.drain(0..excess.min(lines.len()));
        }
        Paragraph::new(lines).wrap(Wrap { trim: false })
    };

    if should_clear_message_area(app.last_rendered_message_visual_count, total_visual) {
        frame.render_widget(Clear, msg_area);
    }
    // Bottom-align messages: pad the top with empty space when content is shorter
    // than the viewport so the newest message sits near the separator (chat-style).
    let render_area = if total_visual < msg_viewport_height && app.scroll_offset == 0 {
        let pad = (msg_viewport_height - total_visual) as u16;
        Rect::new(
            msg_area.x,
            msg_area.y + pad,
            msg_area.width,
            msg_area.height.saturating_sub(pad),
        )
    } else {
        msg_area
    };
    frame.render_widget(paragraph, render_area);
    app.last_rendered_message_visual_count = Some(total_visual);

    // Render sticky header overlay at the top of the message area.
    if sticky_rows > 0 {
        if let Some(idx) = app.sticky_anchor {
            let sticky_area = Rect::new(
                adjusted_area.x,
                adjusted_area.y,
                adjusted_area.width,
                sticky_rows,
            );
            render_sticky_header(frame, sticky_area, &app.messages[idx]);
        }
    }

    // Render "N new messages" pill when scrolled up and new content arrived.
    if app.new_messages_count > 0 && app.scroll_offset > 0 {
        render_new_messages_pill(frame, adjusted_area, app.new_messages_count);
    }

    // Render ephemeral agent progress lines at the bottom of the message area.
    // Hidden when scrolled up so the overlay does not obscure message history.
    if app.scroll_offset == 0 {
        render_agent_progress(frame, adjusted_area, app);
    }

    // Render scrollbar on the right edge when content overflows.
    if has_scrollbar {
        let sb_area = Rect::new(
            area.x + area.width.saturating_sub(sb_width),
            msg_area.y,
            sb_width,
            msg_viewport_height as u16,
        );
        render_scrollbar(
            frame,
            sb_area,
            total_visual,
            msg_viewport_height,
            first_visible_line,
        );
        app.scrollbar_rect = sb_area;
        app.scrollbar_total = total_visual;
        app.scrollbar_viewport = msg_viewport_height;
    } else {
        app.scrollbar_rect = Rect::default();
        app.scrollbar_total = 0;
        app.scrollbar_viewport = 0;
    }
}

/// Render ephemeral agent progress as an overlay at the bottom of the message area.
/// Progress disappears automatically when agents complete or terminate.
pub(super) fn render_agent_progress(frame: &mut Frame, area: Rect, app: &App) {
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
pub(super) fn compute_sticky_rows(app: &mut App, viewport_height: usize) -> u16 {
    if app.scroll_offset == 0 || app.messages.is_empty() {
        app.sticky_anchor = None;
        return 0;
    }

    let total_visual: usize = app
        .message_line_counts
        .iter()
        .map(|c| c.unwrap_or(1) as usize)
        .sum::<usize>()
        .max(app.cached_visible_lines.len());
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
pub(super) fn render_sticky_header(frame: &mut Frame, area: Rect, msg: &Message) {
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
pub(super) fn render_new_messages_pill(frame: &mut Frame, area: Rect, count: usize) {
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

pub(super) fn render_queue_banner(frame: &mut Frame, area: Rect, queued: &[String]) {
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
pub(super) fn render_suggestions_overlay(frame: &mut Frame, area: Rect, app: &App) {
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

pub(super) fn render_input_separator(frame: &mut Frame, area: Rect) {
    let w = area.width as usize;
    if w == 0 {
        return;
    }
    let style = Style::default().fg(MUTED);
    let sep = "\u{2500}".repeat(w);
    frame.render_widget(Paragraph::new(Line::styled(sep, style)), area);
}

pub(super) fn render_separator(frame: &mut Frame, area: Rect, scroll_offset: usize, app: &App) {
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
pub(super) fn render_search_box(frame: &mut Frame, area: Rect, app: &App) {
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
pub(super) fn context_bar(pct: f64) -> &'static str {
    const SEGMENTS: f64 = 5.0;
    let segment_pct = 100.0 / SEGMENTS;
    let filled = (pct / segment_pct).round().clamp(0.0, SEGMENTS) as usize;
    &CONTEXT_BARS[filled]
}

pub(super) fn build_context_bar(pct: f64, width: usize) -> Line<'static> {
    const COLORS: [Color; 5] = [
        Color::Green,
        Color::Cyan,
        Color::Yellow,
        Color::Rgb(255, 165, 0),
        Color::Red,
    ];
    const EMPTY: Color = Color::Rgb(100, 100, 100);
    if width == 0 {
        return Line::default();
    }
    let filled = ((pct / 100.0 * width as f64).round() as usize).clamp(0, width);
    let spans: Vec<Span> = (0..width)
        .map(|i| {
            if i < filled {
                let color_idx = (i * 5 / width).min(4);
                Span::styled("\u{25B0}", Style::default().fg(COLORS[color_idx]))
            } else {
                Span::styled("\u{25B1}", Style::default().fg(EMPTY))
            }
        })
        .collect();
    Line::from(spans)
}

/// Shorten a model identifier for display in the separator.
/// e.g. "claude-3-5-sonnet-20241022" → "claude-3.5-sonnet"
///      "gpt-4o-mini"               → "gpt-4o-mini"
pub(super) fn shorten_model_name(model: &str) -> String {
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
pub(super) fn fmt_tokens(n: u64) -> String {
    if n >= 1_000 {
        format!("{:.0}k", n as f64 / 1000.0)
    } else {
        n.to_string()
    }
}

pub(super) fn render_input(frame: &mut Frame, area: Rect, app: &App) {
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

pub(super) fn render_completion_popup(
    frame: &mut Frame,
    popup_slot: Rect,
    input_area: Rect,
    app: &App,
) {
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

pub(super) fn render_footer_picker(
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

pub(super) fn render_welcome_lines(
    width: u16,
    model: &str,
    permission_mode: &str,
) -> Vec<Line<'static>> {
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
