//! Overlay system for TUI slash-command output.
//!
//! Two overlay types:
//! - **SelectionList**: interactive picker (e.g. `/model`, `/theme`)
//! - **InfoPanel**: scrollable read-only text (e.g. `/status`, `/help`, `/config`)
//!
//! Overlays render on top of the message area and capture all keyboard input
//! until dismissed with Esc.

use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap},
    Frame,
};

// ── Data types ───────────────────────────────────────────────────────────────

/// A single item in a `SelectionList` overlay.
#[derive(Debug, Clone)]
pub struct SelectionItem {
    /// Display label shown in the picker.
    pub label: String,
    /// Optional description shown to the right.
    pub description: String,
    /// The value returned on selection (e.g. model ID).
    pub value: String,
    /// Whether this is the currently active item.
    pub is_current: bool,
}

/// The active overlay, if any.
#[derive(Debug)]
pub enum Overlay {
    /// Interactive picker list (e.g. `/model`, `/theme`).
    SelectionList {
        title: String,
        items: Vec<SelectionItem>,
        selected: usize,
        scroll_offset: usize,
    },
    /// Read-only scrollable info panel (e.g. `/status`, `/help`, `/config`).
    InfoPanel {
        title: String,
        lines: Vec<Line<'static>>,
        scroll_offset: usize,
    },
}

// ── Overlay actions ──────────────────────────────────────────────────────────

/// Result of handling a key event inside an overlay.
pub enum OverlayAction {
    /// Overlay consumed the key, no further action needed.
    Consumed,
    /// User dismissed the overlay (Esc / q in info panel).
    Dismissed,
    /// User selected an item in a SelectionList; value is returned.
    Selected(String),
}

// ── Key handling ─────────────────────────────────────────────────────────────

impl Overlay {
    /// Handle a key event. Returns the action to take.
    pub fn handle_key(&mut self, code: crossterm::event::KeyCode) -> OverlayAction {
        use crossterm::event::KeyCode;
        match self {
            Overlay::SelectionList {
                items,
                selected,
                scroll_offset,
                ..
            } => match code {
                KeyCode::Esc => OverlayAction::Dismissed,
                KeyCode::Up | KeyCode::Char('k') => {
                    if *selected > 0 {
                        *selected -= 1;
                        // Keep selected in view
                        if *selected < *scroll_offset {
                            *scroll_offset = *selected;
                        }
                    }
                    OverlayAction::Consumed
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    if *selected + 1 < items.len() {
                        *selected += 1;
                    }
                    OverlayAction::Consumed
                }
                KeyCode::Enter => {
                    if let Some(item) = items.get(*selected) {
                        OverlayAction::Selected(item.value.clone())
                    } else {
                        OverlayAction::Dismissed
                    }
                }
                KeyCode::Home => {
                    *selected = 0;
                    *scroll_offset = 0;
                    OverlayAction::Consumed
                }
                KeyCode::End => {
                    *selected = items.len().saturating_sub(1);
                    OverlayAction::Consumed
                }
                _ => OverlayAction::Consumed,
            },
            Overlay::InfoPanel {
                lines,
                scroll_offset,
                ..
            } => match code {
                KeyCode::Esc | KeyCode::Char('q') => OverlayAction::Dismissed,
                KeyCode::Up | KeyCode::Char('k') => {
                    *scroll_offset = scroll_offset.saturating_sub(1);
                    OverlayAction::Consumed
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    *scroll_offset = scroll_offset
                        .saturating_add(1)
                        .min(lines.len().saturating_sub(1));
                    OverlayAction::Consumed
                }
                KeyCode::PageUp => {
                    *scroll_offset = scroll_offset.saturating_sub(10);
                    OverlayAction::Consumed
                }
                KeyCode::PageDown => {
                    *scroll_offset = scroll_offset
                        .saturating_add(10)
                        .min(lines.len().saturating_sub(1));
                    OverlayAction::Consumed
                }
                KeyCode::Home => {
                    *scroll_offset = 0;
                    OverlayAction::Consumed
                }
                KeyCode::End => {
                    *scroll_offset = lines.len().saturating_sub(1);
                    OverlayAction::Consumed
                }
                _ => OverlayAction::Consumed,
            },
        }
    }
}

