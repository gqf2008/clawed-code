//! Skill runner — executes a skill by delivering content as a user message.

use clawed_agent::engine::QueryEngine;
use clawed_core::skills::SkillEntry;

use crate::input::{InputReader, InputResult};
use crate::output::print_stream;

pub(crate) fn find_skill<'a>(
    skills: &'a [SkillEntry],
    name: &str,
) -> Result<&'a SkillEntry, String> {
    skills
        .iter()
        .find(|skill| skill.name == name)
        .ok_or_else(|| format!("Unknown skill: {name}"))
}

/// Build the skill content string with argument substitution and base directory.
/// Returns `None` if the skill has no effective content to deliver.
pub(crate) fn build_skill_content(skill: &SkillEntry, skill_args: &str) -> Option<String> {
    let mut content = clawed_core::skills::substitute_arguments(skill, skill_args);
    if content.is_empty() {
        return None;
    }
    // Prepend base directory (matches TS getPromptForCommand behavior)
    if let Some(ref root) = skill.skill_root {
        content = format!("Base directory for this skill: {}\n\n{}", root.display(), content);
    }
    Some(content)
}

/// Build the full user message for a skill invocation, including XML metadata tags
/// and skill content. This is sent as the user message (not injected into the system prompt),
/// matching the reference implementation's `getMessagesForPromptSlashCommand` behavior.
pub(crate) fn build_skill_user_message(
    skill: &SkillEntry,
    skill_args: &str,
    user_prompt: &str,
) -> Option<String> {
    let skill_content = build_skill_content(skill, skill_args)?;
    let skill_name = &skill.name;

    // Build combined message: metadata tags + skill content + user prompt
    // The <command-name> tag tells the model (and future Skill tool) that
    // a skill is already loaded, preventing double-invocation.
    let mut parts = Vec::with_capacity(5);
    parts.push(format!("<command-message>{skill_name}</command-message>"));
    parts.push(format!("<command-name>{skill_name}</command-name>"));

    // Add command_permissions if skill restricts tools or overrides model
    if !skill.allowed_tools.is_empty() || skill.model.is_some() {
        parts.push(format!(
            "<command_permissions>\n  allowed_tools: {}\n  model: {}\n</command_permissions>",
            skill.allowed_tools.join(", "),
            skill.model.as_deref().unwrap_or("inherit")
        ));
    }

    parts.push(skill_content);
    parts.push(user_prompt.to_string());

    Some(parts.join("\n\n"))
}

/// Temporarily switch the engine model for a skill invocation.
/// Returns `(original_model, display_message)` if a switch occurred.
pub(crate) async fn switch_model_for_skill(
    engine: &QueryEngine,
    skill_model: &str,
) -> (Option<String>, Option<String>) {
    let resolved = clawed_core::model::resolve_model_string(skill_model);
    let current = { engine.state().read().await.model.clone() };
    if current != resolved {
        engine.state().write().await.model = resolved.clone();
        let display = format!(
            "Switching model to: {}",
            clawed_core::model::display_name_any(&resolved)
        );
        (Some(current), Some(display))
    } else {
        (None, None)
    }
}

