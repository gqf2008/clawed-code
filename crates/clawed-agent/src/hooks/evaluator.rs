//! Hook condition evaluator — local fast-path evaluation for `HookCondition`.
//!
//! Supports `Contains`, `Regex`, `All`, `Any`, and a keyword-based fast-path
//! for `Semantic` conditions (extracts nouns/verbs from the description and
//! checks overlap with the context). Full LLM evaluation is stubbed for
//! future integration when an API client is wired into the hook registry.

use regex::Regex;
use std::collections::HashSet;

use clawed_core::config::HookCondition;

use super::types::HookContext;

/// Cached compiled regexes for condition evaluation.
static REGEX_CACHE: std::sync::OnceLock<std::sync::Mutex<HashSet<String>>> =
    std::sync::OnceLock::new();

/// Evaluate a hook condition against the given context.
///
/// Returns `true` if the condition is satisfied (or if there is no condition).
/// `Semantic` conditions use a keyword fast-path: if the context and description
/// share enough keywords, returns `true`; otherwise returns `true` conservatively
/// (the shell hook itself can do finer-grained filtering).
pub fn evaluate_condition(condition: Option<&HookCondition>, ctx: &HookContext) -> bool {
    let Some(cond) = condition else {
        return true;
    };
    match cond {
        HookCondition::Contains { text } => context_contains(ctx, text),
        HookCondition::Regex { pattern } => context_matches_regex(ctx, pattern),
        HookCondition::All { conditions } => conditions
            .iter()
            .all(|c| evaluate_condition(Some(c), ctx)),
        HookCondition::Any { conditions } => conditions
            .iter()
            .any(|c| evaluate_condition(Some(c), ctx)),
        HookCondition::Semantic { description } => semantic_fast_path(ctx, description),
    }
}

// ── Local evaluators ─────────────────────────────────────────────────────────

fn context_contains(ctx: &HookContext, needle: &str) -> bool {
    let needle_lower = needle.to_lowercase();
    let haystack = context_text(ctx).to_lowercase();
    haystack.contains(&needle_lower)
}

fn context_matches_regex(ctx: &HookContext, pattern: &str) -> bool {
    match Regex::new(pattern) {
        Ok(re) => re.is_match(&context_text(ctx)),
        Err(e) => {
            tracing::warn!("Invalid hook regex '{}': {}", pattern, e);
            false
        }
    }
}

