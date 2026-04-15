//! MCP Streamable HTTP transport — JSON-RPC 2.0 over HTTP with optional SSE streaming.
//!
//! Implements the MCP "Streamable HTTP" transport (protocol version 2025-03-26):
//!   - Client POSTs JSON-RPC requests to a single endpoint
//!   - Server responds with either JSON (Content-Type: application/json)
//!     or SSE stream (Content-Type: text/event-stream)
//!   - Client can GET the endpoint for server-initiated notifications (SSE)
//!   - Session state tracked via `Mcp-Session-Id` header
//!   - Client can DELETE to terminate the session

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use anyhow::{Context, Result};
use serde_json::Value;
use tokio::sync::{mpsc, oneshot, Mutex, RwLock};
use tracing::{debug, info, warn};

use crate::protocol::{JsonRpcNotification, JsonRpcRequest, JsonRpcResponse};

struct PendingRequest {
    tx: oneshot::Sender<JsonRpcResponse>,
}

/// Configuration for connecting to an MCP server via Streamable HTTP.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct McpStreamableHttpConfig {
    /// Endpoint URL (e.g. `https://mcp.example.com/mcp`).
    pub url: String,
    /// Optional HTTP headers (for auth, etc.).
    #[serde(default)]
    pub headers: HashMap<String, String>,
}

/// Streamable HTTP transport for MCP.
///
/// Unlike the legacy SSE transport (which discovers a POST endpoint from the SSE
/// stream), this transport uses a single URL for all operations:
/// - POST: send JSON-RPC requests; response is JSON or SSE
/// - GET: open SSE stream for server-initiated messages
/// - DELETE: terminate session
pub struct StreamableHttpTransport {
    http: reqwest::Client,
    url: String,
    headers: HashMap<String, String>,
    session_id: Arc<RwLock<Option<String>>>,
    next_id: AtomicU64,
    pending: Arc<Mutex<HashMap<u64, PendingRequest>>>,
    notification_rx: Mutex<mpsc::UnboundedReceiver<JsonRpcNotification>>,
    notification_tx: mpsc::UnboundedSender<JsonRpcNotification>,
    _sse_listener: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl StreamableHttpTransport {
    /// Connect to a Streamable HTTP MCP server.
    ///
    /// This does NOT open the optional GET SSE stream — that happens lazily
    /// when the server sends back a session ID.
    pub async fn connect(config: &McpStreamableHttpConfig) -> Result<Self> {
        info!("Connecting to MCP Streamable HTTP server at {}", config.url);

        let http = reqwest::Client::builder()
            .user_agent("claude-code-rs/0.1 MCP-StreamableHTTP")
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        let (notification_tx, notification_rx) = mpsc::unbounded_channel();

        Ok(Self {
            http,
            url: config.url.clone(),
            headers: config.headers.clone(),
            session_id: Arc::new(RwLock::new(None)),
            next_id: AtomicU64::new(1),
            pending: Arc::new(Mutex::new(HashMap::new())),
            notification_rx: Mutex::new(notification_rx),
            notification_tx,
            _sse_listener: Mutex::new(None),
        })
    }

    /// Build request headers (auth + session ID + custom).
    async fn build_headers(&self) -> reqwest::header::HeaderMap {
        let mut header_map = reqwest::header::HeaderMap::new();
        header_map.insert(
            reqwest::header::CONTENT_TYPE,
            reqwest::header::HeaderValue::from_static("application/json"),
        );
        header_map.insert(
            reqwest::header::ACCEPT,
            reqwest::header::HeaderValue::from_static("application/json, text/event-stream"),
        );
        // Session ID
        if let Some(sid) = self.session_id.read().await.as_ref() {
            if let Ok(val) = reqwest::header::HeaderValue::from_str(sid) {
                header_map.insert(
                    reqwest::header::HeaderName::from_static("mcp-session-id"),
                    val,
                );
            }
        }
        // Custom headers
        for (k, v) in &self.headers {
            if let (Ok(name), Ok(val)) = (
                reqwest::header::HeaderName::from_bytes(k.as_bytes()),
                reqwest::header::HeaderValue::from_str(v),
            ) {
                header_map.insert(name, val);
            }
        }
        header_map
    }

    /// Extract and store session ID from response headers.
    async fn capture_session_id(&self, response: &reqwest::Response) {
        if let Some(sid) = response.headers().get("mcp-session-id") {
            if let Ok(s) = sid.to_str() {
                let mut current = self.session_id.write().await;
                let is_new = current.is_none();
                *current = Some(s.to_string());
                if is_new {
                    debug!("MCP Streamable HTTP: session ID = {s}");
                }
            }
        }
    }

    /// Send a JSON-RPC request and wait for the response.
    ///
    /// The server may respond with:
    /// - `application/json` — parse as a single JSON-RPC response
    /// - `text/event-stream` — parse SSE events, first response with matching ID wins
    pub async fn request(&self, method: &str, params: Option<Value>) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let request = JsonRpcRequest::new(id, method, params);

        let headers = self.build_headers().await;
        let body = serde_json::to_string(&request)?;

        let response = self
            .http
            .post(&self.url)
            .headers(headers)
            .body(body)
            .send()
            .await
            .with_context(|| format!("Failed to POST to MCP endpoint: {}", self.url))?;

        if !response.status().is_success() {
            anyhow::bail!(
                "MCP POST failed with status {}: {}",
                response.status(),
                response.text().await.unwrap_or_default()
            );
        }

        self.capture_session_id(&response).await;

        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();

        if content_type.contains("text/event-stream") {
            // SSE streaming response — parse events until we get our response
            self.handle_sse_response(id, response).await
        } else {
            // Direct JSON response
            let rpc_response: JsonRpcResponse = response
                .json()
                .await
                .context("Failed to parse JSON-RPC response")?;
            if let Some(error) = rpc_response.error {
                anyhow::bail!(
                    "MCP error {}: {} {}",
                    error.code,
                    error.message,
                    error.data.map(|d| d.to_string()).unwrap_or_default()
                );
            }
            Ok(rpc_response.result.unwrap_or(Value::Null))
        }
    }

