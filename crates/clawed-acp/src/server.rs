//! ACP server — JSON-RPC dispatch over stdio/WebSocket.
//! Routes ACP methods to the agent. Supports streaming via session/update.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::LazyLock;
use std::sync::Mutex as StdMutex;

use anyhow::Result;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::Mutex;
use tracing::info;

use clawed_agent::engine::QueryEngine;
use clawed_mcp::McpManager;

use crate::agent::AcpAgent;
use crate::mcp_bridge::McpAcpBridge;
use crate::transport::AcpTransportConfig;
use crate::types;

static TERMINALS: LazyLock<StdMutex<HashMap<String, std::process::Child>>> =
    LazyLock::new(|| StdMutex::new(HashMap::new()));

pub struct AcpServer {
    pub(crate) agent: Arc<AcpAgent>,
    pub(crate) _transport: AcpTransportConfig,
    pub(crate) version: Option<String>,
}

impl AcpServer {
    #[must_use]
    pub fn new(engine: Arc<QueryEngine>, mcp_manager: Arc<McpManager>, transport: AcpTransportConfig) -> Self {
        Self { agent: Arc::new(AcpAgent::new(engine, Arc::new(McpAcpBridge::new(mcp_manager)))), _transport: transport, version: None }
    }

    #[must_use]
    pub fn version(mut self, v: &str) -> Self { self.version = Some(v.into()); self }

    /// Run on stdio with streaming notification support.
    pub async fn run_stdio(&self) -> Result<()> {
        info!("ACP stdio starting");
        let writer = Arc::new(Mutex::new(tokio::io::stdout()));
        crate::agent::set_notify(Arc::new({
            let w = Arc::clone(&writer);
            move |msg: Value| {
                if let Ok(j) = serde_json::to_string(&msg) {
                    let w = Arc::clone(&w);
                    tokio::spawn(async move {
                        let mut w = w.lock().await;
                        let _ = w.write_all(j.as_bytes()).await;
                        let _ = w.write_all(b"\n").await;
                        let _ = w.flush().await;
                    });
                }
            }
        }));
        let mut lines = BufReader::new(tokio::io::stdin()).lines();
        while let Some(line) = lines.next_line().await? {
            let trimmed = line.trim();
            if !trimmed.is_empty() {
                if let Some(r) = self.dispatch(trimmed).await {
                    let mut j = serde_json::to_string(&r)?; j.push('\n');
                    let mut w = writer.lock().await;
                    w.write_all(j.as_bytes()).await?; w.flush().await.ok();
                }
            }
        }
        Ok(())
    }

    /// Run on WebSocket.
    pub async fn run_ws(&self, addr: &str) -> Result<()> {
        use tokio::net::TcpListener;
        use futures::StreamExt;
        let listener = TcpListener::bind(addr).await?;
        info!("ACP WS on {addr}");
        let self_arc = Arc::new(AcpServer {
            agent: Arc::clone(&self.agent),
            _transport: AcpTransportConfig::default(),
            version: self.version.clone(),
        });
        loop {
            let (stream, _peer) = listener.accept().await?;
            let ws_stream = tokio_tungstenite::accept_async(stream).await?;
            let (mut write, mut read) = ws_stream.split();
            let server = Arc::clone(&self_arc);
            tokio::spawn(async move {
                use futures::SinkExt;
                while let Some(Ok(msg)) = read.next().await {
                    let text = match msg.to_text() { Ok(t) => t.to_string(), _ => continue };
                    if let Some(r) = server.dispatch(&text).await {
                        if let Ok(j) = serde_json::to_string(&r) {
                            let _ = write.send(tokio_tungstenite::tungstenite::Message::Text(j)).await;
                        }
                    }
                }
            });
        }
    }

