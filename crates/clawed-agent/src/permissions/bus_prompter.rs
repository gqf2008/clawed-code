//! Bus-based permission prompter — routes permission requests through the
//! event bus so the ratatui TUI (or any other bus-connected UI) can handle
//! them instead of the raw terminal prompt.

use clawed_bus::events::{
    PermissionRequest as BusPermissionRequest,
    PermissionResponse as BusPermissionResponse,
    RiskLevel,
};
use clawed_core::permissions::{PermissionResponse, PermissionSuggestion};
use tokio::sync::{broadcast, mpsc, Mutex};
use uuid::Uuid;

use super::PermissionPrompter;

/// Permission prompter that broadcasts requests through the event bus and
/// waits for responses from the UI client.
pub struct BusPermissionPrompter {
    req_tx: broadcast::Sender<BusPermissionRequest>,
    resp_rx: Mutex<mpsc::Receiver<BusPermissionResponse>>,
}

impl BusPermissionPrompter {
    /// Create from the channels extracted from a [`BusHandle`].
    ///
    /// Call [`BusHandle::perm_req_sender()`] and [`BusHandle::take_perm_resp_rx()`]
    /// to obtain the arguments.
    pub fn new(
        req_tx: broadcast::Sender<BusPermissionRequest>,
        resp_rx: mpsc::Receiver<BusPermissionResponse>,
    ) -> Self {
        Self {
            req_tx,
            resp_rx: Mutex::new(resp_rx),
        }
    }
}

#[async_trait::async_trait]
impl PermissionPrompter for BusPermissionPrompter {
    async fn ask_permission(
        &self,
        tool_name: &str,
        description: &str,
        _suggestions: &[PermissionSuggestion],
    ) -> PermissionResponse {
        let request_id = Uuid::new_v4().to_string();
        let req = BusPermissionRequest {
            request_id: request_id.clone(),
            tool_name: tool_name.to_string(),
            input: serde_json::Value::Null,
            risk_level: RiskLevel::Medium,
            description: description.to_string(),
        };

        // Broadcast to all UI clients
        if self.req_tx.send(req).is_err() {
            tracing::warn!("No UI client listening for permission requests; auto-denying");
            return PermissionResponse::deny();
        }

        // Wait for matching response (5 min timeout)
        let timeout = std::time::Duration::from_secs(300);
        let wait = async {
            let mut rx = self.resp_rx.lock().await;
            while let Some(resp) = rx.recv().await {
                if resp.request_id == request_id {
                    return Some(resp);
                }
                tracing::warn!(
                    "BusPermissionPrompter: response for unknown request_id: {}",
                    resp.request_id,
                );
            }
            None
        };

        match tokio::time::timeout(timeout, wait).await {
            Ok(Some(bus_resp)) => {
                // Convert bus PermissionResponse → core PermissionResponse
                if bus_resp.granted {
                    if bus_resp.remember {
                        PermissionResponse::allow_always()
                    } else {
                        PermissionResponse::allow_once()
                    }
                } else {
                    PermissionResponse::deny()
                }
            }
            Ok(None) => {
                tracing::warn!("Permission response channel closed; auto-denying");
                PermissionResponse::deny()
            }
            Err(_) => {
                tracing::warn!("Permission prompt timed out (bus); auto-denying");
                PermissionResponse::deny()
            }
        }
    }
}
