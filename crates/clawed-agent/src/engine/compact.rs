//! Compact — conversation compaction methods for QueryEngine.

use clawed_core::message::{ContentBlock, Message, UserMessage};
use uuid::Uuid;

use crate::compact::{compact_context_message, compact_conversation};
use crate::hooks::{HookDecision, HookEvent};

use super::QueryEngine;

impl QueryEngine {
    /// Compact the current conversation history.
    ///
    /// Fires PreCompact hooks (which can block or append custom instructions),
    /// calls Claude to summarise the conversation, replaces the history with a
    /// single system context message, then fires PostCompact hooks.
    ///
    /// Returns `Ok(summary)` on success, `Err` if the conversation is empty or
    /// the PreCompact hook blocked the operation.
    pub async fn compact(
        &self,
        trigger: &str,
        custom_instructions: Option<&str>,
    ) -> anyhow::Result<String> {
        let messages = {
            let s = self.state.read().await;
            s.messages.clone()
        };

        if messages.is_empty() {
            anyhow::bail!("Nothing to compact — conversation is empty.");
        }

        // ── PreCompact hook ──────────────────────────────────────────────────
        let mut extra_instructions = custom_instructions.map(|s| s.to_string());
        if self.hooks.has_hooks(HookEvent::PreCompact) {
            let ctx = self.hooks.compact_ctx(HookEvent::PreCompact, trigger, None);
            match self.hooks.run(HookEvent::PreCompact, ctx).await {
                HookDecision::Block { reason } => {
                    anyhow::bail!("Compaction blocked by PreCompact hook: {}", reason);
                }
                HookDecision::AppendContext { text } => {
                    extra_instructions = Some(match extra_instructions {
                        Some(existing) => format!("{}\n\n{}", existing, text),
                        None => text,
                    });
                }
                _ => {}
            }
        }

        // ── Call Claude for summary ──────────────────────────────────────────
        let model = { self.state.read().await.model.clone() };
        let summary = compact_conversation(
            &self.client,
            &messages,
            &model,
            extra_instructions.as_deref(),
        )
        .await?;

        // ── Replace conversation history ─────────────────────────────────────
        let context_msg = compact_context_message(&summary, None);
        {
            let mut s = self.state.write().await;
            s.messages = vec![Message::User(UserMessage {
                uuid: Uuid::new_v4().to_string(),
                content: vec![ContentBlock::Text { text: context_msg }],
            })];
            s.total_input_tokens = 0;
            s.total_output_tokens = 0;
        }

        // ── PostCompact hook ─────────────────────────────────────────────────
        if self.hooks.has_hooks(HookEvent::PostCompact) {
            let ctx =
                self.hooks
                    .compact_ctx(HookEvent::PostCompact, trigger, Some(summary.clone()));
            // Fire-and-forget
            let _ = self.hooks.run(HookEvent::PostCompact, ctx).await;
        }

        Ok(summary)
    }

    /// Check if auto-compact should trigger.
    ///
    /// Uses hybrid token counting: last API response's real token count plus
    /// rough estimation for messages added since.  Falls back to the simple
    /// fixed threshold for legacy callers that set a custom `compact_threshold`.
    pub async fn should_auto_compact(&self) -> bool {
        let s = self.state.read().await;
        let current_tokens =
            clawed_core::token_estimation::token_count_with_estimation(&s.messages);
        drop(s);

        let ac = self.auto_compact.lock().await;
        if self.context_window > 0 {
            ac.should_auto_compact(current_tokens, self.context_window)
        } else if self.compact_threshold > 0 {
            current_tokens >= self.compact_threshold
        } else {
            false
        }
    }

    /// Record a successful auto-compact (resets the circuit breaker).
    pub async fn record_compact_success(&self) {
        self.auto_compact.lock().await.record_success();
    }

    /// Record a failed auto-compact attempt (increments circuit breaker counter).
    pub async fn record_compact_failure(&self) {
        self.auto_compact.lock().await.record_failure();
    }

    /// Get the current context window usage as a percentage (0–100).
    /// Returns None if context window is unknown (0).
    pub async fn context_usage_percent(&self) -> Option<u8> {
        if self.context_window == 0 {
            return None;
        }
        let s = self.state.read().await;
        let current = clawed_core::token_estimation::token_count_with_estimation(&s.messages);
        let pct = (current as f64 / self.context_window as f64 * 100.0).min(100.0) as u8;
        Some(pct)
    }
}