    /// Dispatch a single JSON-RPC 2.0 message.
    pub(crate) async fn dispatch(&self, msg_str: &str) -> Option<Value> {
        let msg: Value = match serde_json::from_str(msg_str) {
            Ok(m) => m,
            Err(e) => return Some(types::rpc_error(&Value::Null, -32700, &format!("Parse: {e}"))),
        };
        let method = msg.get("method")?.as_str()?;
        let id = msg.get("id").cloned().unwrap_or(Value::Null);
        let params = msg.get("params");
        let is_notif = id.is_null();

        let result = match method {
            "initialize" => Ok(types::initialize_response(self.version.as_deref())),
            "session/new" | "session/load" | "session/resume" => self.handle_new_session(params),
            "session/prompt" => self.handle_prompt(params).await,
            "session/cancel" => self.handle_cancel(params).await,
            "session/close" => self.handle_close(params).await,
            "session/list" => Ok(self.agent.handle_list_sessions().await),
            "session/set_config_option" => self.handle_set_config_option(params),
            "session/set_mode" => self.handle_set_mode(params),
            "authenticate" => Ok(json!({})),
            "session/request_permission" => Ok(json!({"outcome":"selected","optionId":"allow_once"})),
            "fs/read_text_file" => self.handle_fs_read(params).await,
            "fs/write_text_file" => self.handle_fs_write(params).await,
            "terminal/create" => self.handle_terminal_create(params),
            "terminal/kill" | "terminal/release" => { Self::handle_terminal_kill(params); Ok(json!({})) }
            "terminal/output" => Ok(Self::handle_terminal_output(params)),
            "terminal/wait_for_exit" => Self::handle_terminal_wait(params),
            "mcp/connect" => self.handle_mcp_connect(params),
            "mcp/message" => self.handle_mcp_msg(params).await,
            "mcp/disconnect" => self.handle_mcp_disconnect(params).await,
            _ => { if is_notif { return None; } Err(anyhow::anyhow!("Unknown: {method}")) }
        };

        match result {
            Ok(v) => if is_notif { None } else { Some(types::rpc_result(&id, v)) },
            Err(e) => { if is_notif { None } else { Some(types::rpc_error(&id, -32603, &e.to_string())) } }
        }
    }
}

// ── Handler implementations ──────────────────────────────────────────────

