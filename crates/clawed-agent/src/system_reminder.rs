//! Runtime system-reminder injection into tool results.
//!
//! Claude Code injects `<system-reminder>` tags into tool-result content at
//! runtime to convey contextual events to the model: token usage, hook
//! feedback, file modifications, plan-mode transitions, etc.
//!
//! The model is told these "bear no direct relation to the specific tool
//! results or user messages in which they appear" — they are ambient context.

use clawed_core::message::{ContentBlock, ToolResultContent, Usage};

/// A system-reminder event to inject into the next tool result.
#[derive(Debug, Clone)]
pub enum SystemReminder {
    /// Token usage stats after an API turn.
    TokenUsage {
        input_tokens: u64,
        output_tokens: u64,
        context_window: u64,
    },
    /// Hook execution result (success or feedback).
    HookResult {
        success: bool,
        feedback: Option<String>,
    },
    /// A file was modified externally (by user, linter, or IDE).
    FileModified { path: String },
    /// Plan mode state change.
    PlanModeChange { active: bool },
    /// Session is being continued from a prior conversation.
    SessionContinuation,
    /// Compact file reference — file was read before compaction.
    CompactFileReference { path: String },
    /// Custom reminder with arbitrary content.
    Custom(String),
}

impl SystemReminder {
    /// Format this reminder as a `<system-reminder>` XML tag.
    pub fn to_xml(&self) -> String {
        match self {
            Self::TokenUsage { input_tokens, output_tokens, context_window } => {
                let used = input_tokens + output_tokens;
                let remaining = context_window.saturating_sub(used);
                format!(
                    "<system-reminder>\nToken usage: {used}/{context_window} tokens used; {remaining} remaining\n</system-reminder>"
                )
            }
            Self::HookResult { success: true, feedback: None } => {
                "<system-reminder>\nhook success: Success\n</system-reminder>".to_string()
            }
            Self::HookResult { success: true, feedback: Some(fb) } => {
                format!("<system-reminder>\nhook success: {fb}\n</system-reminder>")
            }
            Self::HookResult { success: false, feedback } => {
                let msg = feedback.as_deref().unwrap_or("blocked");
                format!("<system-reminder>\nhook blocked: {msg}\n</system-reminder>")
            }
            Self::FileModified { path } => {
                format!("<system-reminder>\nFile modified by user or linter: {path}\n</system-reminder>")
            }
            Self::PlanModeChange { active: true } => {
                "<system-reminder>\nPlan mode is now active. Use read-only tools to explore and plan. When ready, call ExitPlanMode.\n</system-reminder>".to_string()
            }
            Self::PlanModeChange { active: false } => {
                "<system-reminder>\nPlan mode has been exited. You may now use write tools.\n</system-reminder>".to_string()
            }
            Self::SessionContinuation => {
                "<system-reminder>\nThis is a continuation of a previous session. The conversation history has been compacted.\n</system-reminder>".to_string()
            }
            Self::CompactFileReference { path } => {
                format!("<system-reminder>\nFile previously read but contents omitted after compaction: {path}\n</system-reminder>")
            }
            Self::Custom(content) => {
                format!("<system-reminder>\n{content}\n</system-reminder>")
            }
        }
    }
}

/// Collector for pending system reminders during a turn.
///
/// Reminders are accumulated during turn processing and injected into
/// the first tool result of the next tool-use round (or appended as a
/// synthetic tool result if no tools were used).
#[derive(Debug, Default)]
pub struct ReminderCollector {
    pending: Vec<SystemReminder>,
}

impl ReminderCollector {
    /// Add a reminder to be injected into the next tool result.
    pub fn push(&mut self, reminder: SystemReminder) {
        self.pending.push(reminder);
    }

    /// Add a token-usage reminder from the latest API response.
    pub fn push_token_usage(&mut self, usage: &Usage, context_window: u64) {
        self.pending.push(SystemReminder::TokenUsage {
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            context_window,
        });
    }

    /// Check if there are pending reminders.
    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    /// Drain all pending reminders as a single XML block.
    fn drain_as_xml(&mut self) -> Option<String> {
        if self.pending.is_empty() {
            return None;
        }
        let xml: String = self
            .pending
            .drain(..)
            .map(|r| r.to_xml())
            .collect::<Vec<_>>()
            .join("\n");
        Some(xml)
    }

