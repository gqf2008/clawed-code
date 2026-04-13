//! Submit — user prompt entry points for QueryEngine.

use clawed_core::message::{ContentBlock, Message, UserMessage};
use clawed_core::tool::ToolContext;
use uuid::Uuid;

use crate::hooks::{HookDecision, HookEvent};
use crate::query::{query_stream, AgentEvent};

use super::QueryEngine;

impl QueryEngine {
    /// Submit a user message and get back a stream of AgentEvents.
    pub async fn submit(
        &self,
        user_prompt: impl Into<String>,
    ) -> std::pin::Pin<Box<dyn futures::Stream<Item = AgentEvent> + Send>> {
        // Reset the abort signal so a previous Esc/abort doesn't immediately
        // cancel this new submission.
        self.abort_signal.reset();

        let mut prompt_text: String = user_prompt.into();

        // ── Empty prompt validation ──────────────────────────────────────────
        if prompt_text.trim().is_empty() {
            let err_stream = async_stream::stream! {
                yield AgentEvent::Error("Prompt cannot be empty".to_string());
            };
            return Box::pin(err_stream);
        }

        // ── UserPromptSubmit hook ────────────────────────────────────────────
        if self.hooks.has_hooks(HookEvent::UserPromptSubmit) {
            let ctx = self.hooks.prompt_ctx(HookEvent::UserPromptSubmit, Some(prompt_text.clone()));
            match self.hooks.run(HookEvent::UserPromptSubmit, ctx).await {
                HookDecision::Block { reason } => {
                    let err_stream = async_stream::stream! {
                        yield AgentEvent::Error(format!("[UserPromptSubmit hook blocked]: {}", reason));
                    };
                    return Box::pin(err_stream);
                }
                HookDecision::AppendContext { text } => {
                    prompt_text = format!("{}\n\n{}", prompt_text, text);
                }
                _ => {}
            }
        }

        let (permission_mode, mut messages) = {
            let s = self.state.read().await;
            (s.permission_mode, s.messages.clone())
        };

        let user_msg = UserMessage {
            uuid: Uuid::new_v4().to_string(),
            content: vec![ContentBlock::Text { text: prompt_text }],
        };
        messages.push(Message::User(user_msg));

        let tools = self.tool_definitions(permission_mode);
        let tool_context = ToolContext {
            cwd: self.cwd.clone(),
            abort_signal: self.abort_signal.clone(),
            permission_mode,
            messages: Vec::new(),
        };

        query_stream(
            self.client.clone(),
            self.executor.clone(),
            self.state.clone(),
            tool_context,
            self.build_query_config(),
            messages,
            tools,
            self.hooks.clone(),
        )
    }

    /// Submit a user message with mixed content blocks (text + images).
    ///
    /// Use this when the user attaches images via `@path/to/image.png` syntax.
    /// The content blocks should be pre-built (text blocks for text, image blocks
    /// for attached images).
    pub async fn submit_with_content(
        &self,
        content: Vec<ContentBlock>,
    ) -> std::pin::Pin<Box<dyn futures::Stream<Item = AgentEvent> + Send>> {
        // Reset the abort signal so a previous Esc/abort doesn't immediately
        // cancel this new submission.
        self.abort_signal.reset();

        if content.is_empty() {
            let err_stream = async_stream::stream! {
                yield AgentEvent::Error("Prompt cannot be empty".to_string());
            };
            return Box::pin(err_stream);
        }

        // Run UserPromptSubmit hook with text from first text block
        let text_preview: String = content.iter().filter_map(|b| match b {
            ContentBlock::Text { text } => Some(text.clone()),
            _ => None,
        }).collect::<Vec<_>>().join("\n");

        let mut final_content = content;

        if self.hooks.has_hooks(HookEvent::UserPromptSubmit) {
            let ctx = self.hooks.prompt_ctx(HookEvent::UserPromptSubmit, Some(text_preview));
            match self.hooks.run(HookEvent::UserPromptSubmit, ctx).await {
                HookDecision::Block { reason } => {
                    let err_stream = async_stream::stream! {
                        yield AgentEvent::Error(format!("[UserPromptSubmit hook blocked]: {}", reason));
                    };
                    return Box::pin(err_stream);
                }
                HookDecision::AppendContext { text } => {
                    final_content.push(ContentBlock::Text { text });
                }
                _ => {}
            }
        }

        let (permission_mode, mut messages) = {
            let s = self.state.read().await;
            (s.permission_mode, s.messages.clone())
        };

        let user_msg = UserMessage {
            uuid: Uuid::new_v4().to_string(),
            content: final_content,
        };
        messages.push(Message::User(user_msg));

        let tools = self.tool_definitions(permission_mode);
        let tool_context = ToolContext {
            cwd: self.cwd.clone(),
            abort_signal: self.abort_signal.clone(),
            permission_mode,
            messages: Vec::new(),
        };

        query_stream(
            self.client.clone(),
            self.executor.clone(),
            self.state.clone(),
            tool_context,
            self.build_query_config(),
            messages,
            tools,
            self.hooks.clone(),
        )
    }
}