    /// Parse an SSE streaming response, waiting for the response matching our request ID.
    async fn handle_sse_response(
        &self,
        request_id: u64,
        response: reqwest::Response,
    ) -> Result<Value> {
        use futures::StreamExt;

        let mut byte_stream = response.bytes_stream();
        let mut buffer = String::new();
        let timeout = std::time::Duration::from_secs(300);
        let deadline = tokio::time::Instant::now() + timeout;

        loop {
            match tokio::time::timeout_at(deadline, byte_stream.next()).await {
                Ok(Some(Ok(chunk))) => {
                    buffer.push_str(&String::from_utf8_lossy(&chunk));
                    while let Some(pos) = buffer.find("\n\n") {
                        let event_block = buffer[..pos].to_string();
                        buffer = buffer[pos + 2..].to_string();

                        if let Some(data) = extract_sse_data(&event_block) {
                            // Try as a JSON-RPC response
                            if let Ok(rpc_resp) = serde_json::from_str::<JsonRpcResponse>(&data) {
                                if rpc_resp.id == Some(request_id) {
                                    if let Some(error) = rpc_resp.error {
                                        anyhow::bail!(
                                            "MCP error {}: {} {}",
                                            error.code,
                                            error.message,
                                            error.data.map(|d| d.to_string()).unwrap_or_default()
                                        );
                                    }
                                    return Ok(rpc_resp.result.unwrap_or(Value::Null));
                                }
                                // Response for a different request — route to pending
                                if let Some(id) = rpc_resp.id {
                                    let mut pending = self.pending.lock().await;
                                    if let Some(req) = pending.remove(&id) {
                                        let _ = req.tx.send(rpc_resp);
                                    }
                                }
                                continue;
                            }
                            // Try as a notification
                            if let Ok(notif) = serde_json::from_str::<JsonRpcNotification>(&data) {
                                let _ = self.notification_tx.send(notif);
                            }
                        }
                    }
                }
                Ok(Some(Err(e))) => {
                    anyhow::bail!("MCP SSE stream error: {e}");
                }
                Ok(None) => {
                    anyhow::bail!("MCP SSE stream ended before receiving response");
                }
                Err(_) => {
                    anyhow::bail!("MCP request timed out after {}s", timeout.as_secs());
                }
            }
        }
    }

    /// Send a notification (fire-and-forget).
    pub async fn notify(&self, method: &str, params: Option<Value>) -> Result<()> {
        let notification = JsonRpcNotification::new(method, params);
        let headers = self.build_headers().await;
        let body = serde_json::to_string(&notification)?;

        let response = self
            .http
            .post(&self.url)
            .headers(headers)
            .body(body)
            .send()
            .await?;

        self.capture_session_id(&response).await;
        Ok(())
    }

    /// Try to receive a server-initiated notification (non-blocking).
    pub async fn try_recv_notification(&self) -> Option<JsonRpcNotification> {
        self.notification_rx.lock().await.try_recv().ok()
    }

