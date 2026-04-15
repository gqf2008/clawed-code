//! /session, /undo, /export command handlers.

use std::fmt::Write as _;

use crate::theme;
use clawed_agent::engine::QueryEngine;

pub(crate) enum SessionCommandOutput {
    Message(String),
    Restored { title: String, message_count: usize },
}

/// Handle /session subcommands.
pub(crate) async fn handle_session_command(sub: &str, engine: &QueryEngine) {
    match handle_session_command_output(sub, engine).await {
        SessionCommandOutput::Message(message) => println!("{}", message),
        SessionCommandOutput::Restored {
            title,
            message_count,
        } => {
            println!("{}✓ Resumed session: {}\x1b[0m", theme::c_ok(), title);
            println!("  ({} messages restored)", message_count);
        }
    }
}

pub(crate) async fn handle_session_command_output(
    sub: &str,
    engine: &QueryEngine,
) -> SessionCommandOutput {
    let parts: Vec<&str> = sub.splitn(2, ' ').collect();
    match parts.first().copied().unwrap_or("") {
        "" | "list" => SessionCommandOutput::Message(list_sessions_text()),
        "save" => match engine.save_session().await {
            Ok(()) => SessionCommandOutput::Message(format!(
                "{}✓ Session saved ({})\x1b[0m",
                theme::c_ok(),
                &engine.session_id()[..8]
            )),
            Err(error) => SessionCommandOutput::Message(format!(
                "{}Failed to save session: {}\x1b[0m",
                theme::c_err(),
                error
            )),
        },
        "load" | "resume" => {
            restore_session_output(engine, parts.get(1).copied().unwrap_or("")).await
        }
        "delete" | "rm" => SessionCommandOutput::Message(delete_session_text(
            parts.get(1).copied().unwrap_or("").trim(),
        )),
        other => SessionCommandOutput::Message(format!(
            "Unknown session subcommand: '{}'. Use save, list, load <id>, or delete <id>.",
            other
        )),
    }
}

/// Undo the last assistant turn — remove trailing assistant+user message pair.
pub(crate) async fn handle_undo(engine: &QueryEngine) {
    let mut s = engine.state().write().await;
    let len = s.messages.len();
    if len < 2 {
        println!("Nothing to undo.");
        return;
    }

    let mut removed_assistant = false;
    while let Some(last) = s.messages.last() {
        let is_assistant = matches!(last, clawed_core::message::Message::Assistant(_));
        s.messages.pop();
        if is_assistant {
            removed_assistant = true;
            break;
        }
    }

    if removed_assistant {
        if let Some(last) = s.messages.last() {
            if matches!(last, clawed_core::message::Message::User(_)) {
                s.messages.pop();
            }
        }
    }

    if removed_assistant {
        let new_len = s.messages.len();
        println!(
            "{}✓ Undone (removed {} message(s), {} remaining)\x1b[0m",
            theme::c_ok(),
            len - new_len,
            new_len
        );
    } else {
        println!("Nothing to undo.");
    }
}

