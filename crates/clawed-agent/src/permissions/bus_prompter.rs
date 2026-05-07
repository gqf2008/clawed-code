//! Bus-based permission prompter — routes permission requests through the
//! event bus so the ratatui TUI (or any other bus-connected UI) can handle
//! them instead of the raw terminal prompt.

use std::collections::HashMap;
use std::sync::Arc;

use clawed_bus::events::{
    PermissionRequest as BusPermissionRequest, PermissionResponse as BusPermissionResponse,
    RiskLevel,
};
use clawed_core::permissions::{PermissionResponse, PermissionSuggestion};
use tokio::sync::{broadcast, mpsc, oneshot, Mutex};
use uuid::Uuid;

use super::PermissionPrompter;

/// Permission prompter that broadcasts requests through the event bus and
/// waits for responses from the UI client.
///
/// A background task continuously drains the shared `mpsc` response channel
/// and routes each response to the correct in-flight request via a
/// per-request `oneshot` channel. This eliminates the need for a global
/// mutex and allows concurrent permission requests to proceed in parallel.
pub struct BusPermissionPrompter {
    req_tx: broadcast::Sender<BusPermissionRequest>,
    pending: Arc<Mutex<HashMap<String, oneshot::Sender<BusPermissionResponse>>>>,
}

impl BusPermissionPrompter {
    /// Create from the channels extracted from a [`BusHandle`].
    ///
    /// Call [`BusHandle::perm_req_sender()`] and [`BusHandle::take_perm_resp_rx()`]
    /// to obtain the arguments.
    ///
    /// This constructor spawns a background task that owns `resp_rx` and
    /// routes incoming responses to their matching request by `request_id`.
    pub fn new(
        req_tx: broadcast::Sender<BusPermissionRequest>,
        mut resp_rx: mpsc::Receiver<BusPermissionResponse>,
    ) -> Self {
        let pending: Arc<Mutex<HashMap<String, oneshot::Sender<BusPermissionResponse>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let pending_bg = Arc::clone(&pending);

        tokio::spawn(async move {
            while let Some(resp) = resp_rx.recv().await {
                let tx = {
                    let mut map = pending_bg.lock().await;
                    map.remove(&resp.request_id)
                };
                match tx {
                    Some(tx) => {
                        let _ = tx.send(resp);
                    }
                    None => {
                        tracing::warn!(
                            request_id = %resp.request_id,
                            "BusPermissionPrompter: response for unknown or timed-out request"
                        );
                    }
                }
            }
            tracing::debug!("BusPermissionPrompter response router task ended");
        });

        Self { req_tx, pending }
    }
}

#[async_trait::async_trait]
impl PermissionPrompter for BusPermissionPrompter {
    async fn ask_permission(
        &self,
        tool_name: &str,
        description: &str,
        _suggestions: &[PermissionSuggestion],
        input: &serde_json::Value,
    ) -> PermissionResponse {
        let request_id = Uuid::new_v4().to_string();
        let risk_level = classify_tool_risk(tool_name, input);
        let req = BusPermissionRequest {
            request_id: request_id.clone(),
            tool_name: tool_name.to_string(),
            input: input.clone(),
            risk_level,
            description: description.to_string(),
        };

        let (tx, rx) = oneshot::channel();
        {
            let mut map = self.pending.lock().await;
            map.insert(request_id.clone(), tx);
        }

        if self.req_tx.send(req).is_err() {
            let mut map = self.pending.lock().await;
            map.remove(&request_id);
            tracing::warn!("No UI client listening for permission requests; auto-denying");
            return PermissionResponse::deny();
        }

        let timeout = std::time::Duration::from_secs(300);
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(bus_resp)) => {
                if bus_resp.granted {
                    if bus_resp.remember {
                        // "Allow Always" → persist to project-local settings so the rule
                        // survives across sessions (written to .claude/settings.json).
                        clawed_core::permissions::PermissionResponse {
                            allowed: true,
                            persist: true,
                            feedback: None,
                            selected_suggestion: None,
                            destination: Some(
                                clawed_core::permissions::PermissionDestination::LocalSettings,
                            ),
                        }
                    } else {
                        PermissionResponse::allow_once()
                    }
                } else {
                    PermissionResponse::deny()
                }
            }
            Ok(Err(_)) => {
                tracing::warn!("Permission response oneshot dropped; auto-denying");
                PermissionResponse::deny()
            }
            Err(_) => {
                // Timeout: remove the stale pending entry so the background
                // router doesn't warn about it when/if a late response arrives.
                let mut map = self.pending.lock().await;
                map.remove(&request_id);
                tracing::warn!("Permission prompt timed out (bus); auto-denying");
                PermissionResponse::deny()
            }
        }
    }
}

fn classify_tool_risk(tool_name: &str, input: &serde_json::Value) -> RiskLevel {
    match tool_name {
        "Bash" | "PowerShell" => {
            if let Some(cmd) = input.get("command").and_then(|v| v.as_str()) {
                let classification = clawed_core::bash_classifier::classify(cmd);
                if classification.risk.always_ask() {
                    return RiskLevel::High;
                }
                if classification.risk.auto_approvable() {
                    return RiskLevel::Low;
                }
            }
            RiskLevel::Medium
        }
        "Write" | "MultiEdit" | "NotebookEdit" => RiskLevel::Medium,
        _ if tool_name.starts_with("mcp__") => RiskLevel::Medium,
        _ => RiskLevel::Low,
    }
}