// ── Rendering ────────────────────────────────────────────────────────────────

/// Render the overlay on top of the given area (typically the messages area).
pub fn render(frame: &mut Frame, area: Rect, overlay: &Overlay) {
    // Overlay occupies the message area with a 1-cell margin on each side
    let margin = 1u16;
    let overlay_area = Rect {
        x: area.x + margin,
        y: area.y + margin,
        width: area.width.saturating_sub(margin * 2),
        height: area.height.saturating_sub(margin * 2),
    };

    if overlay_area.width < 10 || overlay_area.height < 4 {
        return; // too small to render
    }

    // Clear the area first
    frame.render_widget(Clear, overlay_area);

    match overlay {
        Overlay::SelectionList {
            title,
            items,
            selected,
            scroll_offset,
        } => render_selection_list(frame, overlay_area, title, items, *selected, *scroll_offset),
        Overlay::InfoPanel {
            title,
            lines,
            scroll_offset,
        } => render_info_panel(frame, overlay_area, title, lines, *scroll_offset),
    }
}

fn render_selection_list(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    items: &[SelectionItem],
    selected: usize,
    scroll_offset: usize,
) {
    let inner_height = area.height.saturating_sub(3) as usize; // borders + hint line

    // Keep selected in visible window
    let scroll = if selected >= scroll_offset + inner_height {
        selected - inner_height + 1
    } else if selected < scroll_offset {
        selected
    } else {
        scroll_offset
    };

    let list_items: Vec<ListItem> = items
        .iter()
        .enumerate()
        .skip(scroll)
        .take(inner_height)
        .map(|(i, item)| {
            let is_sel = i == selected;
            let marker = if item.is_current { "● " } else { "  " };

            let label_style = if is_sel {
                Style::default()
                    .fg(Color::White)
                    .bg(Color::Blue)
                    .add_modifier(Modifier::BOLD)
            } else if item.is_current {
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().add_modifier(Modifier::BOLD) // bold = bright regardless of palette
            };

            let desc_style = if is_sel {
                Style::default().fg(Color::LightCyan).bg(Color::Blue)
            } else {
                Style::default() // normal weight — visually secondary to bold label
            };

            let marker_style = if is_sel {
                Style::default().fg(Color::Cyan).bg(Color::Blue)
            } else if item.is_current {
                Style::default().fg(Color::Cyan)
            } else {
                Style::default()
            };

            let mut spans = vec![
                Span::styled(marker.to_string(), marker_style),
                Span::styled(item.label.clone(), label_style),
            ];
            if !item.description.is_empty() {
                spans.push(Span::styled(format!("  {}", item.description), desc_style));
            }
            ListItem::new(Line::from(spans))
        })
        .collect();

    // Scroll indicators
    let has_above = scroll > 0;
    let has_below = scroll + inner_height < items.len();
    let scroll_hint = match (has_above, has_below) {
        (true, true) => " ↑↓ ",
        (true, false) => " ↑ ",
        (false, true) => " ↓ ",
        (false, false) => "",
    };

    let block_title = format!(" {title} ({}/{}) {scroll_hint}", selected + 1, items.len());
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(block_title)
        .title_bottom(Line::styled(
            " ↑↓/jk: navigate  Enter: select  Esc: cancel ",
            Style::default(),
        ));

    let list = List::new(list_items).block(block);
    frame.render_widget(list, area);
}