impl AcpServer {
    fn handle_new_session(&self, params: Option<&Value>) -> Result<Value> {
        let p = params.ok_or(anyhow::anyhow!("Missing params for session/new"))?;
        let cwd = p.get("cwd").and_then(|v| v.as_str()).ok_or(anyhow::anyhow!("Missing cwd"))?;
        let agent = self.agent.clone(); let cwd = cwd.to_string();
        tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(agent.handle_new_session(&cwd)))
    }

    async fn handle_prompt(&self, params: Option<&Value>) -> Result<Value> {
        let p = params.ok_or(anyhow::anyhow!("Missing params"))?;
        let sid = p.get("sessionId").and_then(|v| v.as_str()).ok_or(anyhow::anyhow!("Missing sessionId"))?;
        let content = p.get("prompt").and_then(|v| v.as_array()).cloned().unwrap_or_default();
        self.agent.handle_prompt(sid, &content).await
    }

    async fn handle_cancel(&self, params: Option<&Value>) -> Result<Value> {
        let sid = self.get_sid(params)?;
        self.agent.handle_cancel(sid).await?;
        Ok(json!({}))
    }

    async fn handle_close(&self, params: Option<&Value>) -> Result<Value> {
        let sid = self.get_sid(params)?;
        self.agent.handle_close_session(sid).await?;
        Ok(json!({}))
    }

    fn handle_set_config_option(&self, params: Option<&Value>) -> Result<Value> {
        let p = params.ok_or(anyhow::anyhow!("Missing params"))?;
        let sid = p.get("sessionId").and_then(|v| v.as_str()).ok_or(anyhow::anyhow!("Missing sessionId"))?;
        let key = p.get("configId").and_then(|v| v.as_str()).ok_or(anyhow::anyhow!("Missing configId"))?;
        let val = p.get("value").and_then(|v| v.as_str()).ok_or(anyhow::anyhow!("Missing value"))?;
        let agent = self.agent.clone(); let sid = sid.to_string();
        let key = key.to_string(); let val = val.to_string();
        let res = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async {
                agent.session_manager().set_config(&sid, &key, &val).await
            })
        });
        res?;
        Ok(tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(agent.session_manager().get_config_json(&sid))
        }))
    }

    fn handle_set_mode(&self, params: Option<&Value>) -> Result<Value> {
        let p = params.ok_or(anyhow::anyhow!("Missing params"))?;
        let sid = p.get("sessionId").and_then(|v| v.as_str()).ok_or(anyhow::anyhow!("Missing sessionId"))?;
        let mid = p.get("modeId").and_then(|v| v.as_str()).ok_or(anyhow::anyhow!("Missing modeId"))?;
        let agent = self.agent.clone(); let sid = sid.to_string(); let mid = mid.to_string();
        tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(agent.session_manager().set_mode(&sid, &mid)))?;
        Ok(json!({}))
    }

    /// Terminal handlers (static, use TERMINALS global)
    #[allow(clippy::unused_self)]
    fn handle_terminal_create(&self, params: Option<&Value>) -> Result<Value> {
        let p = params.ok_or(anyhow::anyhow!("Missing params"))?;
        let cmd = p.get("command").and_then(|v| v.as_str()).ok_or(anyhow::anyhow!("Missing command"))?;
        let tid = format!("term_{}", &uuid::Uuid::new_v4().simple().to_string()[..12]);
        match std::process::Command::new(cmd).spawn() {
            Ok(child) => { TERMINALS.lock().unwrap_or_else(|e| e.into_inner()).insert(tid.clone(), child); Ok(json!({"terminalId": tid})) }
            Err(e) => Err(anyhow::anyhow!("{e}")),
        }
    }

    fn handle_terminal_kill(params: Option<&Value>) {
        if let Some(tid) = params.and_then(|p| p.get("terminalId").and_then(|v| v.as_str())) {
            if let Some(mut child) = TERMINALS.lock().unwrap_or_else(|e| e.into_inner()).remove(tid) { let _ = child.kill(); let _ = child.wait(); }
        }
    }

    fn handle_terminal_output(params: Option<&Value>) -> Value {
        let tid = params.and_then(|p| p.get("terminalId").and_then(|v| v.as_str())).map(|s| s.to_string());
        let mut buf = String::new();
        if let Some(ref tid) = tid {
            let mut terms = TERMINALS.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(child) = terms.get_mut(tid) {
                use std::io::Read;
                if let Some(out) = child.stdout.as_mut() { let _ = out.read_to_string(&mut buf); }
            }
        }
        json!({"output": buf, "truncated": false})
    }

    fn handle_terminal_wait(params: Option<&Value>) -> Result<Value> {
        let tid = params.and_then(|p| p.get("terminalId").and_then(|v| v.as_str())).map(|s| s.to_string());
        match tid {
            Some(tid) => {
                let mut terms = TERMINALS.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(child) = terms.get_mut(&tid) {
                    Ok(json!({"exitCode": child.wait().ok().and_then(|s| s.code())}))
                } else {
                    Err(anyhow::anyhow!("Terminal not found"))
                }
            }
            None => Err(anyhow::anyhow!("Terminal ID required")),
        }
    }

    /// MCP handlers
    fn handle_mcp_connect(&self, params: Option<&Value>) -> Result<Value> {
        let p = params.ok_or(anyhow::anyhow!("Missing params"))?;
        let id = p.get("acpId").and_then(|v| v.as_str()).ok_or(anyhow::anyhow!("Missing acpId"))?;
        let agent = self.agent.clone(); let id = id.to_string();
        let conn_id = tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(agent.mcp_bridge().handle_connect(&id)))?;
        Ok(json!({"connectionId": conn_id}))
    }

    async fn handle_mcp_msg(&self, params: Option<&Value>) -> Result<Value> {
        let p = params.ok_or(anyhow::anyhow!("Missing params"))?;
        let cid = p.get("connectionId").and_then(|v| v.as_str()).ok_or(anyhow::anyhow!("Missing connectionId"))?;
        let method = p.get("method").and_then(|v| v.as_str()).ok_or(anyhow::anyhow!("Missing method"))?;
        self.agent.mcp_bridge().handle_message(cid, method, p.get("params").cloned()).await
    }

    async fn handle_mcp_disconnect(&self, params: Option<&Value>) -> Result<Value> {
        let cid = params.and_then(|p| p.get("connectionId").and_then(|v| v.as_str())).ok_or(anyhow::anyhow!("Missing connectionId"))?;
        self.agent.mcp_bridge().handle_disconnect(cid).await?;
        Ok(json!({}))
    }

    /// Filesystem handlers
    async fn handle_fs_read(&self, params: Option<&Value>) -> Result<Value> {
        let path = params.and_then(|p| p.get("path").and_then(|v| v.as_str())).ok_or(anyhow::anyhow!("Missing path"))?;
        self.agent.handle_read_text_file(path).await
    }

    async fn handle_fs_write(&self, params: Option<&Value>) -> Result<Value> {
        let p = params.ok_or(anyhow::anyhow!("Missing params"))?;
        let path = p.get("path").and_then(|v| v.as_str()).ok_or(anyhow::anyhow!("Missing path"))?;
        let content = p.get("content").and_then(|v| v.as_str()).ok_or(anyhow::anyhow!("Missing content"))?;
        self.agent.handle_write_text_file(path, content).await
    }

    /// Helpers
    fn get_sid<'a>(&self, params: Option<&'a Value>) -> Result<&'a str> {
        params.and_then(|p| p.get("sessionId").and_then(|v| v.as_str())).ok_or(anyhow::anyhow!("Missing sessionId"))
    }
}