/// Export conversation to file.
pub(crate) async fn handle_export(engine: &QueryEngine, cwd: &std::path::Path, format: &str) {
    let state = engine.state().read().await;
    if state.messages.is_empty() {
        println!("No conversation to export.");
        return;
    }

    let timestamp = chrono::Utc::now().format("%Y%m%d_%H%M%S");

    match format {
        "json" => {
            let filename = format!("claude_export_{}.json", timestamp);
            let path = cwd.join(&filename);

            // Build per-model usage stats
            let model_stats: serde_json::Value = state
                .model_usage
                .iter()
                .map(|(model, usage)| {
                    (
                        model.clone(),
                        serde_json::json!({
                            "input_tokens": usage.input_tokens,
                            "output_tokens": usage.output_tokens,
                            "cache_read_tokens": usage.cache_read_tokens,
                            "cache_creation_tokens": usage.cache_creation_tokens,
                            "api_calls": usage.api_calls,
                            "cost_usd": usage.cost_usd,
                        }),
                    )
                })
                .collect::<serde_json::Map<_, _>>()
                .into();

            let export = serde_json::json!({
                "model": state.model,
                "turn_count": state.turn_count,
                "total_input_tokens": state.total_input_tokens,
                "total_output_tokens": state.total_output_tokens,
                "total_cost_usd": state.total_cost(),
                "total_errors": state.total_errors,
                "lines_added": state.total_lines_added,
                "lines_removed": state.total_lines_removed,
                "model_usage": model_stats,
                "messages": state.messages.iter().map(|m| match m {
                    clawed_core::message::Message::User(u) => serde_json::json!({
                        "role": "user",
                        "content": u.content.iter().filter_map(|b| match b {
                            clawed_core::message::ContentBlock::Text { text } => Some(serde_json::json!(text)),
                            _ => None,
                        }).collect::<Vec<_>>(),
                    }),
                    clawed_core::message::Message::Assistant(a) => serde_json::json!({
                        "role": "assistant",
                        "content": a.content.iter().filter_map(|b| match b {
                            clawed_core::message::ContentBlock::Text { text } => Some(serde_json::json!(text)),
                            clawed_core::message::ContentBlock::ToolUse { name, input, .. } =>
                                Some(serde_json::json!({"tool": name, "input": input})),
                            _ => None,
                        }).collect::<Vec<_>>(),
                    }),
                    clawed_core::message::Message::System(s) => serde_json::json!({
                        "role": "system",
                        "content": s.message,
                    }),
                }).collect::<Vec<_>>(),
            });
            let json = serde_json::to_string_pretty(&export).unwrap_or_else(|_| "{}".into());
            match std::fs::write(&path, json) {
                Ok(_) => println!("{}✓ Exported to {}\x1b[0m", theme::c_ok(), path.display()),
                Err(e) => eprintln!("{}Export failed: {}\x1b[0m", theme::c_err(), e),
            }
        }
        _ => {
            // Default: markdown export
            let filename = format!("claude_export_{}.md", timestamp);
            let path = cwd.join(&filename);
            let mut md = String::new();
            md.push_str("# Claude Conversation Export\n\n");
            md.push_str(&format!("Model: {}\n\n", state.model));

            for msg in &state.messages {
                match msg {
                    clawed_core::message::Message::User(u) => {
                        md.push_str("## User\n\n");
                        for block in &u.content {
                            if let clawed_core::message::ContentBlock::Text { text } = block {
                                md.push_str(text);
                                md.push_str("\n\n");
                            }
                        }
                    }
                    clawed_core::message::Message::Assistant(a) => {
                        md.push_str("## Assistant\n\n");
                        for block in &a.content {
                            match block {
                                clawed_core::message::ContentBlock::Text { text } => {
                                    md.push_str(text);
                                    md.push_str("\n\n");
                                }
                                clawed_core::message::ContentBlock::ToolUse { name, .. } => {
                                    md.push_str(&format!("*Used tool: {}*\n\n", name));
                                }
                                _ => {}
                            }
                        }
                    }
                    clawed_core::message::Message::System(_) => {}
                }
                md.push_str("---\n\n");
            }

            match std::fs::write(&path, &md) {
                Ok(_) => println!("{}✓ Exported to {}\x1b[0m", theme::c_ok(), path.display()),
                Err(e) => eprintln!("{}Export failed: {}\x1b[0m", theme::c_err(), e),
            }
        }
    }
}

/// Search conversation history for a query string (case-insensitive).
pub(crate) async fn handle_search(engine: &QueryEngine, query: &str) {
    println!("{}", handle_search_str(engine, query).await);
}

/// Browse conversation turns with pagination.
///
/// Shows 10 messages per page with role labels and truncated content.
pub(crate) async fn handle_history(engine: &QueryEngine, page: usize) {
    println!("{}", handle_history_str(engine, page).await);
}