fn render_info_panel(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    lines: &[Line<'static>],
    scroll_offset: usize,
) {
    let inner_height = area.height.saturating_sub(2) as usize; // borders

    let has_above = scroll_offset > 0;
    let has_below = scroll_offset + inner_height < lines.len();
    let scroll_hint = match (has_above, has_below) {
        (true, true) => " ↑↓ ",
        (true, false) => " ↑ ",
        (false, true) => " ↓ ",
        (false, false) => "",
    };

    let block_title = format!(" {title} {scroll_hint}");
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(block_title)
        .title_bottom(Line::styled(
            " ↑↓/jk: scroll  Esc/q: close ",
            Style::default(),
        ));

    // Manually slice lines for scroll support (Paragraph's scroll doesn't
    // work well with styled lines that wrap).
    let visible: Vec<Line> = lines
        .iter()
        .skip(scroll_offset)
        .take(inner_height)
        .cloned()
        .collect();

    let paragraph = Paragraph::new(visible)
        .block(block)
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
}

// ── Builders ─────────────────────────────────────────────────────────────────

/// Build a model selection overlay showing all aliases + current model.
pub fn build_model_overlay(current_model: &str) -> Overlay {
    let aliases = clawed_core::model::list_aliases();
    let mut items: Vec<SelectionItem> = aliases
        .iter()
        .map(|(alias, model_id)| {
            let display = clawed_core::model::display_name_any(model_id);
            let caps = clawed_core::model::model_capabilities(model_id);
            SelectionItem {
                label: (*alias).to_string(),
                description: if caps.supports_1m {
                    format!("→ {display}  (supports 1M)")
                } else {
                    format!("→ {display}")
                },
                value: alias.to_string(),
                is_current: model_id == current_model,
            }
        })
        .collect();

    // Add 1M context variants for models that support it
    let mut one_m_items: Vec<SelectionItem> = aliases
        .iter()
        .filter(|(a, model_id)| {
            let caps = clawed_core::model::model_capabilities(model_id);
            caps.supports_1m && *a != "best" // skip "best" alias (duplicate of opus)
        })
        .map(|(alias, model_id)| {
            let display = clawed_core::model::display_name_any(model_id);
            let label = format!("{alias}[1m]");
            SelectionItem {
                description: format!("→ {display} (1M context)"),
                label,
                value: format!("{alias}[1m]"),
                is_current: false,
            }
        })
        .collect();
    items.append(&mut one_m_items);

    // Deduplicate (opus and best resolve to same model)
    items.dedup_by(|a, b| a.value == b.value);

    // Pre-select the current model
    let selected = items.iter().position(|i| i.is_current).unwrap_or(0);

    Overlay::SelectionList {
        title: "Switch Model".to_string(),
        items,
        selected,
        scroll_offset: 0,
    }
}

/// Build a theme selection overlay.
pub fn build_theme_overlay(current_theme: &str) -> Overlay {
    let items: Vec<SelectionItem> = crate::theme::ThemeSetting::ALL
        .iter()
        .map(|setting| SelectionItem {
            label: setting.display_name().to_string(),
            description: theme_setting_value(*setting).to_string(),
            value: theme_setting_value(*setting).to_string(),
            is_current: crate::repl_commands::setting_to_name(*setting)
                .is_some_and(|theme_name| theme_name.as_str() == current_theme),
        })
        .collect();

    let selected = items.iter().position(|i| i.is_current).unwrap_or(0);

    Overlay::SelectionList {
        title: "Theme".to_string(),
        items,
        selected,
        scroll_offset: 0,
    }
}

/// Strip ANSI/VT100 escape sequences from a string.
/// The help text is generated with ANSI codes for REPL mode; we must remove
/// them before passing the text to ratatui which manages styling itself.
pub(crate) fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            if chars.peek() == Some(&'[') {
                chars.next(); // consume '['
                              // skip CSI parameters and intermediate bytes until final byte
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            // other escape sequences: just skip the ESC
        } else {
            out.push(ch);
        }
    }
    out
}

