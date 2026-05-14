//! ACP Agent — wraps QueryEngine, manages sessions, handles prompts.
//!
//! The agent implements core ACP protocol methods: initialize,
//! session lifecycle, prompt submission, cancellation, and filesystem access.

use std::sync::Arc;

use anyhow::{Context, Result};
use serde_json::Value;
use tracing::{debug, info};

use clawed_agent::engine::QueryEngine;

use crate::mcp_bridge::McpAcpBridge;
use crate::session::SessionManager;
use crate::types;

/// The ACP Agent wrapping a QueryEngine.
pub struct AcpAgent {
    engine: Arc<QueryEngine>,
    sessions: Arc<SessionManager>,
    mcp_bridge: Arc<McpAcpBridge>,
}

impl AcpAgent {
    /// Create a new ACP agent.
    #[must_use]
    pub fn new(engine: Arc<QueryEngine>, mcp_bridge: Arc<McpAcpBridge>) -> Self {
        Self {
            engine,
            sessions: Arc::new(SessionManager::new()),
            mcp_bridge,
        }
    }

    /// Reference to session manager.
    #[must_use]
    pub fn session_manager(&self) -> &Arc<SessionManager> {
        &self.sessions
    }

    /// Reference to MCP bridge.
    #[must_use]
    pub fn mcp_bridge(&self) -> &Arc<McpAcpBridge> {
        &self.mcp_bridge
    }

    /// Agent info for initialize response.
    #[must_use]
    pub fn info(&self) -> Value {
        types::agent_info()
    }

    /// Agent capabilities for initialize response.
    #[must_use]
    pub fn capabilities(&self) -> Value {
        types::agent_capabilities()
    }

    /// Handle session/new — create a new session.
    pub async fn handle_new_session(&self, cwd: &str) -> Result<Value> {
        let session_id = self
            .sessions
            .create_session(self.engine.clone(), cwd.to_string())
            .await?;
        info!("ACP session: {} (cwd: {cwd})", &session_id[..16]);
        Ok(serde_json::json!({ "sessionId": session_id }))
    }

    /// Handle session/prompt.
    pub async fn handle_prompt(&self, session_id: &str, content: &[Value]) -> Result<Value> {
        let session = self
            .sessions
            .get_session(session_id)
            .await
            .context("Session not found")?;

        {
            let s = session.read().await;
            if !s.active {
                anyhow::bail!("Session is not active");
            }
        }

        let text = types::extract_text_from_content(content).unwrap_or_default();
        debug!("ACP prompt {}: {} chars", &session_id[..16], text.len());

        {
            let s = session.read().await;
            let _stream = s.engine.submit(&text).await;
        }

        Ok(serde_json::json!({ "stopReason": "end_turn" }))
    }

    /// Handle session/cancel.
    pub async fn handle_cancel(&self, session_id: &str) -> Result<()> {
        let session = self
            .sessions
            .get_session(session_id)
            .await
            .context("Session not found")?;
        session.read().await.engine.abort();
        Ok(())
    }

    /// Handle session/close.
    pub async fn handle_close_session(&self, session_id: &str) -> Result<()> {
        self.sessions.close_session(session_id).await
    }

    /// Handle session/list.
    pub async fn handle_list_sessions(&self) -> Value {
        let sessions = self.sessions.list_sessions().await;
        serde_json::json!({ "sessions": sessions })
    }

    /// Handle fs/read_text_file.
    pub async fn handle_read_text_file(&self, path: &str) -> Result<Value> {
        let content = tokio::fs::read_to_string(path)
            .await
            .with_context(|| format!("Failed to read: {path}"))?;
        Ok(serde_json::json!({ "content": content }))
    }

    /// Handle fs/write_text_file.
    pub async fn handle_write_text_file(&self, path: &str, content: &str) -> Result<Value> {
        tokio::fs::write(path, content)
            .await
            .with_context(|| format!("Failed to write: {path}"))?;
        Ok(serde_json::json!({}))
    }
}
