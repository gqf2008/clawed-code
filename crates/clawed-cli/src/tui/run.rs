use super::*;

/// If a skill temporarily changed the model or set a context, and the turn
/// has ended (`is_generating` is false), restore the original state.
pub(super) async fn restore_skill_state_if_done(app: &mut App, engine: &Arc<QueryEngine>) {
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
pub(super) async fn abort_session(client: &ClientHandle, app: &mut App, engine: &Arc<QueryEngine>) {
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
pub(super) async fn do_resume_session(engine: &Arc<QueryEngine>, app: &mut App, id: &str) {
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
    app.task_list.side_panel_visible = true; // Auto-show the right panel
    app.max_context_tokens = engine.context_window();
    app.permission_mode =
        crate::config::format_permission_mode(engine.state().read().await.permission_mode)
            .to_string();

    // Load settings and configure external status line if present.
    let loaded = clawed_core::config::Settings::load_merged(&cwd);
    if let Some(ref cfg) = loaded.settings.status_line {
        app.status_line = statusline::StatusLineState::new(Some(cfg.command.clone()));
    }

    // Refresh task list on startup and auto-show side panel if tasks exist.
    app.task_list.refresh(&cwd);
    if app.task_list.task_count() > 0 {
        app.task_list.side_panel_visible = true;
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

    // Initialize terminal: raw mode + alternate screen.
    // Alternate screen hides the terminal's native scrollbar and isolates
    // TUI output from the scrollback buffer.
    crossterm::terminal::enable_raw_mode()?;
    crossterm::execute!(std::io::stdout(), crossterm::terminal::EnterAlternateScreen)?;
    crossterm::execute!(std::io::stdout(), EnableMouseCapture)?;
    // Enable button-event tracking (1002) so wheel events are reported
    // reliably on terminals that need it (e.g. Windows Terminal).
    let _ = std::io::Write::write_all(&mut std::io::stdout(), b"\x1b[?1002h");
    let _terminal_guard = TuiTerminalGuard;

    // Enable bracketed paste so multi-line paste arrives as Event::Paste(String)
    // instead of individual Key events (which would submit on Enter).
    crossterm::execute!(std::io::stdout(), EnableBracketedPaste)?;

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
    // Set a default render width before the first frame so streaming
    // markdown rendering (which may run during notification processing
    // before the first render) has a reasonable width to work with.
    if let Ok(size) = crossterm::terminal::size() {
        markdown::set_render_width(size.0.max(40));
    }

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

            let goal_submitted = if turn_complete {
                handle_goal_turn_complete(&client, &engine, &mut app).await
            } else {
                false
            };
            if goal_submitted {
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

        // On resize or layout change, force a real terminal clear before redraw
        // so the new layout doesn't get scribbled over the old one.
        if app.needs_full_clear {
            app.needs_full_clear = false;
            let _ = terminal.clear();
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
                            app.task_list.toggle_side_panel();
                            if !app.task_list.is_expanded() {
                                app.right_panel_focus = None;
                            }
                            app.request_redraw();
                            continue;
                        }
                        (KeyCode::Left, KeyModifiers::ALT) => {
                            if app.task_list.is_expanded() {
                                let base = app.effective_panel_width();
                                app.panel_width = (base + 5).min(60);
                                app.request_redraw();
                            }
                            continue;
                        }
                        (KeyCode::Right, KeyModifiers::ALT) => {
                            if app.task_list.is_expanded() {
                                let base = app.effective_panel_width();
                                app.panel_width = base.saturating_sub(5).max(30);
                                app.request_redraw();
                            }
                            continue;
                        }
                        (KeyCode::Tab, KeyModifiers::NONE) if app.task_list.is_expanded() => {
                            app.right_panel_focus = match app.right_panel_focus {
                                None => Some(RightPanelFocus::Tasks),
                                Some(f) => f.next(),
                            };
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
                            let step = if key.code == KeyCode::PageUp {
                                10usize
                            } else {
                                1usize
                            };
                            if let Some(sub) = app.right_panel_focus {
                                scroll_right_sub(&mut app, sub, step as i32);
                            } else {
                                app.scroll_offset = app.scroll_offset.saturating_add(step);
                                app.auto_scroll = false;
                            }
                            app.request_redraw();
                            continue;
                        }
                        (KeyCode::PageDown, _) | (KeyCode::Down, KeyModifiers::SHIFT) => {
                            let step = if key.code == KeyCode::PageDown {
                                10usize
                            } else {
                                1usize
                            };
                            if let Some(sub) = app.right_panel_focus {
                                scroll_right_sub(&mut app, sub, -(step as i32));
                            } else if app.scroll_offset > 0 {
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
                Event::Mouse(mouse) => {
                    // Determine which panel the mouse is over.
                    let in_right_panel =
                        app.last_right_panel_x > 0 && mouse.column >= app.last_right_panel_x;
                    let right_sub = if in_right_panel {
                        if mouse.row >= app.right_tasks_rect.y
                            && mouse.row < app.right_tasks_rect.y + app.right_tasks_rect.height
                        {
                            Some(RightPanelFocus::Tasks)
                        } else if mouse.row >= app.right_tools_rect.y
                            && mouse.row < app.right_tools_rect.y + app.right_tools_rect.height
                        {
                            Some(RightPanelFocus::ToolHistory)
                        } else if mouse.row >= app.right_stats_rect.y
                            && mouse.row < app.right_stats_rect.y + app.right_stats_rect.height
                        {
                            Some(RightPanelFocus::Stats)
                        } else {
                            None
                        }
                    } else {
                        None
                    };
                    let on_scrollbar = app.scrollbar_rect.width > 0
                        && mouse.column >= app.scrollbar_rect.x
                        && mouse.column < app.scrollbar_rect.x + app.scrollbar_rect.width
                        && mouse.row >= app.scrollbar_rect.y
                        && mouse.row < app.scrollbar_rect.y + app.scrollbar_rect.height;
                    match mouse.kind {
                        MouseEventKind::ScrollUp => {
                            if let Some(sub) = right_sub {
                                scroll_right_sub(&mut app, sub, 3);
                            } else {
                                app.scroll_offset = app.scroll_offset.saturating_add(3);
                                app.auto_scroll = false;
                            }
                            app.request_redraw();
                        }
                        MouseEventKind::ScrollDown => {
                            if let Some(sub) = right_sub {
                                scroll_right_sub(&mut app, sub, -3i32);
                            } else if app.scroll_offset > 0 {
                                app.scroll_offset = app.scroll_offset.saturating_sub(3);
                                if app.scroll_offset == 0 {
                                    app.auto_scroll = true;
                                    app.new_messages_count = 0;
                                }
                            }
                            app.request_redraw();
                        }
                        MouseEventKind::Down(_) if on_scrollbar => {
                            app.scrollbar_dragging = true;
                            scroll_to_row(&mut app, mouse.row);
                        }
                        MouseEventKind::Drag(_) if app.scrollbar_dragging => {
                            scroll_to_row(&mut app, mouse.row);
                        }
                        MouseEventKind::Up(_) => {
                            app.scrollbar_dragging = false;
                        }
                        _ => {}
                    }
                }
                Event::Resize(w, h) => {
                    app.term_width = w;
                    app.term_height = h;
                    app.needs_full_clear = true;
                    app.tool_monitor_cache.dirty = true;
                    // Reset to bottom on resize — the height cache will be rebuilt
                    // with the new geometry, making old scroll_offset meaningless.
                    app.auto_scroll = true;
                    app.scroll_offset = 0;
                    app.new_messages_count = 0;
                    app.invalidate_visible_lines();
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

pub(super) fn submit_prepared_prompt(
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

pub(super) fn submit_queued_inputs(client: &ClientHandle, app: &mut App) {
    if let Some(merged) = app.take_queued_inputs() {
        app.push_message(MessageContent::UserInput(merged.clone()));
        let _ = client.submit(&merged);
        app.mark_generating();
    }
}

pub(super) fn submit_goal_iteration(client: &ClientHandle, app: &mut App) -> bool {
    let Some((prompt, iteration, objective)) = app.goal_state.as_mut().and_then(|goal| {
        if goal.status != GoalStatus::Active {
            None
        } else {
            let prompt = prepare_goal_iteration(goal);
            Some((prompt, goal.iteration, goal.objective.clone()))
        }
    }) else {
        return false;
    };

    app.push_message(MessageContent::System(format!(
        "Goal iteration {}: {}",
        iteration, objective
    )));
    let _ = client.submit(&prompt);
    app.mark_generating();
    true
}

pub(super) async fn handle_goal_turn_complete(
    client: &ClientHandle,
    engine: &Arc<QueryEngine>,
    app: &mut App,
) -> bool {
    let Some(goal) = app.goal_state.as_mut() else {
        return false;
    };
    if goal.status != GoalStatus::Active {
        return false;
    }

    let decision = match judge_goal_progress(engine, goal).await {
        Ok(decision) => decision,
        Err(e) => {
            goal.status = GoalStatus::Blocked;
            goal.last_reason = Some(format!("Goal judge failed: {}", e));
            app.push_message(MessageContent::System(format!("Goal blocked: {e}")));
            return false;
        }
    };

    goal.last_reason = Some(decision.reason.clone());
    goal.next_prompt = decision.next_prompt.clone();

    match decision.action {
        GoalDecisionAction::Continue => {
            app.push_message(MessageContent::System(format!(
                "Goal continuing: {}",
                clawed_core::text_util::truncate_chars(&decision.reason, 200, "…")
            )));
            submit_goal_iteration(client, app)
        }
        GoalDecisionAction::Completed => {
            goal.status = GoalStatus::Completed;
            app.push_message(MessageContent::System(format!(
                "Goal completed: {}",
                clawed_core::text_util::truncate_chars(&decision.reason, 200, "…")
            )));
            false
        }
        GoalDecisionAction::Blocked => {
            goal.status = GoalStatus::Blocked;
            app.push_message(MessageContent::System(format!(
                "Goal blocked: {}",
                clawed_core::text_util::truncate_chars(&decision.reason, 200, "…")
            )));
            false
        }
    }
}

pub(super) async fn git_status_porcelain(cwd: &std::path::Path) -> String {
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

pub(super) async fn handle_pending_workflow(client: &ClientHandle, app: &mut App) -> bool {
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
pub(super) async fn handle_overlay_selection(
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

pub(super) async fn handle_footer_picker_selection(
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