fn theme_setting_value(setting: crate::theme::ThemeSetting) -> &'static str {
    match setting {
        crate::theme::ThemeSetting::Auto => "auto",
        crate::theme::ThemeSetting::Dark => "dark",
        crate::theme::ThemeSetting::Light => "light",
        crate::theme::ThemeSetting::DarkDaltonized => "dark-daltonized",
        crate::theme::ThemeSetting::LightDaltonized => "light-daltonized",
        crate::theme::ThemeSetting::DarkAnsi => "dark-ansi",
        crate::theme::ThemeSetting::LightAnsi => "light-ansi",
    }
}

/// Build an info panel overlay from a title and multi-line text.
///
/// Applies ratatui styling per line:
/// - Lines with no leading whitespace → section header (Cyan Bold)
/// - Lines starting with `/` after indent → command name (White) + description (Muted)
/// - Lines starting with `•` after indent → tip text (Muted)
/// - Everything else → terminal default
pub fn build_info_overlay(title: &str, text: &str) -> Overlay {
    let lines: Vec<Line<'static>> = text
        .lines()
        .map(|l| style_info_line(&strip_ansi(l)))
        .collect();

    Overlay::InfoPanel {
        title: title.to_string(),
        lines,
        scroll_offset: 0,
    }
}

fn style_info_line(line: &str) -> Line<'static> {
    if line.is_empty() {
        return Line::from("");
    }

    let trimmed = line.trim_start();
    let indent_len = line.len() - trimmed.len();
    let indent = " ".repeat(indent_len);

    // Section header: no leading whitespace
    if indent_len == 0 {
        return Line::styled(
            line.to_string(),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        );
    }

    // Tip line: starts with bullet — secondary/dim text
    if trimmed.starts_with('•') {
        return Line::from(vec![
            Span::raw(indent),
            Span::styled(trimmed.to_string(), Style::default()),
        ]);
    }

    // Command line: starts with '/' — bold command name, normal description
    if trimmed.starts_with('/') {
        // Split on first run of 2+ spaces to separate command from description
        if let Some(space_pos) = trimmed.find("  ") {
            let cmd = &trimmed[..space_pos];
            let desc = trimmed[space_pos..].trim_start();
            return Line::from(vec![
                Span::raw(indent),
                Span::styled(
                    cmd.to_string(),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::raw("  "),
                Span::styled(desc.to_string(), Style::default()),
            ]);
        }
        return Line::from(vec![
            Span::raw(indent),
            Span::styled(
                trimmed.to_string(),
                Style::default().add_modifier(Modifier::BOLD),
            ),
        ]);
    }

    // Default
    Line::styled(line.to_string(), Style::default())
}

/// Build a styled info panel from pre-rendered ratatui Lines.
#[allow(dead_code)]
pub fn build_styled_info_overlay(title: &str, lines: Vec<Line<'static>>) -> Overlay {
    Overlay::InfoPanel {
        title: title.to_string(),
        lines,
        scroll_offset: 0,
    }
}

/// Build a status overlay with colored sections.
pub fn build_status_overlay(
    model: &str,
    turns: u32,
    input_tokens: u64,
    output_tokens: u64,
    elapsed_secs: u64,
) -> Overlay {
    let display = clawed_core::model::display_name_any(model);
    let total = input_tokens + output_tokens;
    let label = Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD);
    let value = Style::default(); // use terminal default to avoid ANSI color remapping

    let lines = vec![
        Line::from(""),
        Line::from(vec![
            Span::styled("  Model:    ", label),
            Span::styled(format!("{display} ({model})"), value),
        ]),
        Line::from(vec![
            Span::styled("  Turns:    ", label),
            Span::styled(format!("{turns}"), value),
        ]),
        Line::from(vec![
            Span::styled("  Tokens:   ", label),
            Span::styled(
                format!("{input_tokens}↑ in / {output_tokens}↓ out ({total} total)"),
                value,
            ),
        ]),
        Line::from(vec![
            Span::styled("  Elapsed:  ", label),
            Span::styled(format_elapsed(elapsed_secs), value),
        ]),
        Line::from(vec![
            Span::styled("  Version:  ", label),
            Span::styled(format!("v{}", env!("CARGO_PKG_VERSION")), value),
        ]),
        Line::from(""),
    ];

    Overlay::InfoPanel {
        title: "Session Status".to_string(),
        lines,
        scroll_offset: 0,
    }
}