/// Fast-path for semantic conditions: extract keywords from the description
/// and check overlap with the context. If overlap is strong, return true.
/// Otherwise return true conservatively (let the shell hook refine).
fn semantic_fast_path(ctx: &HookContext, description: &str) -> bool {
    let ctx_words = extract_keywords(&context_text(ctx));
    let desc_words = extract_keywords(description);

    if ctx_words.is_empty() || desc_words.is_empty() {
        return true; // conservative
    }

    let overlap: usize = ctx_words.intersection(&desc_words).count();
    let union = ctx_words.union(&desc_words).count();

    if union == 0 {
        return true;
    }

    let score = overlap as f64 / union as f64;
    // If strong keyword overlap, definitely match
    // If weak overlap, still allow (shell hook can reject)
    tracing::debug!(
        "Semantic hook condition fast-path: score={:.2} (desc='{}')",
        score,
        description.chars().take(60).collect::<String>()
    );
    score >= 0.15 || true // conservative: always true for now, score logged
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Flatten the hook context into a single searchable string.
fn context_text(ctx: &HookContext) -> String {
    let mut parts = Vec::new();
    if let Some(ref prompt) = ctx.prompt {
        parts.push(prompt.clone());
    }
    if let Some(ref tool_name) = ctx.tool_name {
        parts.push(tool_name.clone());
    }
    if let Some(ref input) = ctx.tool_input {
        if let Ok(s) = serde_json::to_string(input) {
            parts.push(s);
        }
    }
    if let Some(ref output) = ctx.tool_output {
        parts.push(output.clone());
    }
    if let Some(ref trigger) = ctx.trigger {
        parts.push(trigger.clone());
    }
    if let Some(ref summary) = ctx.summary {
        parts.push(summary.clone());
    }
    if let Some(ref err) = ctx.error {
        parts.push(err.clone());
    }
    parts.join(" ")
}

/// Extract meaningful keywords from text: lowercase alphanumeric tokens >= 3 chars.
fn extract_keywords(text: &str) -> HashSet<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() >= 3)
        .map(|w| w.to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ctx_with_prompt(prompt: &str) -> HookContext {
        HookContext {
            event: "UserPromptSubmit".into(),
            tool_name: None,
            tool_input: None,
            tool_output: None,
            tool_error: None,
            error: None,
            prompt: Some(prompt.into()),
            trigger: None,
            summary: None,
            agent_id: None,
            cwd: "/tmp".into(),
            session_id: "s1".into(),
        }
    }

    fn ctx_with_tool(tool: &str, input: serde_json::Value) -> HookContext {
        HookContext {
            event: "PreToolUse".into(),
            tool_name: Some(tool.into()),
            tool_input: Some(input),
            tool_output: None,
            tool_error: None,
            error: None,
            prompt: None,
            trigger: None,
            summary: None,
            agent_id: None,
            cwd: "/tmp".into(),
            session_id: "s1".into(),
        }
    }

    #[test]
    fn no_condition_is_always_true() {
        let ctx = ctx_with_prompt("hello");
        assert!(evaluate_condition(None, &ctx));
    }

    #[test]
    fn contains_matches_case_insensitive() {
        let ctx = ctx_with_prompt("Please review the Rust code");
        let cond = HookCondition::Contains {
            text: "rust".into(),
        };
        assert!(evaluate_condition(Some(&cond), &ctx));
    }

    #[test]
    fn contains_no_match() {
        let ctx = ctx_with_prompt("Please review the Python code");
        let cond = HookCondition::Contains {
            text: "rust".into(),
        };
        assert!(!evaluate_condition(Some(&cond), &ctx));
    }

    #[test]
    fn regex_matches() {
        let ctx = ctx_with_prompt("File path is /src/main.rs");
        let cond = HookCondition::Regex {
            pattern: r"/src/.*\.rs".into(),
        };
        assert!(evaluate_condition(Some(&cond), &ctx));
    }

    #[test]
    fn regex_invalid_pattern_logs_and_returns_false() {
        let ctx = ctx_with_prompt("hello");
        let cond = HookCondition::Regex {
            pattern: "[invalid".into(),
        };
        assert!(!evaluate_condition(Some(&cond), &ctx));
    }

    #[test]
    fn all_requires_every_subcondition() {
        let ctx = ctx_with_prompt("Rust security audit");
        let cond = HookCondition::All {
            conditions: vec![
                HookCondition::Contains {
                    text: "rust".into(),
                },
                HookCondition::Contains {
                    text: "security".into(),
                },
            ],
        };
        assert!(evaluate_condition(Some(&cond), &ctx));
    }

    #[test]
    fn all_fails_if_one_missing() {
        let ctx = ctx_with_prompt("Rust code review");
        let cond = HookCondition::All {
            conditions: vec![
                HookCondition::Contains {
                    text: "rust".into(),
                },
                HookCondition::Contains {
                    text: "security".into(),
                },
            ],
        };
        assert!(!evaluate_condition(Some(&cond), &ctx));
    }

    #[test]
    fn any_matches_one() {
        let ctx = ctx_with_prompt("Python script");
        let cond = HookCondition::Any {
            conditions: vec![
                HookCondition::Contains {
                    text: "rust".into(),
                },
                HookCondition::Contains {
                    text: "python".into(),
                },
            ],
        };
        assert!(evaluate_condition(Some(&cond), &ctx));
    }

    #[test]
    fn any_fails_when_none_match() {
        let ctx = ctx_with_prompt("Go module");
        let cond = HookCondition::Any {
            conditions: vec![
                HookCondition::Contains {
                    text: "rust".into(),
                },
                HookCondition::Contains {
                    text: "python".into(),
                },
            ],
        };
        assert!(!evaluate_condition(Some(&cond), &ctx));
    }

    #[test]
    fn semantic_fast_path_conservative() {
        let ctx = ctx_with_prompt("We should audit the authentication module");
        let cond = HookCondition::Semantic {
            description: "security related tasks".into(),
        };
        // Conservative: returns true even with weak overlap
        assert!(evaluate_condition(Some(&cond), &ctx));
    }

    #[test]
    fn context_text_includes_tool_input() {
        let ctx = ctx_with_tool("Read", json!({"file_path": "/src/main.rs"}));
        let text = super::context_text(&ctx);
        assert!(text.contains("Read"));
        assert!(text.contains("file_path"));
    }
}
