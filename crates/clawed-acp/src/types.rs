//! ACP protocol message builders using serde_json::Value.
//!
//! Building protocol messages as JSON Values avoids tight coupling
//! with the `#[non_exhaustive]` types in the schema crate and lets
//! us control the exact wire format.

use serde_json::{json, Value};

/// ACP agent info metadata.
pub fn agent_info(version: Option<&str>) -> Value {
    json!({
        "name": "clawed",
        "title": "Clawed Code Agent",
        "version": version.unwrap_or(env!("CARGO_PKG_VERSION")),
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
pub fn initialize_response(version: Option<&str>) -> Value {
    json!({
        "protocolVersion": 1,
        "agentCapabilities": agent_capabilities(),
        "agentInfo": agent_info(version),
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn version_override() { assert_eq!(agent_info(Some("2.0"))["version"], "2.0"); }
    #[test] fn caps_exist() { assert!(agent_capabilities().get("promptCapabilities").is_some()); }
    #[test] fn init_ok() { assert_eq!(initialize_response(Some("1"))["protocolVersion"], 1); }
    #[test] fn rpc_ok() { let r = rpc_result(&json!(1), json!({})); assert_eq!(r["jsonrpc"], "2.0"); }
    #[test] fn rpc_err() { let e = rpc_error(&json!("a"), -1, "err"); assert_eq!(e["error"]["code"], -1); }
    #[test] fn extract() { let b: Vec<Value> = serde_json::from_value(json!([{"type":"text","text":"hi"}])).unwrap(); assert_eq!(extract_text_from_content(&b), Some("hi\n".into())); }
    #[test] fn extract_empty() { assert_eq!(extract_text_from_content(&[]), None); }
}