/// Run local environment diagnostics and build an InfoPanel overlay.
pub async fn build_doctor_overlay(
    engine: &clawed_agent::engine::QueryEngine,
    cwd: &std::path::Path,
) -> Overlay {
    let ok = Style::default().fg(Color::Green);
    let warn = Style::default().fg(Color::Yellow);
    let err = Style::default().fg(Color::Red);
    let dim = Style::default();
    let label = Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD);

    let mut lines: Vec<Line<'static>> = vec![Line::from("")];
    let mut warnings = 0u32;
    let mut errors = 0u32;

    macro_rules! row {
        ($icon:expr, $style:expr, $msg:expr) => {
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled($icon, $style),
                Span::raw(" "),
                Span::raw($msg),
            ]));
        };
    }

    // 1. API key
    if std::env::var("ANTHROPIC_API_KEY").is_ok() {
        row!("✓", ok, "ANTHROPIC_API_KEY configured");
    } else {
        row!("✗", err, "ANTHROPIC_API_KEY not set");
        errors += 1;
    }

    // Parallel tool checks
    let cwd_owned = cwd.to_path_buf();
    let (git_ver, git_repo, rg_ver, node_ver) = tokio::join!(
        tokio::task::spawn_blocking(|| {
            std::process::Command::new("git").arg("--version").output()
        }),
        tokio::task::spawn_blocking(move || {
            std::process::Command::new("git")
                .args(["rev-parse", "--is-inside-work-tree"])
                .current_dir(&cwd_owned)
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
        }),
        tokio::task::spawn_blocking(|| {
            std::process::Command::new("rg").arg("--version").output()
        }),
        tokio::task::spawn_blocking(|| {
            std::process::Command::new("node").arg("--version").output()
        }),
    );

    // 2. Git version
    match git_ver.ok().and_then(|r| r.ok()) {
        Some(out) if out.status.success() => {
            let ver = String::from_utf8_lossy(&out.stdout).trim().to_string();
            row!("✓", ok, ver);
        }
        _ => {
            row!("✗", err, "git not found in PATH");
            errors += 1;
        }
    }

    // 3. Git repo
    if git_repo.unwrap_or(false) {
        row!("✓", ok, "Inside git repository");
    } else {
        row!("⚠", warn, "Not inside a git repository");
        warnings += 1;
    }

    // 4. CLAUDE.md
    let claude_md = cwd.join("CLAUDE.md");
    if claude_md.exists() {
        let size = std::fs::metadata(&claude_md).map(|m| m.len()).unwrap_or(0);
        row!("✓", ok, format!("CLAUDE.md found ({size} bytes)"));
    } else {
        row!("⚠", warn, "No CLAUDE.md — run /init to create one");
        warnings += 1;
    }

    // 5. .claude/rules/
    let rules_dir = cwd.join(".claude").join("rules");
    if rules_dir.is_dir() {
        let count = std::fs::read_dir(&rules_dir)
            .into_iter()
            .flatten()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|x| x == "md"))
            .count();
        if count > 0 {
            row!("✓", ok, format!(".claude/rules/: {count} rule file(s)"));
        } else {
            row!("·", dim, ".claude/rules/ exists (empty)");
        }
    }

    // 6. .claude/skills/
    let skills_dir = cwd.join(".claude").join("skills");
    if skills_dir.is_dir() {
        let count = std::fs::read_dir(&skills_dir)
            .into_iter()
            .flatten()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|x| x == "md"))
            .count();
        if count > 0 {
            row!("✓", ok, format!(".claude/skills/: {count} skill(s)"));
        } else {
            row!("·", dim, ".claude/skills/ exists (empty)");
        }
    }

    // 7. Memory files
    let mem_files = clawed_core::memory::list_memory_files(cwd);
    if !mem_files.is_empty() {
        row!("✓", ok, format!("{} memory file(s)", mem_files.len()));
    }

    // 8. Sessions
    let sessions = clawed_core::session::list_sessions();
    if !sessions.is_empty() {
        let latest_age = clawed_core::session::format_age(&sessions[0].updated_at);
        row!(
            "✓",
            ok,
            format!("{} saved session(s), latest: {latest_age}", sessions.len())
        );
    }

    // 9. Settings
    let loaded = clawed_core::config::Settings::load_merged(cwd);
    if loaded.layers.is_empty() {
        row!("·", dim, "Using default settings (no config files found)");
    } else {
        let sources: Vec<String> = loaded.sources.iter().map(|s| s.to_string()).collect();
        row!("✓", ok, format!("Settings: {}", sources.join(", ")));
    }

    // 10. Ripgrep
    match rg_ver.ok().and_then(|r| r.ok()) {
        Some(out) if out.status.success() => {
            let ver = String::from_utf8_lossy(&out.stdout)
                .lines()
                .next()
                .unwrap_or("")
                .to_string();
            row!("✓", ok, ver);
        }
        _ => {
            row!("⚠", warn, "ripgrep (rg) not found — GrepTool may not work");
            warnings += 1;
        }
    }

    // 11. Node.js
    match node_ver.ok().and_then(|r| r.ok()) {
        Some(out) if out.status.success() => {
            let ver = String::from_utf8_lossy(&out.stdout).trim().to_string();
            row!("✓", ok, format!("Node.js {ver}"));
        }
        _ => {
            row!("·", dim, "Node.js not found (optional, for MCP servers)");
        }
    }

    // 12. Model + engine info
    {
        let s = engine.state().read().await;
        let display = clawed_core::model::display_name_any(&s.model);
        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            Span::styled("  Model:       ", label),
            Span::raw(format!("{display} ({})", s.model)),
        ]));
        lines.push(Line::from(vec![
            Span::styled("  Permission:  ", label),
            Span::raw(format!("{:?}", s.permission_mode)),
        ]));
        lines.push(Line::from(vec![
            Span::styled("  Tools:       ", label),
            Span::raw(format!("{} registered", engine.tool_count())),
        ]));
        if let Some(pct) = engine.context_usage_percent().await {
            lines.push(Line::from(vec![
                Span::styled("  Context:     ", label),
                Span::raw(format!("{pct}%")),
            ]));
        }
    }

    // 13. MCP config
    let mcp_config = cwd.join(".claude").join("mcp.json");
    if mcp_config.exists() {
        if let Ok(content) = std::fs::read_to_string(&mcp_config) {
            match serde_json::from_str::<serde_json::Value>(&content) {
                Ok(val) => {
                    let count = val
                        .get("mcpServers")
                        .and_then(|s| s.as_object())
                        .map(|o| o.len())
                        .unwrap_or(0);
                    row!("✓", ok, format!("MCP config: {count} server(s)"));
                }
                Err(_) => {
                    row!("⚠", warn, ".claude/mcp.json: invalid JSON");
                    warnings += 1;
                }
            }
        }
    }

    // 14. Custom provider env vars
    if let Ok(base_url) = std::env::var("ANTHROPIC_BASE_URL") {
        lines.push(Line::from(vec![
            Span::styled("  Base URL:    ", label),
            Span::raw(base_url),
        ]));
    }
    if let Ok(provider) = std::env::var("CLAUDE_CODE_PROVIDER") {
        lines.push(Line::from(vec![
            Span::styled("  Provider:    ", label),
            Span::raw(provider),
        ]));
    }

    // Summary
    lines.push(Line::from(""));
    if errors == 0 && warnings == 0 {
        lines.push(Line::from(vec![Span::styled(
            "  🎉 All checks passed!",
            ok.add_modifier(Modifier::BOLD),
        )]));
    } else {
        if errors > 0 {
            lines.push(Line::from(vec![Span::styled(
                format!("  ✗ {errors} error(s)"),
                err.add_modifier(Modifier::BOLD),
            )]));
        }
        if warnings > 0 {
            lines.push(Line::from(vec![Span::styled(
                format!("  ⚠ {warnings} warning(s)"),
                warn.add_modifier(Modifier::BOLD),
            )]));
        }
    }
    lines.push(Line::from(""));

    Overlay::InfoPanel {
        title: "Doctor".to_string(),
        lines,
        scroll_offset: 0,
    }
}

