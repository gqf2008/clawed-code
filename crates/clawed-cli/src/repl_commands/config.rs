//! /config, /context, /login, /logout command handlers.

use std::fmt::Write as _;
use std::io::Write as _;
use std::path::Path;

use clawed_agent::engine::QueryEngine;

/// Show current configuration.
pub(crate) fn handle_config_command(cwd: &Path) {
    println!("{}", handle_config_command_str(cwd));
}

pub(crate) fn handle_config_command_str(cwd: &Path) -> String {
    let loaded = clawed_core::config::Settings::load_merged(cwd);
    let mut out = String::from("\x1b[1mConfiguration\x1b[0m\n");
    out.push_str(&loaded.display_sources());

    let claude_md = cwd.join("CLAUDE.md");
    if claude_md.exists() {
        let size = std::fs::metadata(&claude_md).map_or(0, |metadata| metadata.len());
        let _ = writeln!(out, "  CLAUDE.md: {} ({} bytes)", claude_md.display(), size);
    }

    out.push_str("\n\x1b[1mSettings files:\x1b[0m\n");
    if let Some(user_path) = dirs::home_dir().map(|home| home.join(".claude").join("settings.json"))
    {
        let exists = if user_path.exists() { "✓" } else { "✗" };
        let _ = writeln!(out, "  {} User:    {}", exists, user_path.display());
    }

    let proj_path = cwd.join(".claude").join("settings.json");
    let proj_exists = if proj_path.exists() { "✓" } else { "✗" };
    let _ = writeln!(out, "  {} Project: {}", proj_exists, proj_path.display());

    let local_path = cwd.join(".claude").join("settings.local.json");
    let local_exists = if local_path.exists() { "✓" } else { "✗" };
    let _ = write!(out, "  {} Local:   {}", local_exists, local_path.display());

    out
}

/// Show loaded context details.
pub(crate) async fn handle_context(engine: &QueryEngine, cwd: &Path) {
    println!("{}", handle_context_str(engine, cwd).await);
}

pub(crate) async fn handle_context_str(engine: &QueryEngine, cwd: &Path) -> String {
    let mut out = String::from("\x1b[1;36m── Loaded Context ──\x1b[0m\n\n");

    let state = engine.state().read().await;
    let display = clawed_core::model::display_name_any(&state.model);
    let _ = writeln!(out, "\x1b[1mModel:\x1b[0m {} ({})", display, state.model);
    let _ = writeln!(
        out,
        "\x1b[1mPermission mode:\x1b[0m {:?}",
        state.permission_mode
    );
    let _ = writeln!(out, "\x1b[1mTurns:\x1b[0m {}", state.turn_count);
    let _ = writeln!(out, "\x1b[1mMessages:\x1b[0m {}", state.messages.len());
    drop(state);

    let loaded = clawed_core::config::Settings::load_merged(cwd);
    out.push_str("\n\x1b[1;33m── Settings ──\x1b[0m\n");
    if loaded.layers.is_empty() {
        out.push_str("  \x1b[2m(defaults only)\x1b[0m\n");
    } else {
        for (source, _) in &loaded.layers {
            let _ = writeln!(out, "  ✓ {}", source);
        }
    }
    let _ = writeln!(out, "{}", loaded.settings.summary());

    out.push_str("\n\x1b[1;33m── Tools ──\x1b[0m\n");
    let _ = writeln!(out, "  {} tool(s) registered", engine.tool_count());

    out.push_str("\n\x1b[1;33m── CLAUDE.md ──\x1b[0m\n");
    let claude_md = clawed_core::claude_md::load_claude_md(cwd);
    if claude_md.is_empty() {
        out.push_str("  \x1b[2m(none found)\x1b[0m\n");
    } else {
        let preview = claude_md.lines().take(20).collect::<Vec<_>>().join("\n");
        let _ = writeln!(out, "{}", preview);
        let total_lines = claude_md.lines().count();
        if total_lines > 20 {
            let _ = writeln!(out, "  \x1b[2m… ({} more lines)\x1b[0m", total_lines - 20);
        }
    }

    out.push_str("\n\x1b[1;33m── Memory ──\x1b[0m\n");
    let mem_files = clawed_core::memory::list_memory_files(cwd);
    if mem_files.is_empty() {
        out.push_str("  \x1b[2m(no memory files)\x1b[0m\n");
    } else {
        for file in &mem_files {
            let type_tag = file
                .memory_type
                .as_ref()
                .map_or_else(String::new, |memory_type| {
                    format!("[{}] ", memory_type.as_str())
                });
            let _ = writeln!(out, "  {}{}", type_tag, file.filename);
        }
    }

    out.push_str("\n\x1b[1;33m── Skills ──\x1b[0m\n");
    let skills = clawed_core::skills::get_skills(cwd);
    if skills.is_empty() {
        out.push_str("  \x1b[2m(no skills)\x1b[0m\n");
    } else {
        for skill in &skills {
            let _ = writeln!(out, "  /{}: {}", skill.name, skill.description);
        }
    }

    out.push_str("\n\x1b[1;33m── Hooks ──\x1b[0m\n");
    let hooks = &loaded.settings.hooks;
    let hook_counts = [
        ("PreToolUse", hooks.pre_tool_use.len()),
        ("PostToolUse", hooks.post_tool_use.len()),
        ("Stop", hooks.stop.len()),
        ("SessionStart", hooks.session_start.len()),
        ("SessionEnd", hooks.session_end.len()),
        ("UserPromptSubmit", hooks.user_prompt_submit.len()),
    ];
    let total_hooks: usize = hook_counts.iter().map(|(_, count)| count).sum();
    if total_hooks == 0 {
        out.push_str("  \x1b[2m(no hooks configured)\x1b[0m\n");
    } else {
        for (name, count) in &hook_counts {
            if *count > 0 {
                let _ = writeln!(out, "  {}: {} rule(s)", name, count);
            }
        }
    }

    let state = engine.state().read().await;
    let system_tokens = clawed_core::token_estimation::estimate_text_tokens(&claude_md);
    let msg_tokens = clawed_core::token_estimation::estimate_messages_tokens(&state.messages);
    out.push_str("\n\x1b[1;33m── Token Estimates ──\x1b[0m\n");
    let _ = writeln!(out, "  System prompt: ~{} tokens", system_tokens);
    let _ = writeln!(out, "  Conversation:  ~{} tokens", msg_tokens);
    let _ = write!(
        out,
        "  Total:         ~{} tokens",
        system_tokens + msg_tokens
    );

    out
}

