use clawed_agent::engine::QueryEngine;
use clawed_agent::task_runner::run_task_silent;
use serde::Deserialize;

const GOAL_REASON_PREVIEW_CHARS: usize = 200;
const GOAL_NO_TOOLS_SENTINEL: &str = "__goal_no_tools__";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum GoalStatus {
    Active,
    Paused,
    Completed,
    Blocked,
}

impl GoalStatus {
    pub(crate) fn label(&self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Paused => "paused",
            Self::Completed => "completed",
            Self::Blocked => "blocked",
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct GoalState {
    pub(crate) objective: String,
    pub(crate) status: GoalStatus,
    pub(crate) iteration: u32,
    pub(crate) next_prompt: Option<String>,
    pub(crate) last_reason: Option<String>,
}

impl GoalState {
    pub(crate) fn new(objective: String) -> Self {
        Self {
            objective,
            status: GoalStatus::Active,
            iteration: 0,
            next_prompt: None,
            last_reason: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum GoalDecisionAction {
    Continue,
    Completed,
    Blocked,
}

#[derive(Debug, Clone)]
pub(crate) struct GoalDecision {
    pub(crate) action: GoalDecisionAction,
    pub(crate) reason: String,
    pub(crate) next_prompt: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GoalJudgeResponse {
    action: String,
    reason: String,
    next_prompt: Option<String>,
}

pub(crate) fn goal_status_message(goal: &GoalState) -> String {
    let mut message = format!(
        "Goal [{}] iteration {}: {}",
        goal.status.label(),
        goal.iteration,
        goal.objective
    );
    if let Some(reason) = &goal.last_reason {
        message.push_str(&format!(
            "\nLast reason: {}",
            clawed_core::text_util::truncate_chars(reason, GOAL_REASON_PREVIEW_CHARS, "…")
        ));
    }
    if let Some(next_prompt) = &goal.next_prompt {
        message.push_str(&format!(
            "\nNext step: {}",
            clawed_core::text_util::truncate_chars(next_prompt, GOAL_REASON_PREVIEW_CHARS, "…")
        ));
    }
    message
}

pub(crate) fn prepare_goal_iteration(goal: &mut GoalState) -> String {
    goal.iteration += 1;
    if let Some(next_prompt) = &goal.next_prompt {
        format!(
            "Active goal:\n{}\n\nContinue this goal. Focus specifically on:\n{}\n\nMake concrete progress and stop when this iteration either completes the goal or reaches a real blocker.",
            goal.objective, next_prompt
        )
    } else {
        format!(
            "Active goal:\n{}\n\nWork on the next concrete step toward fully completing this goal. Make real progress, use tools when needed, and stop only when you have either completed the goal or reached a real blocker.",
            goal.objective
        )
    }
}

fn build_goal_judge_prompt(goal: &GoalState) -> String {
    format!(
        "You are deciding whether an active coding goal should continue.\n\nGoal:\n{}\n\nIteration: {}\nThe full conversation history contains the latest work result.\n\nReturn JSON only with this exact shape:\n{{\"action\":\"continue|completed|blocked\",\"reason\":\"short explanation\",\"next_prompt\":\"next concrete step or empty string\"}}\n\nRules:\n- Use \"completed\" only if the goal is truly done.\n- Use \"continue\" only if meaningful work remains and you can name the next concrete step.\n- Use \"blocked\" if progress now requires user input, credentials, or an external dependency.\n- Do not call tools.",
        goal.objective, goal.iteration
    )
}

fn extract_goal_judge_json(raw: &str) -> String {
    let trimmed = raw.trim();
    let without_fence = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .unwrap_or(trimmed)
        .trim();
    let without_end_fence = without_fence.strip_suffix("```").unwrap_or(without_fence).trim();
    if let (Some(start), Some(end)) = (without_end_fence.find('{'), without_end_fence.rfind('}')) {
        without_end_fence[start..=end].to_string()
    } else {
        without_end_fence.to_string()
    }
}

fn parse_goal_judge_response(raw: &str) -> anyhow::Result<GoalDecision> {
    let json = extract_goal_judge_json(raw);
    let parsed: GoalJudgeResponse =
        serde_json::from_str(&json).map_err(|e| anyhow::anyhow!("invalid goal-judge JSON: {}", e))?;

    let action = match parsed.action.trim().to_ascii_lowercase().as_str() {
        "continue" => GoalDecisionAction::Continue,
        "completed" | "complete" | "done" => GoalDecisionAction::Completed,
        "blocked" => GoalDecisionAction::Blocked,
        other => anyhow::bail!("unknown goal-judge action '{}'", other),
    };

    Ok(GoalDecision {
        action,
        reason: parsed.reason,
        next_prompt: parsed
            .next_prompt
            .map(|next| next.trim().to_string())
            .filter(|next| !next.is_empty()),
    })
}

pub(crate) async fn judge_goal_progress(
    engine: &QueryEngine,
    goal: &GoalState,
) -> anyhow::Result<GoalDecision> {
    engine.set_skill_allowed_tools(vec![GOAL_NO_TOOLS_SENTINEL.to_string()]);
    let judge_result = run_task_silent(engine, &build_goal_judge_prompt(goal)).await;
    engine.clear_skill_allowed_tools();

    if !judge_result.success() {
        anyhow::bail!("goal judge stopped with {}", judge_result.reason);
    }

    parse_goal_judge_response(&judge_result.output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn goal_judge_json_handles_code_fences() {
        let parsed = parse_goal_judge_response(
            "```json\n{\"action\":\"continue\",\"reason\":\"keep going\",\"next_prompt\":\"run tests\"}\n```",
        )
        .unwrap();
        assert_eq!(parsed.action, GoalDecisionAction::Continue);
        assert_eq!(parsed.reason, "keep going");
        assert_eq!(parsed.next_prompt.as_deref(), Some("run tests"));
    }

    #[test]
    fn goal_status_message_includes_reason_and_next_step() {
        let goal = GoalState {
            objective: "finish the feature".into(),
            status: GoalStatus::Paused,
            iteration: 2,
            next_prompt: Some("resume from tests".into()),
            last_reason: Some("Interrupted by user".into()),
        };

        let message = goal_status_message(&goal);
        assert!(message.contains("Goal [paused] iteration 2"));
        assert!(message.contains("Interrupted by user"));
        assert!(message.contains("resume from tests"));
    }
}