/// Format elapsed seconds for overlay display.
/// Note: distinct from `verbs::format_duration` which takes milliseconds
/// and is used for turn-completion / status rendering.
fn format_elapsed(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m {}s", secs / 60, secs % 60)
    } else {
        format!("{}h {}m {}s", secs / 3600, (secs % 3600) / 60, secs % 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyCode;

    #[test]
    fn selection_navigate_down() {
        let items = vec![
            SelectionItem {
                label: "a".into(),
                description: String::new(),
                value: "a".into(),
                is_current: true,
            },
            SelectionItem {
                label: "b".into(),
                description: String::new(),
                value: "b".into(),
                is_current: false,
            },
        ];
        let mut overlay = Overlay::SelectionList {
            title: "test".into(),
            items,
            selected: 0,
            scroll_offset: 0,
        };
        let action = overlay.handle_key(KeyCode::Down);
        assert!(matches!(action, OverlayAction::Consumed));
        if let Overlay::SelectionList { selected, .. } = &overlay {
            assert_eq!(*selected, 1);
        }
    }

    #[test]
    fn selection_enter_returns_value() {
        let items = vec![SelectionItem {
            label: "sonnet".into(),
            description: String::new(),
            value: "sonnet".into(),
            is_current: false,
        }];
        let mut overlay = Overlay::SelectionList {
            title: "test".into(),
            items,
            selected: 0,
            scroll_offset: 0,
        };
        let action = overlay.handle_key(KeyCode::Enter);
        assert!(matches!(action, OverlayAction::Selected(v) if v == "sonnet"));
    }

    #[test]
    fn selection_esc_dismisses() {
        let mut overlay = Overlay::SelectionList {
            title: "test".into(),
            items: vec![],
            selected: 0,
            scroll_offset: 0,
        };
        let action = overlay.handle_key(KeyCode::Esc);
        assert!(matches!(action, OverlayAction::Dismissed));
    }

    #[test]
    fn info_panel_scroll() {
        let lines: Vec<Line> = (0..20).map(|i| Line::raw(format!("line {i}"))).collect();
        let mut overlay = Overlay::InfoPanel {
            title: "test".into(),
            lines,
            scroll_offset: 0,
        };
        overlay.handle_key(KeyCode::Down);
        if let Overlay::InfoPanel { scroll_offset, .. } = &overlay {
            assert_eq!(*scroll_offset, 1);
        }
    }

    #[test]
    fn info_panel_q_dismisses() {
        let mut overlay = Overlay::InfoPanel {
            title: "test".into(),
            lines: vec![],
            scroll_offset: 0,
        };
        let action = overlay.handle_key(KeyCode::Char('q'));
        assert!(matches!(action, OverlayAction::Dismissed));
    }

    #[test]
    fn build_model_overlay_has_items() {
        let overlay = build_model_overlay("claude-sonnet-4-6");
        if let Overlay::SelectionList { items, .. } = &overlay {
            assert!(!items.is_empty());
        } else {
            panic!("expected SelectionList");
        }
    }

    #[test]
    fn build_status_overlay_has_lines() {
        let overlay = build_status_overlay("claude-sonnet-4-6", 5, 1000, 500, 120);
        if let Overlay::InfoPanel { lines, .. } = &overlay {
            assert!(lines.len() >= 5);
        } else {
            panic!("expected InfoPanel");
        }
    }
}