pub(crate) fn handle_login() {
    match prompt_for_api_key() {
        Ok(Some(key)) => match save_api_key(&key) {
            Ok(message) => println!("{}", message),
            Err(message) => eprintln!("{}", message),
        },
        Ok(None) => println!("No key provided. Cancelled."),
        Err(message) => eprintln!("{}", message),
    }
}

fn prompt_for_api_key() -> Result<Option<String>, String> {
    print!("Enter your Anthropic API key: ");
    std::io::stdout()
        .flush()
        .map_err(|error| format!("\x1b[31mFailed to flush stdout: {}\x1b[0m", error))?;

    let key = rpassword::read_password()
        .map_err(|error| format!("\x1b[31mFailed to read input: {}\x1b[0m", error))?;
    let trimmed = key.trim().to_string();

    if trimmed.is_empty() {
        Ok(None)
    } else {
        Ok(Some(trimmed))
    }
}

pub(crate) fn prompt_for_api_key_interactive() -> Result<Option<String>, String> {
    prompt_for_api_key()
}

pub(crate) fn save_api_key(key: &str) -> Result<String, String> {
    let Some(config_dir) = clawed_core::config::Settings::config_dir() else {
        return Err("\x1b[31mCannot determine config directory\x1b[0m".to_string());
    };
    let settings_path = config_dir.join("settings.json");
    let mut settings = load_settings_json(&settings_path);

    settings["api_key"] = serde_json::Value::String(key.to_string());
    std::fs::create_dir_all(&config_dir)
        .map_err(|error| format!("\x1b[31mFailed to create config dir: {}\x1b[0m", error))?;

    let masked = if key.len() > 8 {
        format!("{}...{}", &key[..7], &key[key.len() - 4..])
    } else {
        "****".to_string()
    };

    let json = serde_json::to_string_pretty(&settings)
        .map_err(|error| format!("\x1b[31mFailed to serialize settings: {}\x1b[0m", error))?;
    std::fs::write(&settings_path, json)
        .map_err(|error| format!("\x1b[31mFailed to save settings: {}\x1b[0m", error))?;

    let mut out = String::new();
    if !key.starts_with("sk-ant-") && !key.starts_with("sk-") {
        out.push_str("\x1b[33mWarning: API key doesn't start with 'sk-ant-' — this may not be a valid Anthropic key.\x1b[0m\n");
    }
    let _ = write!(
        out,
        "\x1b[32m✓ API key ({}) saved to {}\x1b[0m",
        masked,
        settings_path.display()
    );

    Ok(out)
}

