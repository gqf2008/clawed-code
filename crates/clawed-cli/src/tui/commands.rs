use super::*;

/// Handle `CommandResult` variants that need `async` engine access.
pub(super) async fn handle_async_command(
    cmd: crate::commands::CommandResult,
    engine: &Arc<QueryEngine>,
    client: &ClientHandle,
    app: &mut App,
    terminal: Option<&mut TuiTerminal>,
) {
    use crate::commands::CommandResult;
    use clawed_core::message::{ContentBlock, Message as CoreMsg};

    match cmd {
        CommandResult::Goal { args } => {
            let goal_arg = args.trim();
            match goal_arg {
                "" | "status" => {
                    if let Some(goal) = &app.goal_state {
                        app.push_message(MessageContent::System(goal_status_message(goal)));
                    } else {
                        app.push_message(MessageContent::System(
                            "No active goal. Use /goal <objective> to start.".to_string(),
                        ));
                    }
                }
                "pause" => {
                    if let Some(goal) = app.goal_state.as_mut() {
                        goal.status = GoalStatus::Paused;
                        goal.last_reason = Some("Paused by user.".into());
                        goal.next_prompt = Some(
                            "Resume from the current point and continue toward the goal.".into(),
                        );
                        if app.is_generating {
                            abort_session(client, app, engine).await;
                        }
                        app.push_message(MessageContent::System("Goal paused.".to_string()));
                    } else {
                        app.push_message(MessageContent::System("No goal to pause.".to_string()));
                    }
                }
                "resume" => {
                    if let Some(goal) = app.goal_state.as_mut() {
                        if goal.status == GoalStatus::Paused {
                            goal.status = GoalStatus::Active;
                            if !submit_goal_iteration(client, app) {
                                app.push_message(MessageContent::System(
                                    "Could not resume goal.".to_string(),
                                ));
                            }
                        } else {
                            let label = goal.status.label().to_string();
                            app.push_message(MessageContent::System(format!(
                                "Goal is {}. Only paused goals can be resumed.",
                                label
                            )));
                        }
                    } else {
                        app.push_message(MessageContent::System(
                            "No paused goal to resume.".to_string(),
                        ));
                    }
                }
                "clear" => {
                    if app.is_generating
                        && app
                            .goal_state
                            .as_ref()
                            .is_some_and(|goal| goal.status == GoalStatus::Active)
                    {
                        abort_session(client, app, engine).await;
                    }
                    if app.goal_state.take().is_some() {
                        app.push_message(MessageContent::System("Goal cleared.".to_string()));
                    } else {
                        app.push_message(MessageContent::System("No goal to clear.".to_string()));
                    }
                }
                _ => {
                    if app.is_generating
                        && app
                            .goal_state
                            .as_ref()
                            .is_some_and(|goal| goal.status == GoalStatus::Active)
                    {
                        abort_session(client, app, engine).await;
                    }
                    app.goal_state = Some(GoalState::new(goal_arg.to_string()));
                    let _ = submit_goal_iteration(client, app);
                }
            }
        }
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
                app.push_message(MessageContent::System(format!("Retrying: {preview}")));
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
                    app.push_message(MessageContent::System(format!("✓ Exported to {filename}")));
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
                    app.push_message(MessageContent::System(format!("Feedback task failed: {e}")));
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
                    app.push_message(MessageContent::System(format!("Running skill: {name}")));
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
        CommandResult::SecurityReview => {
            let cwd = std::env::current_dir().unwrap_or_default();
            let diff = tokio::task::spawn_blocking(move || {
                std::process::Command::new("git")
                    .args(["diff", "HEAD"])
                    .current_dir(&cwd)
                    .output()
                    .ok()
                    .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
                    .unwrap_or_default()
            })
            .await
            .unwrap_or_default();
            let diff_short = if diff.len() > 12000 {
                format!(
                    "{}…\n[truncated, {} bytes total]",
                    &diff[..12000],
                    diff.len()
                )
            } else if diff.is_empty() {
                "No changes found (working tree matches HEAD).".to_string()
            } else {
                diff
            };
            let review_prompt = format!(
                "You are a senior security engineer conducting a focused security review of the changes on this branch.\n\n```\n{}\n```\n\nFocus on high-confidence vulnerabilities. Skip: DoS, secrets-on-disk, rate-limiting, dependency confusion.\nReport format: markdown with file, line, severity (CRITICAL/HIGH/MEDIUM/LOW), category, and fix recommendation.",
                diff_short
            );
            let _ = client.submit(&review_prompt);
            app.mark_generating();
        }
        CommandResult::Statusline { prompt } => {
            let p = if prompt.is_empty() {
                "Configure my statusLine from my shell PS1 configuration"
            } else {
                &prompt
            };
            let agent_prompt = format!(
                "You are a statusline-setup agent. Read shell config (~/.bashrc, ~/.zshrc, etc.) and configure Claude Code status display. User request: {p}"
            );
            let _ = client.submit(&agent_prompt);
            app.mark_generating();
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
        | CommandResult::Teleport
        | CommandResult::Hooks
        | CommandResult::Tasks { .. }
        | CommandResult::Color { .. }
        | CommandResult::Advisor { .. }
        | CommandResult::Sandbox { .. }
        | CommandResult::Ide { .. }
        | CommandResult::Keybindings
        | CommandResult::Session
        | CommandResult::TerminalSetup
        | CommandResult::Desktop
        | CommandResult::Mobile
        | CommandResult::Install { .. }
        | CommandResult::Upgrade
        | CommandResult::PrivacySettings
        | CommandResult::Onboarding
        | CommandResult::Passes => {
            // Should not reach here — these are handled in handle_slash_command
        }
    }
}