    /// Open a long-lived GET SSE stream for server-initiated messages.
    ///
    /// Call this after initialization once the session ID is established.
    /// Notifications received on this stream are routed to `try_recv_notification()`.
    pub async fn open_sse_listener(&self) -> Result<()> {
        let sid = self.session_id.read().await.clone();
        let Some(sid) = sid else {
            debug!("No session ID yet — skipping SSE listener");
            return Ok(());
        };

        let mut header_map = reqwest::header::HeaderMap::new();
        header_map.insert(
            reqwest::header::ACCEPT,
            reqwest::header::HeaderValue::from_static("text/event-stream"),
        );
        if let Ok(val) = reqwest::header::HeaderValue::from_str(&sid) {
            header_map.insert(
                reqwest::header::HeaderName::from_static("mcp-session-id"),
                val,
            );
        }
        for (k, v) in &self.headers {
            if let (Ok(name), Ok(val)) = (
                reqwest::header::HeaderName::from_bytes(k.as_bytes()),
                reqwest::header::HeaderValue::from_str(v),
            ) {
                header_map.insert(name, val);
            }
        }

        let response = self
            .http
            .get(&self.url)
            .headers(header_map)
            .send()
            .await
            .with_context(|| format!("Failed to open SSE stream: {}", self.url))?;

        if !response.status().is_success() {
            anyhow::bail!(
                "SSE GET failed with status {}: {}",
                response.status(),
                response.text().await.unwrap_or_default()
            );
        }

        let pending = Arc::clone(&self.pending);
        let notification_tx = self.notification_tx.clone();

        let handle = tokio::spawn(async move {
            use futures::StreamExt;
            let mut byte_stream = response.bytes_stream();
            let mut buf = String::new();
            loop {
                match byte_stream.next().await {
                    Some(Ok(chunk)) => {
                        buf.push_str(&String::from_utf8_lossy(&chunk));
                        while let Some(pos) = buf.find("\n\n") {
                            let event_block = buf[..pos].to_string();
                            buf = buf[pos + 2..].to_string();
                            if let Some(data) = extract_sse_data(&event_block) {
                                route_message(&data, &pending, &notification_tx).await;
                            }
                        }
                    }
                    Some(Err(e)) => {
                        warn!("MCP Streamable HTTP SSE listener error: {e}");
                        break;
                    }
                    None => {
                        debug!("MCP Streamable HTTP SSE listener stream ended");
                        break;
                    }
                }
            }
        });

        let mut listener = self._sse_listener.lock().await;
        *listener = Some(handle);
        Ok(())
    }

    /// Terminate the session on the server via DELETE.
    pub async fn close(&self) -> Result<()> {
        let headers = self.build_headers().await;
        let _ = self.http.delete(&self.url).headers(headers).send().await;
        debug!("MCP Streamable HTTP session closed");
        Ok(())
    }

    /// The endpoint URL.
    pub fn url(&self) -> &str {
        &self.url
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────────

fn extract_sse_data(block: &str) -> Option<String> {
    for line in block.lines() {
        if let Some(rest) = line.strip_prefix("data:") {
            return Some(rest.trim().to_string());
        }
    }
    None
}

async fn route_message(
    data: &str,
    pending: &Arc<Mutex<HashMap<u64, PendingRequest>>>,
    notification_tx: &mpsc::UnboundedSender<JsonRpcNotification>,
) {
    if let Ok(response) = serde_json::from_str::<JsonRpcResponse>(data) {
        if let Some(id) = response.id {
            let mut pending = pending.lock().await;
            if let Some(req) = pending.remove(&id) {
                let _ = req.tx.send(response);
                return;
            }
        }
    }
    if let Ok(notification) = serde_json::from_str::<JsonRpcNotification>(data) {
        let _ = notification_tx.send(notification);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_data_from_sse_block() {
        let block = "event: message\ndata: {\"jsonrpc\":\"2.0\"}";
        assert_eq!(
            extract_sse_data(block),
            Some("{\"jsonrpc\":\"2.0\"}".to_string())
        );
    }

    #[test]
    fn extract_data_only_field() {
        let block = "data: hello";
        assert_eq!(extract_sse_data(block), Some("hello".to_string()));
    }

    #[test]
    fn extract_data_no_data_field() {
        let block = "event: endpoint\nid: 123";
        assert_eq!(extract_sse_data(block), None);
    }

    #[tokio::test]
    async fn config_round_trips_through_json() {
        let config = McpStreamableHttpConfig {
            url: "https://example.com/mcp".into(),
            headers: HashMap::from([("Authorization".into(), "Bearer tok".into())]),
        };
        let json = serde_json::to_string(&config).unwrap();
        let parsed: McpStreamableHttpConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.url, "https://example.com/mcp");
        assert_eq!(parsed.headers.len(), 1);
    }

    #[tokio::test]
    async fn connect_creates_transport() {
        let config = McpStreamableHttpConfig {
            url: "https://example.com/mcp".into(),
            headers: HashMap::new(),
        };
        let transport = StreamableHttpTransport::connect(&config).await.unwrap();
        assert_eq!(transport.url(), "https://example.com/mcp");
    }
}