    /// Inject pending reminders into a list of tool-result content blocks.
    ///
    /// Appends the reminder XML to the text of the first `ToolResult` block.
    /// If no tool results exist, creates a synthetic one.
    pub fn inject_into(&mut self, results: &mut Vec<ContentBlock>) {
        let xml = match self.drain_as_xml() {
            Some(x) => x,
            None => return,
        };

        // Find the first ToolResult and append the reminder to its text.
        for block in results.iter_mut() {
            if let ContentBlock::ToolResult { content, .. } = block {
                if let Some(ToolResultContent::Text { text }) = content.first_mut() {
                    text.push_str("\n\n");
                    text.push_str(&xml);
                    return;
                }
            }
        }

        // No tool result found — create a synthetic one so reminders aren't lost.
        results.push(ContentBlock::ToolResult {
            tool_use_id: String::new(),
            content: vec![ToolResultContent::Text { text: xml }],
            is_error: false,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_usage_formats_correctly() {
        let r = SystemReminder::TokenUsage {
            input_tokens: 50000,
            output_tokens: 500,
            context_window: 200_000,
        };
        let xml = r.to_xml();
        assert!(xml.contains("<system-reminder>"));
        assert!(xml.contains("50500/200000"));
        assert!(xml.contains("149500 remaining"));
    }

    #[test]
    fn hook_success_formats() {
        let r = SystemReminder::HookResult {
            success: true,
            feedback: None,
        };
        assert!(r.to_xml().contains("hook success: Success"));
    }

    #[test]
    fn hook_blocked_formats() {
        let r = SystemReminder::HookResult {
            success: false,
            feedback: Some("dangerous command".into()),
        };
        assert!(r.to_xml().contains("hook blocked: dangerous command"));
    }

    #[test]
    fn file_modified_formats() {
        let r = SystemReminder::FileModified {
            path: "src/main.rs".into(),
        };
        assert!(r.to_xml().contains("src/main.rs"));
    }

    #[test]
    fn plan_mode_change_formats() {
        let on = SystemReminder::PlanModeChange { active: true };
        assert!(on.to_xml().contains("Plan mode is now active"));
        let off = SystemReminder::PlanModeChange { active: false };
        assert!(off.to_xml().contains("Plan mode has been exited"));
    }

    #[test]
    fn collector_injects_into_first_tool_result() {
        let mut collector = ReminderCollector::default();
        collector.push(SystemReminder::Custom("test reminder".into()));

        let mut results = vec![
            ContentBlock::ToolResult {
                tool_use_id: "t1".into(),
                content: vec![ToolResultContent::Text {
                    text: "original output".into(),
                }],
                is_error: false,
            },
            ContentBlock::ToolResult {
                tool_use_id: "t2".into(),
                content: vec![ToolResultContent::Text {
                    text: "other output".into(),
                }],
                is_error: false,
            },
        ];

        collector.inject_into(&mut results);

        // First result should have the reminder appended
        match &results[0] {
            ContentBlock::ToolResult { content, .. } => {
                let text = &content[0];
                match text {
                    ToolResultContent::Text { text } => {
                        assert!(text.contains("original output"));
                        assert!(text.contains("test reminder"));
                    }
                    _ => panic!("expected text"),
                }
            }
            _ => panic!("expected tool result"),
        }

        // Second result should be unchanged
        match &results[1] {
            ContentBlock::ToolResult { content, .. } => match &content[0] {
                ToolResultContent::Text { text } => {
                    assert_eq!(text, "other output");
                    assert!(!text.contains("test reminder"));
                }
                _ => panic!("expected text"),
            },
            _ => panic!("expected tool result"),
        }

        // Collector should be empty after injection
        assert!(collector.is_empty());
    }

    #[test]
    fn collector_drains_multiple_reminders() {
        let mut collector = ReminderCollector::default();
        collector.push(SystemReminder::Custom("first".into()));
        collector.push(SystemReminder::Custom("second".into()));

        let mut results = vec![ContentBlock::ToolResult {
            tool_use_id: "t1".into(),
            content: vec![ToolResultContent::Text {
                text: "output".into(),
            }],
            is_error: false,
        }];

        collector.inject_into(&mut results);

        match &results[0] {
            ContentBlock::ToolResult { content, .. } => match &content[0] {
                ToolResultContent::Text { text } => {
                    assert!(text.contains("first"));
                    assert!(text.contains("second"));
                }
                _ => panic!("expected text"),
            },
            _ => panic!("expected tool result"),
        }
    }

    #[test]
    fn empty_collector_is_noop() {
        let mut collector = ReminderCollector::default();
        let mut results = vec![ContentBlock::ToolResult {
            tool_use_id: "t1".into(),
            content: vec![ToolResultContent::Text {
                text: "output".into(),
            }],
            is_error: false,
        }];
        collector.inject_into(&mut results);
        match &results[0] {
            ContentBlock::ToolResult { content, .. } => match &content[0] {
                ToolResultContent::Text { text } => assert_eq!(text, "output"),
                _ => panic!("expected text"),
            },
            _ => panic!("expected tool result"),
        }
    }
}
