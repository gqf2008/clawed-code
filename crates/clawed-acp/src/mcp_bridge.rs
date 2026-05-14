//! MCP-over-ACP bridge.
//!
//! Implements the MCP-over-ACP transport mechanism (see ACP RFD:
//! [mcp-over-acp](https://agentclientprotocol.com/rfds/mcp-over-acp.md)).
//!
//! This allows MCP tools to be provided through ACP sessions without
//! requiring separate subprocesses or HTTP endpoints. The bridge:
//!
//! 1. Registers MCP servers with `"type": "acp"` transport in session configs
//! 2. Intercepts `mcp/connect`, `mcp/message`, `mcp/disconnect` ACP messages
//! 3. Routes them to the local `McpManager` for tool execution
//!
//! # Architecture
//!
//! ```text
//!   ACP Client          ACP Agent              McpManager
//!     │                    │                       │
//!     │  mcp/connect       │                       │
//!     │───────────────────▶│                       │
//!     │                    │  register connection   │
//!     │                    │──────────────────────▶│
//!     │  connectionId      │                       │
//!     │◀───────────────────│                       │
//!     │                    │                       │
//!     │  mcp/message       │                       │
//!     │  (tools/list)      │                       │
//!     │───────────────────▶│──────────────────────▶│
//!     │◀───────────────────│◀──────────────────────│
//!     │  tool list         │                       │
//!     │                    │                       │
//!     │  mcp/disconnect    │                       │
//!     │───────────────────▶│──────────────────────▶│
//! ```

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, Result};
use serde_json::Value;
use tokio::sync::RwLock;
use tracing::{info, warn};

use clawed_mcp::McpManager;

/// Represents an active MCP-over-ACP connection.
#[derive(Clone)]
#[derive(Clone)]
struct McpAcpConnection {
    /// The ACP component ID that owns this MCP server.
    #[allow(dead_code)]
    acp_id: String,
}

/// The MCP-over-ACP bridge, managing connections between ACP and MCP.
pub struct McpAcpBridge {
    /// Reference to the MCP manager for tool execution.
    mcp_manager: Arc<McpManager>,
    /// Active MCP-over-ACP connections: connection_id → connection.
    connections: Arc<RwLock<HashMap<String, McpAcpConnection>>>,
}