/// Run a skill as a single-shot sub-agent conversation.
pub(crate) async fn run_skill(
    parent_engine: &QueryEngine,
    skills: &[SkillEntry],
    name: &str,
    prompt: &str,
    rl: &mut InputReader,
) {
    let skill = match find_skill(skills, name) {
        Ok(skill) => skill,
        Err(message) => {
            eprintln!("{message}");
            return;
        }
    };

    let (user_msg, skill_args): (String, &str) = if prompt.is_empty() {
        let prompt_text = format!("[{}]> ", skill.name);
        match rl.readline(&prompt_text) {
            Ok(InputResult::Line(ref p)) if !p.trim().is_empty() => {
                (format!("[{name}] {p}"), prompt)
            }
            // No input from user — use default message, but pass empty args
            // to avoid appending "ARGUMENTS: Execute the X skill" to skill content
            _ => (format!("Execute the {} skill", name), ""),
        }
    } else {
        (format!("[{name}] {prompt}"), prompt)
    };

    println!("\x1b[35m[Running skill: {}]\x1b[0m", skill.name);

    if !skill.allowed_tools.is_empty() {
        println!(
            "\x1b[33m  (Skill restricts tools to: {})\x1b[0m",
            skill.allowed_tools.join(", ")
        );
    }

    // Build the combined user message with skill content + XML tags
    let combined_msg = if let Some(msg) = build_skill_user_message(skill, skill_args, &user_msg) {
        msg
    } else {
        user_msg.clone()
    };

    // Set allowed tools for tool filtering
    if !skill.allowed_tools.is_empty() {
        parent_engine.set_skill_allowed_tools(skill.allowed_tools.clone());
    }

    // Temporarily switch model if skill specifies one
    let original_model = if let Some(ref skill_model) = skill.model {
        let (orig, msg) = switch_model_for_skill(parent_engine, skill_model).await;
        if let Some(msg) = msg {
            println!("\x1b[33m  ({})\x1b[0m", msg);
        }
        orig
    } else {
        None
    };

    let model = { parent_engine.state().read().await.model.clone() };
    let stream = parent_engine.submit(&combined_msg).await;
    let result = print_stream(stream, &model, Some(parent_engine.cost_tracker()), None).await;
    if let Err(e) = result {
        eprintln!("\x1b[31mSkill error: {}\x1b[0m", e);
    }

    // Restore original model
    if let Some(orig) = original_model {
        parent_engine.state().write().await.model = orig;
    }

    // Clear skill tool whitelist
    parent_engine.clear_skill_allowed_tools();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn sample_skill() -> SkillEntry {
        SkillEntry {
            name: "review".into(),
            description: "Code review skill".into(),
            system_prompt: "You are a reviewer".into(),
            allowed_tools: vec!["Read".into()],
            model: None,
            display_name: None,
            when_to_use: None,
            paths: vec![],
            argument_names: vec![],
            argument_hint: None,
            version: None,
            context: None,
            agent: None,
            effort: None,
            user_invocable: true,
            disable_model_invocation: false,
            skill_root: None,
        }
    }

    #[test]
    fn find_skill_returns_matching_entry() {
        let skills = vec![sample_skill()];
        let skill = find_skill(&skills, "review").unwrap();
        assert_eq!(skill.name, "review");
    }

    #[test]
    fn build_skill_content_returns_substituted_prompt() {
        let ctx = build_skill_content(&sample_skill(), "Check auth.rs");
        assert!(ctx.is_some());
        let text = ctx.unwrap();
        assert!(text.contains("You are a reviewer"));
    }

    #[test]
    fn build_skill_content_empty_skill_returns_none() {
        let mut skill = sample_skill();
        skill.system_prompt = String::new();
        assert!(build_skill_content(&skill, "Check auth.rs").is_none());
    }

    #[test]
    fn build_skill_content_prepends_base_directory() {
        let mut skill = sample_skill();
        skill.skill_root = Some(PathBuf::from("/home/user/.claude/skills/review"));
        let ctx = build_skill_content(&skill, "").unwrap();
        assert!(ctx.starts_with("Base directory for this skill:"));
        assert!(ctx.contains("You are a reviewer"));
    }

    #[test]
    fn build_skill_user_message_includes_xml_tags() {
        let msg = build_skill_user_message(&sample_skill(), "", "Do review").unwrap();
        assert!(msg.contains("<command-message>review</command-message>"));
        assert!(msg.contains("<command-name>review</command-name>"));
        assert!(msg.contains("You are a reviewer"));
        assert!(msg.contains("Do review"));
    }

    #[test]
    fn build_skill_user_message_includes_command_permissions() {
        let msg = build_skill_user_message(&sample_skill(), "", "Do review").unwrap();
        assert!(msg.contains("<command_permissions>"));
        assert!(msg.contains("allowed_tools"));
    }

    #[test]
    fn build_skill_user_message_empty_skill_returns_none() {
        let mut skill = sample_skill();
        skill.system_prompt = String::new();
        assert!(build_skill_user_message(&skill, "", "prompt").is_none());
    }
}
