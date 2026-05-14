//! ACP Agent — wraps QueryEngine, manages sessions, handles prompts with streaming.

use std::sync::Arc;
use std::sync::OnceLock;

use anyhow::{Context, Result};
use futures::StreamExt;
use serde_json::{json, Value};
use tracing::{debug, info};

use clawed_agent::engine::QueryEngine;
use clawed_agent::query::AgentEvent;

use crate::mcp_bridge::McpAcpBridge;
use crate::session::SessionManager;
use crate::types;

/// Callback to send a JSON-RPC notification to the ACP client.
pub type NotifyFn = Arc<dyn Fn(Value) + Send + Sync>;

/// Global notification callback set once at startup by the server.
static NOTIFY: OnceLock<NotifyFn> = OnceLock::new();

/// Set the global notification callback for ACP streaming updates.
pub fn set_notify(f: NotifyFn) {
    let _ = NOTIFY.set(f);
}

/// The ACP Agent wrapping a QueryEngine.
pub struct AcpAgent {
    engine: Arc<QueryEngine>,
    sessions: Arc<SessionManager>,
    mcp_bridge: Arc<McpAcpBridge>,
}

impl AcpAgent {
    #[must_use]
    pub fn new(engine: Arc<QueryEngine>, mcp_bridge: Arc<McpAcpBridge>) -> Self {
        Self {
            engine,
            sessions: Arc::new(SessionManager::new()),
            mcp_bridge,
        }
    }

    #[must_use]
    pub fn session_manager(&self) -> &Arc<SessionManager> { &self.sessions }
    #[must_use]
    pub fn mcp_bridge(&self) -> &Arc<McpAcpBridge> { &self.mcp_bridge }
    #[must_use]
    pub fn info(&self) -> Value { types::agent_info(None) }
    #[must_use]
    pub fn capabilities(&self) -> Value { types::agent_capabilities() }

    /// Send a session/update notification to the client.
    fn notify_session(&self, sid: &str, update: Value) {
        if let Some(f) = NOTIFY.get() {
            f(json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":sid,"update":update}}));
        }
    }

    /// Handle session/new — create a new session.
    pub async fn handle_new_session(&self, cwd: &str) -> Result<Value> {
        let sid = self.sessions.create_session(self.engine.clone(), cwd.to_string()).await?;
        info!("ACP session: {} (cwd:{cwd})", &sid[..sid.len().min(16)]);
        Ok(json!({"sessionId": sid}))
    }

    /// Handle session/prompt — submit prompt and stream results.
    pub async fn handle_prompt(&self, sid: &str, content: &[Value]) -> Result<Value> {
        let session = self.sessions.get_session(sid).await.context("Session not found")?;
        let text = {
            let s = session.read().await;
            if !s.active { anyhow::bail!("Session is not active"); }
            types::extract_text_from_content(content).unwrap_or_default()
        };
        debug!("ACP prompt {}: {} chars", &sid[..sid.len().min(16)], text.len());

        let stream = { let s = session.read().await; s.engine.submit(&text).await };
        tokio::pin!(stream);
        let mut stop_reason = "end_turn";

        while let Some(event) = stream.next().await {
            match &event {
                AgentEvent::TextDelta(t) | AgentEvent::ThinkingDelta(t) => {
                    self.notify_session(sid, json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":t}}));
                }
                AgentEvent::ToolUseStart { id, name, .. } => {
                    self.notify_session(sid, json!({"sessionUpdate":"tool_call","toolCallId":id,"title":name,"kind":"tool","status":"pending"}));
                }
                AgentEvent::ToolResult { id, .. } => {
                    self.notify_session(sid, json!({"sessionUpdate":"tool_call_update","toolCallId":id,"status":"completed"}));
                }
                AgentEvent::TurnComplete { stop_reason: sr, .. } => {
                    stop_reason = match sr {
                        clawed_core::message::StopReason::EndTurn => "end_turn",
                        clawed_core::message::StopReason::MaxTokens => "max_tokens",
                        _ => "end_turn",
                    };
                }
                _ => {}
            }
        }
        Ok(json!({"stopReason": stop_reason}))
    }

    pub async fn handle_cancel(&self, sid: &str) -> Result<()> {
        self.sessions.get_session(sid).await.context("not found")?.read().await.engine.abort();
        Ok(())
    }
    pub async fn handle_close_session(&self, sid: &str) -> Result<()> { self.sessions.close_session(sid).await }
    pub async fn handle_list_sessions(&self) -> Value { json!({"sessions": self.sessions.list_sessions().await}) }

    pub async fn handle_read_text_file(&self, path: &str) -> Result<Value> {
        Ok(json!({"content": tokio::fs::read_to_string(path).await.with_context(||format!("read {path}"))?}))
    }
    pub async fn handle_write_text_file(&self, path: &str, content: &str) -> Result<Value> {
        tokio::fs::write(path, content).await.with_context(||format!("write {path}"))?;
        Ok(json!({}))
    }
}
