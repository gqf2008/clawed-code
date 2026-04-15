//! Skill runner — executes a skill as a single-shot sub-agent conversation.

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

pub(crate) fn build_skill_prompt(skill: &SkillEntry, user_prompt: &str) -> String {
    if skill.system_prompt.is_empty() {
        user_prompt.to_string()
    } else {
        format!(
            "<skill_context>\n{}\n</skill_context>\n\n{}",
            skill.system_prompt, user_prompt
        )
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

    let user_prompt: String = if prompt.is_empty() {
        let prompt_text = format!("[{}]> ", skill.name);
        match rl.readline(&prompt_text) {
            Ok(InputResult::Line(p)) if !p.trim().is_empty() => p,
            _ => return,
        }
    } else {
        prompt.to_string()
    };

    println!("\x1b[35m[Running skill: {}]\x1b[0m", skill.name);
    let augmented = build_skill_prompt(skill, &user_prompt);

    if !skill.allowed_tools.is_empty() {
        println!(
            "\x1b[33m  (Skill restricts tools to: {})\x1b[0m",
            skill.allowed_tools.join(", ")
        );
    }

    let model = { parent_engine.state().read().await.model.clone() };
    let stream = parent_engine.submit(&augmented).await;
    if let Err(e) = print_stream(stream, &model, Some(parent_engine.cost_tracker()), None).await {
        eprintln!("\x1b[31mSkill error: {}\x1b[0m", e);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn build_skill_prompt_wraps_skill_context() {
        let prompt = build_skill_prompt(&sample_skill(), "Check auth.rs");
        assert!(prompt.contains("<skill_context>"));
        assert!(prompt.contains("You are a reviewer"));
        assert!(prompt.ends_with("Check auth.rs"));
    }
}
