use super::*;

pub(super) fn plain_text_lines(text: &str) -> Vec<Line<'static>> {
    if text.is_empty() {
        return vec![];
    }

    let dim = Style::default().fg(MUTED);
    let prefix = Span::styled("\u{25CF} ", dim);
    let blank_prefix = Span::raw("   ");
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
pub(super) fn parse_inline_spans(line: &str) -> Vec<Span<'static>> {
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

pub(super) fn extract_tool_input_display(
    tool_name: &str,
    input: &serde_json::Value,
) -> Option<String> {
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

pub(super) fn should_clear_message_area(
    previous_total_visual: Option<usize>,
    total_visual: usize,
) -> bool {
    previous_total_visual.is_some_and(|previous| previous > total_visual)
}

pub(super) fn completion_popup_rows_from_count(match_count: usize) -> u16 {
    if match_count <= 1 {
        0
    } else {
        match_count.min(MAX_COMPLETION_POPUP_ITEMS) as u16
    }
}

pub(super) fn completion_popup_rows(app: &App) -> u16 {
    completion_popup_rows_from_count(app.input.completion_matches().len())
}

pub(super) fn completion_popup_area(
    popup_slot: Rect,
    input_area: Rect,
    matches: &[&str],
) -> Option<Rect> {
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

    pub(super) fn handle_key(&mut self, code: KeyCode) -> FooterPickerAction {
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

pub(super) fn footer_menu_rows(app: &App) -> u16 {
    app.footer_picker
        .as_ref()
        .map_or_else(|| completion_popup_rows(app), FooterPicker::visible_rows)
}

pub(super) fn should_render_print_output_in_overlay(text: &str) -> bool {
    text.lines().nth(12).is_some() || text.len() > 600
}

pub(super) fn footer_picker_from_overlay(
    kind: FooterPickerKind,
    overlay: Overlay,
) -> Option<FooterPicker> {
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

pub(super) fn build_model_picker(current_model: &str) -> FooterPicker {
    footer_picker_from_overlay(
        FooterPickerKind::Model,
        overlay::build_model_overlay(current_model),
    )
    .expect("model overlay should be a selection list")
}

pub(super) fn build_theme_picker(current_theme: &str) -> FooterPicker {
    footer_picker_from_overlay(
        FooterPickerKind::Theme,
        overlay::build_theme_overlay(current_theme),
    )
    .expect("theme overlay should be a selection list")
}

pub(super) fn build_permission_overlay(
    current_mode: clawed_core::permissions::PermissionMode,
) -> Overlay {
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

pub(super) fn build_permissions_picker(
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

pub(super) fn build_skills_picker(
    skills: &[clawed_core::skills::SkillEntry],
) -> Option<FooterPicker> {
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

pub(super) fn build_resume_picker() -> Option<FooterPicker> {
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

pub(super) fn restore_terminal_after_tui() {
    clawed_tools::diff_ui::set_tui_mode(false);
    let _ = std::io::Write::write_all(&mut std::io::stdout(), b"\x1b[?1002l");
    let _ = crossterm::execute!(std::io::stdout(), DisableMouseCapture);
    let _ = crossterm::execute!(std::io::stdout(), DisableBracketedPaste);
    let _ = crossterm::execute!(
        std::io::stdout(),
        crossterm::event::PopKeyboardEnhancementFlags
    );
    let _ = crossterm::execute!(std::io::stdout(), crossterm::cursor::Show);
    let _ = crossterm::execute!(std::io::stdout(), crossterm::terminal::LeaveAlternateScreen);
    let _ = crossterm::terminal::disable_raw_mode();
}

pub(super) fn reenter_tui_terminal(terminal: &mut TuiTerminal) -> io::Result<()> {
    crossterm::terminal::enable_raw_mode()?;
    crossterm::execute!(std::io::stdout(), EnableMouseCapture)?;
    let _ = std::io::Write::write_all(&mut std::io::stdout(), b"\x1b[?1002h");
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

pub(super) fn with_tui_suspended<T, F>(terminal: &mut TuiTerminal, action: F) -> anyhow::Result<T>
where
    F: FnOnce() -> T,
{
    restore_terminal_after_tui();
    let result = action();
    reenter_tui_terminal(terminal)?;
    Ok(result)
}

impl RightPanelFocus {
    pub(super) fn next(self) -> Option<Self> {
        match self {
            Self::Tasks => Some(Self::ToolHistory),
            Self::ToolHistory => Some(Self::Stats),
            Self::Stats => None,
        }
    }
}

/// Detect the IDE environment from environment variables.
/// Returns `Some("vscode")`, `Some("jetbrains")`, etc., or `None` for standalone terminal.
pub(super) fn detect_ide() -> Option<String> {
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

// -- /agents TUI formatter ----------------------------------------------------

/// Format `/agents [sub]` output as plain text for a TUI info overlay.
pub(super) fn format_agents_tui(
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
pub(super) fn read_clipboard_image() -> anyhow::Result<ImageAttachment> {
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
pub(super) async fn replay_session_messages(engine: &Arc<QueryEngine>, app: &mut App) {
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
pub(super) fn agent_color_for_id(agent_id: &str) -> Color {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    agent_id.hash(&mut hasher);
    let idx = (hasher.finish() as usize) % AGENT_COLOR_PALETTE.len();
    AGENT_COLOR_PALETTE[idx].0
}

/// Render "Viewing @agent_name · esc return" header above messages.
pub(super) fn render_teammate_view_header(frame: &mut Frame, area: Rect, app: &App) {
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
