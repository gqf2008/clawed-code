//! Bus-based user question prompter — routes AskUser through the event bus
//! so the ratatui TUI (or any other bus-connected UI) can handle input.

use std::collections::HashMap;
use std::sync::Arc;

use clawed_bus::events::{UserQuestionRequest, UserQuestionResponse};
use tokio::sync::{broadcast, mpsc, oneshot, Mutex};
use uuid::Uuid;

use super::UserPrompter;

/// User prompter that broadcasts requests through the event bus and waits
/// for responses from the UI client.
///
/// A background task continuously drains the shared `mpsc` response channel
/// and routes each response to the correct in-flight request via a
/// per-request `oneshot` channel.
pub struct BusUserPrompter {
    req_tx: broadcast::Sender<UserQuestionRequest>,
    pending: Arc<Mutex<HashMap<String, oneshot::Sender<UserQuestionResponse>>>>,
}

impl BusUserPrompter {
    /// Create from the channels extracted from a [`BusHandle`].
    ///
    /// Call [`BusHandle::user_q_req_sender()`] and [`BusHandle::take_user_q_resp_rx()`]
    /// to obtain the arguments.
    ///
    /// This constructor spawns a background task that owns `resp_rx` and
    /// routes incoming responses to their matching request by `request_id`.
    pub fn new(
        req_tx: broadcast::Sender<UserQuestionRequest>,
        mut resp_rx: mpsc::Receiver<UserQuestionResponse>,
    ) -> Self {
        let pending: Arc<Mutex<HashMap<String, oneshot::Sender<UserQuestionResponse>>>> =
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
                            "BusUserPrompter: response for unknown or timed-out request"
                        );
                    }
                }
            }
            tracing::debug!("BusUserPrompter response router task ended");
        });

        Self { req_tx, pending }
    }
}

#[async_trait::async_trait]
impl UserPrompter for BusUserPrompter {
    async fn ask_user(&self, question: &str) -> anyhow::Result<String> {
        let request_id = Uuid::new_v4().to_string();
        let req = UserQuestionRequest {
            request_id: request_id.clone(),
            question: question.to_string(),
        };

        let (tx, rx) = oneshot::channel();
        {
            let mut map = self.pending.lock().await;
            map.insert(request_id.clone(), tx);
        }

        if self.req_tx.send(req).is_err() {
            let mut map = self.pending.lock().await;
            map.remove(&request_id);
            return Err(anyhow::anyhow!("No UI client listening for user questions"));
        }

        let timeout = std::time::Duration::from_secs(300);
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(resp)) => {
                if resp.cancelled {
                    Err(anyhow::anyhow!("User cancelled the question"))
                } else {
                    Ok(resp.answer)
                }
            }
            Ok(Err(_)) => Err(anyhow::anyhow!("User question response oneshot dropped")),
            Err(_) => {
                let mut map = self.pending.lock().await;
                map.remove(&request_id);
                Err(anyhow::anyhow!("User question timed out (5 minutes)"))
            }
        }
    }
}