impl McpAcpBridge {
    /// Create a new bridge with a reference to the MCP manager.
    #[must_use]
    pub fn new(mcp_manager: Arc<McpManager>) -> Self {
        Self {
            mcp_manager,
            connections: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Handle `mcp/connect` — establishes a new MCP-over-ACP connection.
    ///
    /// Returns a fresh `connection_id` on success.
    pub async fn handle_connect(&self, acp_id: &str) -> Result<String> {
        let connection_id =
            format!("mcp_acp_{}", &uuid::Uuid::new_v4().simple().to_string()[..12]);

        let connection = McpAcpConnection {
            acp_id: acp_id.to_string(),
        };

        let mut conns = self.connections.write().await;
        conns.insert(connection_id.clone(), connection);
        info!(
            "MCP-over-ACP connect: acp_id={}, connection_id={}",
            &acp_id[..acp_id.len().min(8)],
            &connection_id[..connection_id.len().min(16)]
        );
        Ok(connection_id)
    }

    /// Handle `mcp/disconnect` — closes an MCP-over-ACP connection.
    pub async fn handle_disconnect(&self, connection_id: &str) -> Result<()> {
        let mut conns = self.connections.write().await;
        if conns.remove(connection_id).is_some() {
            info!("MCP-over-ACP disconnect: {}", &connection_id[..16]);
        } else {
            warn!("MCP-over-ACP disconnect for unknown connection: {}", &connection_id[..16]);
        }
        Ok(())
    }

    /// Handle `mcp/message` — routes an MCP message through the bridge.
    ///
    /// Supported methods:
    /// - `tools/list` — list tools from all connected MCP servers
    /// - `tools/call` — call a specific tool
    /// - `resources/list` — list resources
    pub async fn handle_message(
        &self,
        connection_id: &str,
        method: &str,
        params: Option<Value>,
    ) -> Result<Value> {
        let _conn = self.get_connection(connection_id).await?;

        match method {
            "tools/list" => {
                let tools = self
                    .mcp_manager
                    .list_all_tools()
                    .await
                    .context("Failed to list MCP tools")?;
                let tool_defs: Vec<Value> = tools
                    .into_iter()
                    .map(|(prefixed, tool)| {
                        serde_json::json!({
                            "name": prefixed,
                            "description": tool.description.unwrap_or_default(),
                            "inputSchema": tool.input_schema.unwrap_or(serde_json::json!({})),
                        })
                    })
                    .collect();
                Ok(serde_json::json!({ "tools": tool_defs }))
            }
            "tools/call" => {
                let tool_name = params
                    .as_ref()
                    .and_then(|p| p.get("name"))
                    .and_then(|v| v.as_str())
                    .context("Missing 'name' in tools/call params")?;
                let arguments = params
                    .as_ref()
                    .and_then(|p| p.get("arguments"))
                    .cloned()
                    .unwrap_or(serde_json::json!({}));

                let result = self
                    .mcp_manager
                    .call_tool(tool_name, arguments)
                    .await
                    .context("MCP tool call failed")?;

                let content: Vec<Value> = result
                    .content
                    .into_iter()
                    .map(|c| {
                        serde_json::json!({
                            "type": c.content_type,
                            "text": c.text,
                            "data": c.data,
                            "mimeType": c.mime_type,
                        })
                    })
                    .collect();

                Ok(serde_json::json!({
                    "content": content,
                    "isError": result.is_error,
                }))
            }
            "resources/list" => {
                let resources = self
                    .mcp_manager
                    .list_all_resources()
                    .await
                    .context("Failed to list MCP resources")?;
                let resource_defs: Vec<Value> = resources
                    .into_iter()
                    .map(|(_server, resource)| {
                        serde_json::json!({
                            "uri": resource.uri,
                            "name": resource.name,
                            "description": resource.description,
                            "mimeType": resource.mime_type,
                        })
                    })
                    .collect();
                Ok(serde_json::json!({ "resources": resource_defs }))
            }
            _ => anyhow::bail!("Unsupported MCP method: {method}"),
        }
    }

    /// List all MCP servers available through the bridge, in ACP `session/new` format.
    pub async fn mcp_servers_for_session(&self) -> Vec<Value> {
        let servers = self.mcp_manager.server_names().await;
        servers
            .into_iter()
            .map(|name| {
                serde_json::json!({
                    "type": "acp",
                    "name": name,
                    "id": format!("acp_{}", &name),
                })
            })
            .collect()
    }

    /// Get a connection by ID.
    async fn get_connection(&self, connection_id: &str) -> Result<McpAcpConnection> {
        let conns = self.connections.read().await;
        conns
            .get(connection_id)
            .cloned()
            .context("MCP-over-ACP connection not found")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn bridge_connect_disconnect() {
        let mcp = Arc::new(McpManager::new());
        let bridge = McpAcpBridge::new(mcp);

        let conn_id = bridge.handle_connect("test_acp_123").await.unwrap();
        assert!(!conn_id.is_empty());

        let result = bridge.handle_disconnect(&conn_id).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn bridge_disconnect_unknown() {
        let mcp = Arc::new(McpManager::new());
        let bridge = McpAcpBridge::new(mcp);

        let result = bridge.handle_disconnect("unknown").await;
        assert!(result.is_ok()); // disconnecting unknown is idempotent
    }

    #[tokio::test]
    async fn bridge_message_unknown_method() {
        let mcp = Arc::new(McpManager::new());
        let bridge = McpAcpBridge::new(mcp);

        let conn_id = bridge.handle_connect("test").await.unwrap();
        let result = bridge
            .handle_message(&conn_id, "unknown_method", None)
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn bridge_message_no_connection() {
        let mcp = Arc::new(McpManager::new());
        let bridge = McpAcpBridge::new(mcp);

        let result = bridge
            .handle_message("nonexistent", "tools/list", None)
            .await;
        assert!(result.is_err());
    }
}
