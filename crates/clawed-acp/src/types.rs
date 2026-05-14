//! ACP protocol message builders using serde_json::Value.
//!
//! Building protocol messages as JSON Values avoids tight coupling
//! with the `#[non_exhaustive]` types in the schema crate and lets
//! us control the exact wire format.

use serde_json::{json, Value};

/// ACP agent info metadata.
pub fn agent_info() -> Value {
    json!({
        "name": "clawed",
        "title": "Clawed Code Agent",
        "version": env!("CARGO_PKG_VERSION"),
    })
}

/// ACP agent capabilities advertised during initialization.
pub fn agent_capabilities() -> Value {
    json!({
        "loadSession": false,
        "promptCapabilities": {
            "image": true,
            "audio": false,
            "embeddedContext": true
        },
        "mcpCapabilities": {
            "http": true,
            "sse": false
        },
        "sessionCapabilities": {}
    })
}

/// ACP initialize response body.
pub fn initialize_response() -> Value {
    json!({
        "protocolVersion": 1,
        "agentCapabilities": agent_capabilities(),
        "agentInfo": agent_info(),
        "authMethods": [],
    })
}

/// A JSON-RPC 2.0 success response.
pub fn rpc_result(id: &Value, result: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result,
    })
}

/// A JSON-RPC 2.0 error response.
pub fn rpc_error(id: &Value, code: i64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": code,
            "message": message,
        }
    })
}

/// Extract plain text from an ACP content block array.
pub fn extract_text_from_content(blocks: &[Value]) -> Option<String> {
    let mut text = String::new();
    for block in blocks {
        match block.get("type").and_then(|v| v.as_str()) {
            Some("text") => {
                if let Some(t) = block.get("text").and_then(|v| v.as_str()) {
                    text.push_str(t);
                    text.push('\n');
                }
            }
            Some("resource_link") => {
                let name = block.get("name").and_then(|v| v.as_str()).unwrap_or("unknown");
                text.push_str(&format!("[Resource: {name}]\n"));
            }
            _ => {}
        }
    }
    if text.is_empty() { None } else { Some(text) }
}
