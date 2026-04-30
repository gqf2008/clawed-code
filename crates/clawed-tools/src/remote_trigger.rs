//! Remote Trigger Tool — manage scheduled remote Claude Code agents via the
//! claude.ai CCR API (`/v1/code/triggers`).
//!
//! This is the Rust port of the TypeScript `RemoteTriggerTool`.
//! Auth is handled in-process: the OAuth access token is loaded from the
//! existing `~/.claude/oauth_token.json` storage and never exposed to the shell.

use anyhow::Context;
use async_trait::async_trait;
use clawed_core::tool::{Tool, ToolCategory, ToolContext, ToolResult};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::PathBuf;
use tracing::{debug, warn};

const TRIGGERS_BETA: &str = "ccr-triggers-2026-01-30";

/// Minimal token stub for reading the local OAuth token file without depending on clawed-api.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct OAuthTokenStub {
    access_token: String,
}

/// Response from the `/v1/account` endpoint.
#[derive(Debug, Deserialize)]
struct AccountInfo {
    #[serde(default)]
    org_uuid: Option<String>,
}

pub struct RemoteTriggerTool;

/// Shared HTTP client for connection pooling across all RemoteTrigger requests.
static HTTP_CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();

fn shared_client() -> &'static reqwest::Client {
    HTTP_CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .build()
            .expect("reqwest Client builder failed")
    })
}

impl RemoteTriggerTool {
    fn base_api_url() -> String {
        // Default to the public Claude AI API; override via env for testing/enterprise.
        std::env::var("CLAUDE_API_BASE_URL").unwrap_or_else(|_| "https://api.claude.ai".to_string())
    }

    fn token_path() -> PathBuf {
        dirs::home_dir()
            .map(|h| h.join(".claude").join("oauth_token.json"))
            .unwrap_or_else(|| PathBuf::from("oauth_token.json"))
    }

    fn load_token_from_disk() -> anyhow::Result<Option<OAuthTokenStub>> {
        let path = Self::token_path();
        if !path.exists() {
            return Ok(None);
        }
        let contents = std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let token: OAuthTokenStub = serde_json::from_str(&contents)
            .with_context(|| format!("invalid JSON in {}", path.display()))?;
        Ok(Some(token))
    }

    fn get_access_token() -> anyhow::Result<String> {
        match Self::load_token_from_disk()? {
            Some(token) => Ok(token.access_token),
            None => Err(anyhow::anyhow!(
                "Not authenticated with a claude.ai account. Run /login and try again."
            )),
        }
    }

    /// Resolve the organization UUID.
    /// Priority: env var → cached successful lookup → fresh /account lookup.
    /// Only successful lookups are cached so transient failures can retry.
    async fn get_org_uuid(access_token: &str) -> Option<String> {
        // 1. Explicit env var (enterprise/CI)
        if let Ok(uuid) = std::env::var("CLAUDE_ORG_UUID") {
            return Some(uuid);
        }

        // 2. Cache only successful lookups
        static CACHED: std::sync::OnceLock<String> = std::sync::OnceLock::new();
        if let Some(cached) = CACHED.get() {
            return Some(cached.clone());
        }

        // 3. Query /v1/account
        let result = Self::fetch_org_uuid(access_token).await;
        if let Some(ref uuid) = result {
            let _ = CACHED.set(uuid.clone());
        }
        result
    }

    async fn fetch_org_uuid(access_token: &str) -> Option<String> {
        let base = Self::base_api_url();
        let url = format!("{}/v1/account", base);

        let send_future = shared_client()
            .get(&url)
            .header("Authorization", format!("Bearer {}", access_token))
            .header("Content-Type", "application/json")
            .send();

        let resp = tokio::time::timeout(std::time::Duration::from_secs(10), send_future)
            .await
            .ok()?
            .ok()?;

        if !resp.status().is_success() {
            debug!("/v1/account returned {}", resp.status());
            return None;
        }

        let info: AccountInfo = resp.json().await.ok()?;
        debug!("Resolved org UUID from /v1/account: {:?}", info.org_uuid);
        info.org_uuid
    }
}

#[async_trait]
impl Tool for RemoteTriggerTool {
    fn name(&self) -> &str {
        "RemoteTrigger"
    }

    fn description(&self) -> &str {
        "Manage scheduled remote Claude Code agents (triggers) via the claude.ai CCR API. \
         Auth is handled in-process — the token never reaches the shell."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["list", "get", "create", "update", "run"],
                    "description": "API action to perform"
                },
                "trigger_id": {
                    "type": "string",
                    "pattern": r"^[\w-]+$",
                    "description": "Required for get, update, and run"
                },
                "body": {
                    "type": "object",
                    "description": "JSON body for create and update (partial update supported)"
                }
            },
            "required": ["action"]
        })
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Session
    }

    fn is_read_only(&self) -> bool {
        // Determined per-call in check_permissions
        false
    }

    fn is_concurrency_safe(&self) -> bool {
        true
    }

    fn is_enabled(&self) -> bool {
        static CACHED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *CACHED.get_or_init(|| {
            std::env::var("CLAWED_ENABLE_REMOTE_TRIGGER").is_ok()
                || Self::load_token_from_disk().ok().flatten().is_some()
        })
    }

    async fn call(&self, input: Value, _context: &ToolContext) -> anyhow::Result<ToolResult> {
        let action = input
            .get("action")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing required field: action"))?;

        let trigger_id = input.get("trigger_id").and_then(|v| v.as_str());
        let body = input.get("body");

        let access_token = Self::get_access_token()?;
        let org_uuid = Self::get_org_uuid(&access_token).await;

        let base = format!("{}/v1/code/triggers", Self::base_api_url());

        let client = shared_client();

        let mut req_builder = match action {
            "list" => client.get(&base),
            "get" => {
                let id = trigger_id.ok_or_else(|| anyhow::anyhow!("get requires trigger_id"))?;
                client.get(format!("{}/{}", base, id))
            }
            "create" => {
                let body = body.ok_or_else(|| anyhow::anyhow!("create requires body"))?;
                client.post(&base).json(body)
            }
            "update" => {
                let id = trigger_id.ok_or_else(|| anyhow::anyhow!("update requires trigger_id"))?;
                let body = body.ok_or_else(|| anyhow::anyhow!("update requires body"))?;
                client.post(format!("{}/{}", base, id)).json(body)
            }
            "run" => {
                let id = trigger_id.ok_or_else(|| anyhow::anyhow!("run requires trigger_id"))?;
                client.post(format!("{}/{}/run", base, id)).json(&json!({}))
            }
            other => return Err(anyhow::anyhow!("Unknown action: {}", other)),
        };

        req_builder = req_builder
            .header("Authorization", format!("Bearer {}", access_token))
            .header("Content-Type", "application/json")
            .header("anthropic-version", "2023-06-01")
            .header("anthropic-beta", TRIGGERS_BETA);

        if let Some(ref uuid) = org_uuid {
            req_builder = req_builder.header("x-organization-uuid", uuid);
        } else {
            warn!("No organization UUID available — set CLAUDE_ORG_UUID or authenticate with an org-linked account");
        }

        debug!("RemoteTrigger {} request to {}", action, base);
        let resp = req_builder.send().await?;
        let status = resp.status().as_u16();
        let json_text = resp.text().await.unwrap_or_else(|_| "{}".to_string());

        let result_text = format!("HTTP {}\n{}", status, json_text);
        if status >= 400 {
            Ok(ToolResult::error(result_text))
        } else {
            Ok(ToolResult::text(result_text))
        }
    }
}
