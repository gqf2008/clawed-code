use async_trait::async_trait;
use clawed_core::tool::{Tool, ToolContext, ToolResult};
use serde_json::{json, Value};

/// PushNotification — send a desktop notification to the user's terminal.
///
/// If Remote Control is connected, also pushes to their phone.
///
/// Mirrors the TS `PushNotificationTool`.
pub struct PushNotificationTool;

#[async_trait]
impl Tool for PushNotificationTool {
    fn name(&self) -> &'static str {
        "PushNotification"
    }

    fn description(&self) -> &'static str {
        "This tool sends a desktop notification in the user's terminal. \
         If Remote Control is connected, it also pushes to their phone. \
         Either way, it pulls their attention from whatever they're doing — \
         a meeting, another task, dinner — to this session. That's the cost. \
         The benefit is they learn something now that they'd want to know now: \
         a long task finished while they were away, a build is ready, you've hit \
         something that needs their decision before you can continue.\n\n\
         Because a notification they didn't need is annoying in a way that accumulates, \
         err toward not sending one. Don't notify for routine progress, or to announce \
         you've answered something they asked seconds ago and are clearly still watching, \
         or when a quick task completes. Notify when there's a real chance they've walked \
         away and there's something worth coming back for — or when they've explicitly \
         asked you to notify them.\n\n\
         Keep the message under 200 characters, one line, no markdown. \
         Lead with what they'd act on — \"build failed: 2 auth tests\" tells them more \
         than \"task done\" and more than a status dump."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "message": {
                    "type": "string",
                    "description": "The notification body. Keep it under 200 characters; mobile OSes truncate."
                },
                "status": {
                    "type": "string",
                    "enum": ["proactive"],
                    "description": "Always 'proactive' for notifications."
                }
            },
            "required": ["message", "status"]
        })
    }

    fn is_read_only(&self) -> bool {
        true
    }

    fn is_concurrency_safe(&self) -> bool {
        true
    }

    async fn call(&self, input: Value, _context: &ToolContext) -> anyhow::Result<ToolResult> {
        let message = input["message"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'message'"))?;

        // Print to stderr so CLI/TUI can pick it up as a notification
        eprintln!("\n\x1b[33m🔔 Notification: {message}\x1b[0m");

        Ok(ToolResult::text(format!("Notification sent: {message}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clawed_core::permissions::PermissionMode;
    use clawed_core::tool::ToolContext;

    fn test_context() -> ToolContext {
        ToolContext {
            cwd: std::env::temp_dir(),
            abort_signal: Default::default(),
            permission_mode: PermissionMode::Default,
            messages: vec![],
            output_line: None,
        }
    }

    #[tokio::test]
    async fn call_basic_notification() {
        let tool = PushNotificationTool;
        let input = json!({ "message": "Build ready!", "status": "proactive" });
        let result = tool.call(input, &test_context()).await.unwrap();
        assert!(!result.is_error);
        let text = result.to_text();
        assert!(text.contains("Build ready!"));
        assert!(text.contains("Notification sent"));
    }

    #[tokio::test]
    async fn call_missing_message_fails() {
        let tool = PushNotificationTool;
        let input = json!({ "status": "proactive" });
        assert!(tool.call(input, &test_context()).await.is_err());
    }

    #[test]
    fn tool_metadata() {
        let tool = PushNotificationTool;
        assert_eq!(tool.name(), "PushNotification");
        assert!(tool.is_read_only());
        assert!(tool.is_concurrency_safe());
        assert_eq!(tool.category(), clawed_core::tool::ToolCategory::Session);
    }
}
