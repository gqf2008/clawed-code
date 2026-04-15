//! Session persistence operations for QueryEngine.

use super::QueryEngine;

impl QueryEngine {
    /// Save the current session to disk.
    pub async fn save_session(&self) -> anyhow::Result<()> {
        use clawed_core::session::*;
        let s = self.state.read().await;
        let snapshot = SessionSnapshot {
            id: self.session_id.clone(),
            title: title_from_messages(&s.messages),
            model: s.model.clone(),
            cwd: self.cwd.to_string_lossy().to_string(),
            created_at: self.created_at,
            updated_at: chrono::Utc::now(),
            turn_count: s.turn_count,
            input_tokens: s.total_input_tokens,
            output_tokens: s.total_output_tokens,
            model_usage: s
                .model_usage
                .iter()
                .map(|(k, v)| {
                    (
                        k.clone(),
                        SessionModelUsage {
                            input_tokens: v.input_tokens,
                            output_tokens: v.output_tokens,
                            cache_read_tokens: v.cache_read_tokens,
                            cache_creation_tokens: v.cache_creation_tokens,
                            api_calls: v.api_calls,
                            cost_usd: v.cost_usd,
                        },
                    )
                })
                .collect(),
            total_cost_usd: s.model_usage.values().map(|u| u.cost_usd).sum(),
            messages: s.messages.clone(),
            git_branch: None,
            custom_title: None,
            ai_title: None,
            summary: None,
            last_prompt: None,
        };
        save_session(&snapshot)
    }

    /// Rename the current session (sets custom_title and re-saves).
    pub async fn rename_session(&self, name: &str) -> anyhow::Result<()> {
        use clawed_core::session::*;
        let s = self.state.read().await;
        let snapshot = SessionSnapshot {
            id: self.session_id.clone(),
            title: name.to_string(),
            model: s.model.clone(),
            cwd: self.cwd.to_string_lossy().to_string(),
            created_at: self.created_at,
            updated_at: chrono::Utc::now(),
            turn_count: s.turn_count,
            input_tokens: s.total_input_tokens,
            output_tokens: s.total_output_tokens,
            model_usage: s
                .model_usage
                .iter()
                .map(|(k, v)| {
                    (
                        k.clone(),
                        SessionModelUsage {
                            input_tokens: v.input_tokens,
                            output_tokens: v.output_tokens,
                            cache_read_tokens: v.cache_read_tokens,
                            cache_creation_tokens: v.cache_creation_tokens,
                            api_calls: v.api_calls,
                            cost_usd: v.cost_usd,
                        },
                    )
                })
                .collect(),
            total_cost_usd: s.model_usage.values().map(|u| u.cost_usd).sum(),
            messages: s.messages.clone(),
            git_branch: None,
            custom_title: Some(name.to_string()),
            ai_title: None,
            summary: None,
            last_prompt: None,
        };
        save_session(&snapshot)
    }

    /// Restore a session from disk, replacing current state.
    /// Applies message sanitization to fix orphaned thinking blocks,
    /// unresolved tool references, and other artifacts from interrupted sessions.
    pub async fn restore_session(&self, session_id: &str) -> anyhow::Result<String> {
        use clawed_core::message_sanitize::sanitize_messages;
        use clawed_core::session::load_session;
        let snap = load_session(session_id)?;
        let title = snap.title.clone();
        let (sanitized_messages, report) = sanitize_messages(snap.messages);
        if report.has_changes() {
            tracing::info!("Session restore {}: {}", session_id, report.summary());
        }
        {
            let mut s = self.state.write().await;
            s.messages = sanitized_messages;
            s.model = snap.model;
            s.turn_count = snap.turn_count;
            s.total_input_tokens = snap.input_tokens;
            s.total_output_tokens = snap.output_tokens;
        }
        // Reset abort signal for new session
        self.abort_signal.reset();
        Ok(title)
    }
}