pub(crate) async fn handle_search_str(engine: &QueryEngine, query: &str) -> String {
    if query.is_empty() {
        return "Usage: /search <query>  (prefix with r/ for regex, e.g. /search r/fn\\s+main)"
            .to_string();
    }

    let state = engine.state().read().await;
    if state.messages.is_empty() {
        return "No conversation to search.".to_string();
    }

    let is_regex = query.starts_with("r/");
    let re = if is_regex {
        let pattern = &query[2..];
        match regex::RegexBuilder::new(pattern)
            .case_insensitive(true)
            .build()
        {
            Ok(regex) => Some(regex),
            Err(error) => {
                return format!("{}Invalid regex: {}\x1b[0m", theme::c_err(), error);
            }
        }
    } else {
        None
    };

    let query_lower = query.to_lowercase();
    let mut hits: Vec<(usize, &str, String)> = Vec::new();

    for (idx, msg) in state.messages.iter().enumerate() {
        let (role, texts): (&str, Vec<&str>) = match msg {
            clawed_core::message::Message::User(user) => (
                "user",
                user.content
                    .iter()
                    .filter_map(|block| match block {
                        clawed_core::message::ContentBlock::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect(),
            ),
            clawed_core::message::Message::Assistant(assistant) => (
                "assistant",
                assistant
                    .content
                    .iter()
                    .filter_map(|block| match block {
                        clawed_core::message::ContentBlock::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect(),
            ),
            clawed_core::message::Message::System(system) => {
                ("system", vec![system.message.as_str()])
            }
        };

        for text in texts {
            let found = if let Some(ref regex) = re {
                regex
                    .find(text)
                    .map(|match_| (match_.start(), match_.end()))
            } else {
                let lower = text.to_lowercase();
                lower.find(&query_lower).map(|position| {
                    let byte_end = position + query_lower.len();
                    (position, byte_end)
                })
            };

            if let Some((byte_start, byte_end)) = found {
                let char_pos = text[..byte_start].chars().count();
                let match_char_len = text[byte_start..byte_end].chars().count();
                let total_chars = text.chars().count();
                let start_char = char_pos.saturating_sub(40);
                let end_char = (char_pos + match_char_len + 40).min(total_chars);
                let snippet: String = text
                    .chars()
                    .skip(start_char)
                    .take(end_char - start_char)
                    .collect();
                let prefix = if start_char > 0 { "…" } else { "" };
                let suffix = if end_char < total_chars { "…" } else { "" };
                hits.push((
                    idx,
                    role,
                    format!("{}{}{}", prefix, snippet.replace('\n', " "), suffix),
                ));
                break;
            }
        }
    }

    let display_query = if is_regex { &query[2..] } else { query };
    if hits.is_empty() {
        return format!("No matches for \"{}\".", display_query);
    }

    let mut out = format!(
        "\x1b[1m{} match(es) for \"{}\":\x1b[0m\n\n",
        hits.len(),
        display_query
    );
    for (idx, role, snippet) in &hits {
        let role_color = match *role {
            "user" => "\x1b[36m",
            "assistant" => "\x1b[33m",
            _ => "\x1b[2m",
        };
        let _ = writeln!(
            out,
            "  #{:<3} {}[{}]\x1b[0m {}",
            idx + 1,
            role_color,
            role,
            snippet
        );
    }
    out.trim_end().to_string()
}

pub(crate) async fn handle_history_str(engine: &QueryEngine, page: usize) -> String {
    let state = engine.state().read().await;
    if state.messages.is_empty() {
        return "No conversation history.".to_string();
    }

    let per_page = 10;
    let total = state.messages.len();
    let total_pages = total.div_ceil(per_page);
    let page = page.clamp(1, total_pages);
    let start = (page - 1) * per_page;
    let end = (start + per_page).min(total);

    let mut out = format!(
        "\x1b[1mConversation History\x1b[0m — page {}/{} ({} messages total)\n\n",
        page, total_pages, total
    );

    for idx in start..end {
        let msg = &state.messages[idx];
        let (role, role_color, preview) = match msg {
            clawed_core::message::Message::User(user) => {
                let text = user
                    .content
                    .iter()
                    .find_map(|block| match block {
                        clawed_core::message::ContentBlock::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .unwrap_or("");
                ("user", "\x1b[36m", truncate_preview(text, 80))
            }
            clawed_core::message::Message::Assistant(assistant) => {
                let text_blocks: Vec<&str> = assistant
                    .content
                    .iter()
                    .filter_map(|block| match block {
                        clawed_core::message::ContentBlock::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect();
                let tool_count = assistant
                    .content
                    .iter()
                    .filter(|block| {
                        matches!(block, clawed_core::message::ContentBlock::ToolUse { .. })
                    })
                    .count();
                let preview = if text_blocks.is_empty() && tool_count > 0 {
                    format!("[{} tool call(s)]", tool_count)
                } else {
                    let combined = text_blocks.join(" ");
                    let suffix = if tool_count > 0 {
                        format!(" [+{} tool(s)]", tool_count)
                    } else {
                        String::new()
                    };
                    format!("{}{}", truncate_preview(&combined, 70), suffix)
                };
                ("assistant", "\x1b[33m", preview)
            }
            clawed_core::message::Message::System(system) => {
                ("system", "\x1b[2m", truncate_preview(&system.message, 80))
            }
        };

        let _ = writeln!(
            out,
            "  \x1b[2m#{:<3}\x1b[0m {}[{}]\x1b[0m {}",
            idx + 1,
            role_color,
            role,
            preview
        );
    }

    if total_pages > 1 {
        let _ = write!(
            out,
            "\n\x1b[2mUse /history {} for next page\x1b[0m",
            if page < total_pages { page + 1 } else { 1 }
        );
    }

    out.trim_end().to_string()
}

/// Truncate a string to `max_chars` and add ellipsis if needed.
fn truncate_preview(text: &str, max_chars: usize) -> String {
    let clean = text.replace('\n', " ").replace('\r', "");
    let clean = clean.trim();
    if clean.chars().count() <= max_chars {
        clean.to_string()
    } else {
        let truncated: String = clean.chars().take(max_chars).collect();
        format!("{}…", truncated)
    }
}

fn list_sessions_text() -> String {
    let sessions = clawed_core::session::list_sessions();
    if sessions.is_empty() {
        return "No saved sessions.".to_string();
    }

    let mut out = String::from("Saved sessions:\n");
    for session in &sessions {
        let age = clawed_core::session::format_age(&session.updated_at);
        let _ = writeln!(
            out,
            "  \x1b[36m{:.8}\x1b[0m  {:<50} ({} msgs, {} turns, {})",
            session.id, session.title, session.message_count, session.turn_count, age
        );
    }
    out.trim_end().to_string()
}

async fn restore_session_output(engine: &QueryEngine, query: &str) -> SessionCommandOutput {
    let query = query.trim();
    let sessions = clawed_core::session::list_sessions();

    if query.is_empty() {
        let Some(latest) = sessions.first() else {
            return SessionCommandOutput::Message(
                "No sessions to resume. Use /session list first.".to_string(),
            );
        };

        return match engine.restore_session(&latest.id).await {
            Ok(title) => SessionCommandOutput::Restored {
                title,
                message_count: latest.message_count,
            },
            Err(error) => SessionCommandOutput::Message(format!(
                "{}Failed to resume: {}\x1b[0m",
                theme::c_err(),
                error
            )),
        };
    }

    if let Some(meta) = sessions
        .iter()
        .find(|session| session.id.starts_with(query))
    {
        return match engine.restore_session(&meta.id).await {
            Ok(title) => SessionCommandOutput::Restored {
                title,
                message_count: meta.message_count,
            },
            Err(error) => SessionCommandOutput::Message(format!(
                "{}Failed to resume: {}\x1b[0m",
                theme::c_err(),
                error
            )),
        };
    }

    let matches: Vec<_> = sessions
        .iter()
        .filter(|session| session_matches_query(session, query))
        .collect();

    match matches.len() {
        0 => SessionCommandOutput::Message(format!(
            "No session found matching '{}'. Use /session list.",
            query
        )),
        1 => {
            let meta = matches[0];
            match engine.restore_session(&meta.id).await {
                Ok(title) => SessionCommandOutput::Restored {
                    title,
                    message_count: meta.message_count,
                },
                Err(error) => SessionCommandOutput::Message(format!(
                    "{}Failed to resume: {}\x1b[0m",
                    theme::c_err(),
                    error
                )),
            }
        }
        count => {
            let mut out = format!("Found {} sessions matching '{}':\n", count, query);
            for session in &matches {
                let age = clawed_core::session::format_age(&session.updated_at);
                let _ = writeln!(
                    out,
                    "  \x1b[36m{:.8}\x1b[0m  {:<50} ({} msgs, {})",
                    session.id, session.title, session.message_count, age
                );
            }
            out.push_str("\nUse /session load <id> with a more specific prefix.");
            SessionCommandOutput::Message(out)
        }
    }
}

fn delete_session_text(id: &str) -> String {
    if id.is_empty() {
        return "Usage: /session delete <id>".to_string();
    }

    let sessions = clawed_core::session::list_sessions();
    let found = sessions.iter().find(|session| session.id.starts_with(id));
    match found {
        Some(meta) => match clawed_core::session::delete_session(&meta.id) {
            Ok(()) => format!(
                "{}✓ Deleted session {:.8} ({})\x1b[0m",
                theme::c_ok(),
                meta.id,
                meta.title
            ),
            Err(error) => format!("{}Failed to delete: {}\x1b[0m", theme::c_err(), error),
        },
        None => format!("No session found matching '{}'. Use /session list.", id),
    }
}

/// Fuzzy match a query against session metadata (case-insensitive substring).
fn session_matches_query(meta: &clawed_core::session::SessionMeta, query: &str) -> bool {
    let q = query.to_lowercase();
    meta.title.to_lowercase().contains(&q)
        || meta
            .custom_title
            .as_deref()
            .unwrap_or("")
            .to_lowercase()
            .contains(&q)
        || meta
            .summary
            .as_deref()
            .unwrap_or("")
            .to_lowercase()
            .contains(&q)
        || meta
            .last_prompt
            .as_deref()
            .unwrap_or("")
            .to_lowercase()
            .contains(&q)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_truncate_preview_short() {
        assert_eq!(truncate_preview("hello", 10), "hello");
    }

    #[test]
    fn test_truncate_preview_exact() {
        assert_eq!(truncate_preview("12345", 5), "12345");
    }

    #[test]
    fn test_truncate_preview_long() {
        let result = truncate_preview("hello world this is a long string", 10);
        assert_eq!(result, "hello worl…");
    }

    #[test]
    fn test_truncate_preview_newlines() {
        assert_eq!(
            truncate_preview("line1\nline2\nline3", 20),
            "line1 line2 line3"
        );
    }

    #[test]
    fn test_truncate_preview_whitespace_trim() {
        assert_eq!(truncate_preview("  hello  ", 10), "hello");
    }

    fn make_meta(
        title: &str,
        custom_title: Option<&str>,
        summary: Option<&str>,
        last_prompt: Option<&str>,
    ) -> clawed_core::session::SessionMeta {
        clawed_core::session::SessionMeta {
            id: "test-id-123".into(),
            title: title.into(),
            model: "claude-sonnet".into(),
            cwd: "/tmp".into(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            turn_count: 5,
            message_count: 10,
            total_cost_usd: 0.01,
            git_branch: None,
            custom_title: custom_title.map(|s| s.into()),
            summary: summary.map(|s| s.into()),
            last_prompt: last_prompt.map(|s| s.into()),
        }
    }

    #[test]
    fn test_session_fuzzy_match_title() {
        let meta = make_meta("Fix authentication bug", None, None, None);
        assert!(session_matches_query(&meta, "auth"));
        assert!(session_matches_query(&meta, "AUTH")); // case-insensitive
        assert!(!session_matches_query(&meta, "deploy"));
    }

    #[test]
    fn test_session_fuzzy_match_custom_title() {
        let meta = make_meta("Some title", Some("My deploy script"), None, None);
        assert!(session_matches_query(&meta, "deploy"));
        assert!(!session_matches_query(&meta, "migration"));
    }

    #[test]
    fn test_session_fuzzy_match_summary() {
        let meta = make_meta("Title", None, Some("Implemented database migration"), None);
        assert!(session_matches_query(&meta, "migration"));
    }

    #[test]
    fn test_session_fuzzy_match_last_prompt() {
        let meta = make_meta("Title", None, None, Some("add unit tests for parser"));
        assert!(session_matches_query(&meta, "parser"));
        assert!(session_matches_query(&meta, "unit test"));
    }

    #[test]
    fn test_session_fuzzy_no_match() {
        let meta = make_meta("Title", None, None, None);
        assert!(!session_matches_query(&meta, "nonexistent"));
    }
}
