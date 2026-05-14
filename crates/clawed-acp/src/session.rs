//! ACP session lifecycle management.
//!
//! Manages ACP sessions, each wrapping a `QueryEngine` instance.
//! Sessions are created via `session/new`, used via `session/prompt`,
//! and cleaned up via `session/close`.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use serde_json::{json, Value};
use tokio::sync::RwLock;
use tracing::info;

use clawed_agent::engine::QueryEngine;

/// A single ACP session wrapping a QueryEngine.
pub struct AcpSession {
    /// Session ID (e.g. "sess_abc123").
    pub id: String,
    /// The engine processing prompts for this session.
    pub engine: Arc<QueryEngine>,
    /// Working directory when session was created.
    pub cwd: String,
    /// Whether the session is active.
    pub active: bool,
    /// Session config options.
    pub config: HashMap<String, String>,
    /// Session mode.
    pub mode: Option<String>,
}

/// Manages all ACP sessions.
pub struct SessionManager {
    sessions: Arc<RwLock<HashMap<String, Arc<RwLock<AcpSession>>>>>,
}

impl Default for SessionManager {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionManager {
    #[must_use]
    pub fn new() -> Self {
        Self {
            sessions: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Create a new session and return its ID.
    pub async fn create_session(
        &self,
        engine: Arc<QueryEngine>,
        cwd: String,
    ) -> Result<String> {
        let session_id = format!("sess_{}", &uuid::Uuid::new_v4().simple().to_string()[..12]);

        let session = AcpSession {
            id: session_id.clone(),
            engine,
            cwd,
            active: true,
            config: HashMap::new(),
            mode: None,
        };

        let mut sessions = self.sessions.write().await;
        sessions.insert(session_id.clone(), Arc::new(RwLock::new(session)));
        info!("ACP session created: {}", &session_id[..16]);
        Ok(session_id)
    }

    /// Get a session by ID.
    pub async fn get_session(
        &self,
        session_id: &str,
    ) -> Option<Arc<RwLock<AcpSession>>> {
        let sessions = self.sessions.read().await;
        sessions.get(session_id).cloned()
    }

    /// Close and remove a session.
    pub async fn close_session(&self, session_id: &str) -> Result<()> {
        let mut sessions = self.sessions.write().await;
        if let Some(session) = sessions.remove(session_id) {
            let mut s = session.write().await;
            s.active = false;
            info!("ACP session closed: {}", &session_id[..16]);
        }
        Ok(())
    }

    /// List active sessions as JSON Values.
    pub async fn list_sessions(&self) -> Vec<Value> {
        let sessions = self.sessions.read().await;
        let mut result = Vec::new();
        for (id, session) in sessions.iter() {
            let s = session.read().await;
            if s.active {
                result.push(serde_json::json!({
                    "sessionId": id,
                    "cwd": s.cwd,
                }));
            }
        }
        result
    }

    /// Check if a session is active.
    pub async fn set_config(&self, sid: &str, key: &str, val: &str) -> Result<()> {
        let sessions = self.sessions.read().await;
        let session = sessions.get(sid).ok_or(anyhow::anyhow!("not found"))?;
        let mut s = session.write().await;
        s.config.insert(key.into(), val.into());
        Ok(())
    }

    pub async fn get_config_json(&self, sid: &str) -> Value {
        let sessions = self.sessions.read().await;
        let session = match sessions.get(sid) { Some(s) => s, _ => return json!({"configOptions":[]}) };
        let s = session.read().await;
        let opts: Vec<Value> = s.config.iter().map(|(k,v)| json!({"configId":k,"type":"select","currentValue":v,"options":[{"name":v,"value":v}]})).collect();
        json!({"configOptions": opts})
    }

    pub async fn set_mode(&self, sid: &str, mid: &str) -> Result<()> {
        let sessions = self.sessions.read().await;
        let session = sessions.get(sid).ok_or(anyhow::anyhow!("not found"))?;
        session.write().await.mode = Some(mid.into());
        Ok(())
    }

    pub async fn is_active(&self, session_id: &str) -> bool {
        let sessions = self.sessions.read().await;
        if let Some(s) = sessions.get(session_id) { s.read().await.active } else { false }
    }
}