pub(crate) fn handle_logout() {
    match handle_logout_str() {
        Ok(message) => println!("{}", message),
        Err(message) => eprintln!("{}", message),
    }
}

pub(crate) fn handle_logout_str() -> Result<String, String> {
    let Some(config_dir) = clawed_core::config::Settings::config_dir() else {
        return Err("\x1b[31mCannot determine config directory\x1b[0m".to_string());
    };
    let settings_path = config_dir.join("settings.json");
    if !settings_path.exists() {
        return Ok("No saved settings found.".to_string());
    }

    let mut settings = load_settings_json(&settings_path);
    if settings.get("api_key").is_none() {
        return Ok("No saved API key found.".to_string());
    }

    if let Some(object) = settings.as_object_mut() {
        object.remove("api_key");
    }

    let json = serde_json::to_string_pretty(&settings)
        .map_err(|error| format!("\x1b[31mFailed to serialize settings: {}\x1b[0m", error))?;
    std::fs::write(&settings_path, json)
        .map_err(|error| format!("\x1b[31mFailed to update settings: {}\x1b[0m", error))?;

    Ok("\x1b[32m✓ API key removed from settings\x1b[0m".to_string())
}

/// Reload CLAUDE.md, memory, and settings without restarting the session.
pub(crate) async fn handle_reload_context(engine: &QueryEngine, cwd: &Path) {
    println!("{}", handle_reload_context_str(engine, cwd).await);
}

pub(crate) async fn handle_reload_context_str(engine: &QueryEngine, cwd: &Path) -> String {
    let mut out = String::from("\x1b[33mReloading context…\x1b[0m\n");

    let loaded = clawed_core::config::Settings::load_merged(cwd);
    let _ = writeln!(
        out,
        "  ✓ Settings reloaded ({} source(s))",
        loaded.layers.len()
    );

    let claude_md = clawed_core::claude_md::load_claude_md(cwd);
    let md_lines = claude_md.lines().count();
    if md_lines > 0 {
        let _ = writeln!(out, "  ✓ CLAUDE.md reloaded ({} lines)", md_lines);
    } else {
        out.push_str("  \x1b[2m(no CLAUDE.md found)\x1b[0m\n");
    }

    let mem_files = clawed_core::memory::list_memory_files(cwd);
    let _ = writeln!(out, "  ✓ Memory: {} file(s)", mem_files.len());

    clawed_core::skills::clear_skill_cache();
    let skills = clawed_core::skills::get_skills(cwd);
    let _ = writeln!(out, "  ✓ Skills: {} loaded", skills.len());

    let hooks = &loaded.settings.hooks;
    let total_hooks = hooks.pre_tool_use.len()
        + hooks.post_tool_use.len()
        + hooks.stop.len()
        + hooks.session_start.len()
        + hooks.session_end.len()
        + hooks.user_prompt_submit.len();
    let _ = writeln!(out, "  ✓ Hooks: {} rule(s)", total_hooks);

    engine.update_system_prompt_context(&claude_md).await;

    let state = engine.state().read().await;
    let system_tokens = clawed_core::token_estimation::estimate_text_tokens(&claude_md);
    let msg_tokens = clawed_core::token_estimation::estimate_messages_tokens(&state.messages);
    let _ = write!(
        out,
        "\n\x1b[32m✓ Context reloaded ({} system + {} conversation tokens)\x1b[0m",
        system_tokens, msg_tokens
    );

    out
}

fn load_settings_json(settings_path: &Path) -> serde_json::Value {
    if settings_path.exists() {
        std::fs::read_to_string(settings_path)
            .ok()
            .and_then(|content| serde_json::from_str(&content).ok())
            .unwrap_or_else(|| serde_json::json!({}))
    } else {
        serde_json::json!({})
    }
}
