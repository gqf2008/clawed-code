//! ACP server — JSON-RPC message dispatch over configurable transports.
//!
//! Routes incoming ACP JSON-RPC messages to the agent and returns responses.
//! Uses `serde_json::Value` for protocol messages to avoid tight coupling
//! with the schema crate's `#[non_exhaustive]` types.

use std::sync::Arc;

use anyhow::{Context, Result};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tracing::{info, warn};

use clawed_agent::engine::QueryEngine;
use clawed_mcp::McpManager;

use crate::agent::AcpAgent;
use crate::mcp_bridge::McpAcpBridge;
use crate::transport::AcpTransportConfig;
use crate::types;

/// ACP server dispatching JSON-RPC messages to the agent.
pub struct AcpServer {
    agent: Arc<AcpAgent>,
    _transport: AcpTransportConfig,
    version: Option<String>,
}

impl AcpServer {
    /// Create a new ACP server wrapping an engine and MCP manager.
    #[must_use]
    pub fn new(
        engine: Arc<QueryEngine>,
        mcp_manager: Arc<McpManager>,
        transport: AcpTransportConfig,
    ) -> Self {
        let mcp_bridge = Arc::new(McpAcpBridge::new(mcp_manager));
        let agent = Arc::new(AcpAgent::new(engine, mcp_bridge));
        Self {
            agent,
            _transport: transport,
            version: None,
        }
    }

    /// Set the application version string reported during initialization.
    #[must_use]
    pub fn version(mut self, version: &str) -> Self {
        self.version = Some(version.to_string());
        self
    }

    /// Run the server on stdio (default local agent transport).
    pub async fn run_stdio(&self) -> Result<()> {
        info!("ACP stdio server starting");
        let mut reader = BufReader::new(tokio::io::stdin());
        let mut writer = tokio::io::stdout();
        let mut line = String::new();

        loop {
            line.clear();
            if reader.read_line(&mut line).await? == 0 {
                info!("ACP stdin closed");
                break;
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            if let Some(resp) = self.dispatch(trimmed).await {
                let mut json = serde_json::to_string(&resp)?;
                json.push('\n');
                writer.write_all(json.as_bytes()).await?;
                writer.flush().await.ok();
            }
        }
        Ok(())
    }

    /// Dispatch a single JSON-RPC message.
    async fn dispatch(&self, msg_str: &str) -> Option<Value> {
        let msg: Value = match serde_json::from_str(msg_str) {
            Ok(m) => m,
            Err(e) => {
                return Some(types::rpc_error(
                    &serde_json::json!(null),
                    -32700,
                    &format!("Parse error: {e}"),
                ));
            }
        };

        let method = msg
            .get("method")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let id = msg.get("id").cloned().unwrap_or(Value::Null);
        let params = msg.get("params");
        let is_notification = id.is_null();

        let result = match method {
            "initialize" => Ok(types::initialize_response(self.version.as_deref())),
            "session/new" => self.handle_new_session(params),
            "session/prompt" => self.handle_prompt(params).await,
            "session/cancel" => self.handle_cancel(params).await,
            "session/close" => self.handle_close(params).await,
            "session/list" => Ok(self.agent.handle_list_sessions().await),
            "mcp/connect" => self.handle_mcp_connect(params),
            "mcp/message" => self.handle_mcp_msg(params).await,
            "mcp/disconnect" => self.handle_mcp_disconnect(params).await,
            "fs/read_text_file" => self.handle_fs_read(params).await,
            "fs/write_text_file" => self.handle_fs_write(params).await,
            _ => {
                if is_notification {
                    return None;
                }
                Err(anyhow::anyhow!("Method not found: {method}"))
            }
        };

        match result {
            Ok(value) => {
                if is_notification {
                    None
                } else {
                    Some(types::rpc_result(&id, value))
                }
            }
            Err(e) => {
                if is_notification {
                    warn!("ACP notification error dropped: {e}");
                    None
                } else {
                    Some(types::rpc_error(&id, -32603, &e.to_string()))
                }
            }
        }
    }

    fn handle_new_session(&self, params: Option<&Value>) -> Result<Value> {
        let p = params.context("Missing params for session/new")?;
        let cwd = p
            .get("cwd")
            .and_then(|v| v.as_str())
            .context("Missing 'cwd' in session/new")?;
        // Spawn async since handle_new_session is async
        let cwd = cwd.to_string();
        let agent = self.agent.clone();
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current()
                .block_on(agent.handle_new_session(&cwd))
        })
    }

    async fn handle_prompt(&self, params: Option<&Value>) -> Result<Value> {
        let p = params.context("Missing params for session/prompt")?;
        let session_id = p
            .get("sessionId")
            .and_then(|v| v.as_str())
            .context("Missing 'sessionId'")?;
        let content: Vec<Value> = p
            .get("prompt")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        self.agent.handle_prompt(session_id, &content).await
    }

    async fn handle_cancel(&self, params: Option<&Value>) -> Result<Value> {
        let p = params.context("Missing params")?;
        let sid = p
            .get("sessionId")
            .and_then(|v| v.as_str())
            .context("Missing sessionId")?;
        self.agent.handle_cancel(sid).await?;
        Ok(serde_json::json!({}))
    }

    async fn handle_close(&self, params: Option<&Value>) -> Result<Value> {
        let p = params.context("Missing params")?;
        let sid = p
            .get("sessionId")
            .and_then(|v| v.as_str())
            .context("Missing sessionId")?;
        self.agent.handle_close_session(sid).await?;
        Ok(serde_json::json!({}))
    }

    fn handle_mcp_connect(&self, params: Option<&Value>) -> Result<Value> {
        let p = params.context("Missing params")?;
        let acp_id = p
            .get("acpId")
            .and_then(|v| v.as_str())
            .context("Missing acpId")?;
        let agent = self.agent.clone();
        let acp_id = acp_id.to_string();
        let conn_id = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current()
                .block_on(agent.mcp_bridge().handle_connect(&acp_id))
        })?;
        Ok(serde_json::json!({ "connectionId": conn_id }))
    }

    async fn handle_mcp_msg(&self, params: Option<&Value>) -> Result<Value> {
        let p = params.context("Missing params")?;
        let cid = p
            .get("connectionId")
            .and_then(|v| v.as_str())
            .context("Missing connectionId")?;
        let method = p
            .get("method")
            .and_then(|v| v.as_str())
            .context("Missing method")?;
        let ip = p.get("params").cloned();
        self.agent
            .mcp_bridge()
            .handle_message(cid, method, ip)
            .await
    }

    async fn handle_mcp_disconnect(&self, params: Option<&Value>) -> Result<Value> {
        let p = params.context("Missing params")?;
        let cid = p
            .get("connectionId")
            .and_then(|v| v.as_str())
            .context("Missing connectionId")?;
        self.agent
            .mcp_bridge()
            .handle_disconnect(cid)
            .await?;
        Ok(serde_json::json!({}))
    }

    async fn handle_fs_read(&self, params: Option<&Value>) -> Result<Value> {
        let p = params.context("Missing params")?;
        let path = p
            .get("path")
            .and_then(|v| v.as_str())
            .context("Missing path")?;
        self.agent.handle_read_text_file(path).await
    }

    async fn handle_fs_write(&self, params: Option<&Value>) -> Result<Value> {
        let p = params.context("Missing params")?;
        let path = p
            .get("path")
            .and_then(|v| v.as_str())
            .context("Missing path")?;
        let content = p
            .get("content")
            .and_then(|v| v.as_str())
            .context("Missing content")?;
        self.agent.handle_write_text_file(path, content).await
    }
}
