//! Event bus — typed channels connecting Agent Core to UI clients.
//!
//! The bus provides two halves:
//! - [`BusHandle`] — held by the Agent Core, sends notifications, receives requests
//! - [`ClientHandle`] — held by UI clients, receives notifications, sends requests
//!
//! ## Channel topology
//!
//! ```text
//! AgentNotification:  Core ──broadcast──→ Client(s)  (1:N, lossy on slow receivers)
//! AgentRequest:       Client ──mpsc────→ Core        (N:1, backpressure via bounded)
//! PermissionRequest:  Core ──broadcast──→ Client(s)  (1:N, first responder wins)
//! PermissionResponse: Client ──mpsc────→ Core        (1:1, paired with request)
//! ```
//!
//! ## Features
//!
//! - **Diagnostics**: [`BusDiagnostics`] exposes subscriber counts, message
//!   counters, and history buffer stats for monitoring and debugging.
//! - **RAII Subscriptions**: [`NotificationSubscription`] wraps a broadcast
//!   receiver with optional filtering and auto-cleanup on drop.
//! - **Event History**: Recent notifications are kept in a bounded ring buffer
//!   so late-joining clients can replay missed state.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use tokio::sync::{broadcast, mpsc, watch};
use uuid::Uuid;

use crate::events::{AgentNotification, AgentRequest, ImageAttachment, PermissionRequest, PermissionResponse, RiskLevel};
#[cfg(test)]
use crate::events::ErrorCode;

// ── Lock helper ──────────────────────────────────────────────────────────────

/// Lock a `std::sync::Mutex`, recovering from poisoning.
///
/// This is the bus-local equivalent of `lock_or_recover` in clawed-core.
/// We define it here to avoid a circular dependency.
fn lock_or_recover<T>(mutex: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

// ── Shared state ─────────────────────────────────────────────────────────────

/// Shared state accessible from both [`BusHandle`] and [`ClientHandle`].
///
/// Contains atomic counters for diagnostics and a bounded ring buffer
/// for notification history (replay for late-joining clients).
struct BusShared {
    /// Total notifications broadcast since bus creation.
    notifications_sent: AtomicU64,
    /// Total requests sent by clients since bus creation.
    requests_sent: AtomicU64,
    /// Circular buffer of recent notifications for replay.
    history: std::sync::Mutex<VecDeque<AgentNotification>>,
    /// Maximum number of notifications kept in history.
    history_capacity: usize,
}

impl BusShared {
    fn new(history_capacity: usize) -> Self {
        Self {
            notifications_sent: AtomicU64::new(0),
            requests_sent: AtomicU64::new(0),
            history: std::sync::Mutex::new(VecDeque::with_capacity(history_capacity)),
            history_capacity,
        }
    }

    fn record_notification(&self, event: &AgentNotification) {
        self.notifications_sent.fetch_add(1, Ordering::Relaxed);
        if self.history_capacity == 0 {
            return;
        }
        let mut history = lock_or_recover(&self.history);
        if history.len() >= self.history_capacity {
            history.pop_front();
        }
        history.push_back(event.clone());
    }

    fn record_request(&self) {
        self.requests_sent.fetch_add(1, Ordering::Relaxed);
    }

    fn recent_history(&self) -> Vec<AgentNotification> {
        lock_or_recover(&self.history).iter().cloned().collect()
    }

    fn history_len(&self) -> usize {
        lock_or_recover(&self.history).len()
    }
}

// ── BusDiagnostics ───────────────────────────────────────────────────────────

/// Snapshot of bus diagnostics at a point in time.
///
/// Obtain via [`BusHandle::diagnostics`] or [`ClientHandle::diagnostics`].
///
/// # Example
///
/// ```rust,ignore
/// let diag = bus.diagnostics();
/// println!("sent {} notifications to {} subscribers",
///     diag.notifications_sent, diag.notification_subscribers);
/// ```
#[derive(Debug, Clone)]
pub struct BusDiagnostics {
    /// Total notifications broadcast since bus creation.
    pub notifications_sent: u64,
    /// Total requests sent by clients since bus creation.
    pub requests_sent: u64,
    /// Current number of active notification subscribers.
    pub notification_subscribers: usize,
    /// Number of notifications currently in the history buffer.
    pub history_len: usize,
    /// Maximum history buffer capacity.
    pub history_capacity: usize,
}

// ── NotificationSubscription ─────────────────────────────────────────────────

/// Type alias for the notification filter predicate to reduce type complexity.
type NotificationFilter = Box<dyn Fn(&AgentNotification) -> bool + Send + Sync>;

/// RAII subscription for agent notifications with optional filtering.
///
/// Created via [`ClientHandle::subscribe`] or [`ClientHandle::subscribe_filtered`].
/// Automatically unsubscribes from the broadcast channel when dropped.
///
/// # Example
///
/// ```rust,ignore
/// // Subscribe to only tool-related notifications
/// let mut sub = client.subscribe().tools_only();
/// while let Some(event) = sub.recv().await {
///     println!("Tool event: {event:?}");
/// }
/// // Automatically unsubscribed when `sub` is dropped
/// ```
pub struct NotificationSubscription {
    rx: broadcast::Receiver<AgentNotification>,
    core_alive_rx: watch::Receiver<bool>,
    filter: Option<NotificationFilter>,
}

impl NotificationSubscription {
    /// Set a filter predicate. Only matching notifications will be yielded.
    #[must_use]
    pub fn with_filter<F>(mut self, filter: F) -> Self
    where
        F: Fn(&AgentNotification) -> bool + Send + Sync + 'static,
    {
        self.filter = Some(Box::new(filter));
        self
    }

    /// Filter to only tool-lifecycle notifications
    /// (`ToolUseStart`, `ToolUseReady`, `ToolUseComplete`, `ToolSelected`).
    #[must_use]
    pub fn tools_only(self) -> Self {
        self.with_filter(|n| {
            matches!(
                n,
                AgentNotification::ToolUseStart { .. }
                    | AgentNotification::ToolUseReady { .. }
                    | AgentNotification::ToolUseComplete { .. }
                    | AgentNotification::ToolSelected { .. }
            )
        })
    }

    /// Filter to only sub-agent lifecycle notifications
    /// (`AgentSpawned`, `AgentProgress`, `AgentComplete`, `AgentTerminated`).
    #[must_use]
    pub fn agents_only(self) -> Self {
        self.with_filter(|n| {
            matches!(
                n,
                AgentNotification::AgentSpawned { .. }
                    | AgentNotification::AgentProgress { .. }
                    | AgentNotification::AgentComplete { .. }
                    | AgentNotification::AgentTerminated { .. }
            )
        })
    }

    /// Filter to only streaming content (`TextDelta`, `ThinkingDelta`).
    #[must_use]
    pub fn content_only(self) -> Self {
        self.with_filter(|n| {
            matches!(
                n,
                AgentNotification::TextDelta { .. } | AgentNotification::ThinkingDelta { .. }
            )
        })
    }

    /// Filter to only error notifications.
    #[must_use]
    pub fn errors_only(self) -> Self {
        self.with_filter(|n| matches!(n, AgentNotification::Error { .. }))
    }

    /// Receive the next notification matching the filter.
    ///
    /// Returns `None` when the core is disconnected.
    /// Skips non-matching notifications and handles `Lagged` transparently.
    pub async fn recv(&mut self) -> Option<AgentNotification> {
        loop {
            tokio::select! {
                result = self.rx.recv() => {
                    match result {
                        Ok(event) => {
                            if let Some(ref filter) = self.filter {
                                if !filter(&event) {
                                    continue;
                                }
                            }
                            return Some(event);
                        }
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            tracing::warn!("Subscription lagged by {n} notifications, catching up");
                            continue;
                        }
                        Err(broadcast::error::RecvError::Closed) => return None,
                    }
                }
                result = self.core_alive_rx.changed() => {
                    if result.is_err() || !*self.core_alive_rx.borrow() {
                        return None;
                    }
                }
            }
        }
    }

    /// Try to receive the next matching notification without blocking.
    ///
    /// Returns `Some(event)` if a matching notification is available,
    /// `None` otherwise.
    pub fn try_recv(&mut self) -> Option<AgentNotification> {
        loop {
            match self.rx.try_recv() {
                Ok(event) => {
                    if let Some(ref filter) = self.filter {
                        if !filter(&event) {
                            continue;
                        }
                    }
                    return Some(event);
                }
                Err(_) => return None,
            }
        }
    }
}

// ── EventBus ─────────────────────────────────────────────────────────────────

/// The event bus factory. Call [`EventBus::new`] to create a paired
/// `(BusHandle, ClientHandle)`.
pub struct EventBus;

impl EventBus {
    /// Default history buffer capacity (number of recent notifications kept).
    const DEFAULT_HISTORY_CAPACITY: usize = 512;

    /// Create a new event bus with the given broadcast channel capacity.
    ///
    /// `capacity` controls the broadcast buffer for notifications.
    /// Slow receivers that fall behind by more than `capacity` messages
    /// will miss intermediate events (they get a `Lagged` error and can
    /// continue from the latest).
    ///
    /// Uses a default history capacity of 512 notifications.
    ///
    /// Returns `(core_handle, client_handle)`.
    #[allow(clippy::new_ret_no_self)]
    #[must_use]
    pub fn new(capacity: usize) -> (BusHandle, ClientHandle) {
        Self::with_history(capacity, Self::DEFAULT_HISTORY_CAPACITY)
    }

    /// Create a new event bus with explicit history buffer capacity.
    ///
    /// `history_capacity` controls how many recent notifications are kept
    /// for replay by late-joining clients. Set to 0 to disable history.
    #[allow(clippy::new_ret_no_self)]
    #[must_use]
    pub fn with_history(capacity: usize, history_capacity: usize) -> (BusHandle, ClientHandle) {
        /// Maximum queued requests before backpressure.
        const REQUEST_QUEUE_CAP: usize = 1024;
        /// Maximum queued permission responses before backpressure.
        const PERM_RESP_QUEUE_CAP: usize = 256;
        /// Minimum capacity for critical event channels (permission requests).
        const MIN_CRITICAL_CAP: usize = 256;

        let (notify_tx, notify_rx) = broadcast::channel(capacity);
        let (request_tx, request_rx) = mpsc::channel(REQUEST_QUEUE_CAP);
        let (perm_req_tx, perm_req_rx) = broadcast::channel(capacity.max(MIN_CRITICAL_CAP));
        let (perm_resp_tx, perm_resp_rx) = mpsc::channel(PERM_RESP_QUEUE_CAP);
        let (core_alive_tx, core_alive_rx) = watch::channel(true);
        let shared = Arc::new(BusShared::new(history_capacity));

        let bus = BusHandle {
            notify_tx: notify_tx.clone(),
            request_rx,
            request_tx: request_tx.clone(),
            perm_req_tx: perm_req_tx.clone(),
            perm_resp_rx: Some(perm_resp_rx),
            _perm_resp_tx: perm_resp_tx.clone(),
            core_alive_tx,
            shared: Arc::clone(&shared),
        };

        let client = ClientHandle {
            notify_rx,
            _notify_tx: notify_tx,
            request_tx,
            perm_req_rx,
            _perm_req_tx: perm_req_tx,
            perm_resp_tx: Some(perm_resp_tx),
            core_alive_rx,
            shared,
        };

        (bus, client)
    }
}

// ── BusHandle (Agent Core side) ──────────────────────────────────────────────

/// Handle held by the Agent Core. Provides:
/// - Send notifications to all subscribers
/// - Receive requests from UI clients
/// - Send permission requests and receive responses
/// - Create new client handles
/// - Query diagnostics and history
pub struct BusHandle {
    notify_tx: broadcast::Sender<AgentNotification>,
    request_rx: mpsc::Receiver<AgentRequest>,
    request_tx: mpsc::Sender<AgentRequest>,
    perm_req_tx: broadcast::Sender<PermissionRequest>,
    perm_resp_rx: Option<mpsc::Receiver<PermissionResponse>>,
    /// Kept alive to prevent the mpsc channel from closing.
    /// Only the primary client gets a clone (secondary clients cannot respond).
    _perm_resp_tx: mpsc::Sender<PermissionResponse>,
    /// Signals `false` on drop so clients detect core disconnection.
    core_alive_tx: watch::Sender<bool>,
    /// Shared diagnostics and history state.
    shared: Arc<BusShared>,
}

impl Drop for BusHandle {
    fn drop(&mut self) {
        let _ = self.core_alive_tx.send(false);
    }
}

impl BusHandle {
    /// Broadcast a notification to all subscribed clients.
    ///
    /// Also records the notification in the history buffer for replay
    /// and increments the diagnostics counter.
    ///
    /// Returns the number of receivers that got the message.
    /// Returns 0 if no clients are listening (this is not an error).
    pub fn notify(&self, event: AgentNotification) -> usize {
        self.shared.record_notification(&event);
        self.notify_tx.send(event).unwrap_or(0)
    }

    /// Receive the next request from a UI client.
    ///
    /// Returns `None` when all client handles have been dropped.
    pub async fn recv_request(&mut self) -> Option<AgentRequest> {
        self.request_rx.recv().await
    }

    /// Try to receive a request without blocking.
    pub fn try_recv_request(&mut self) -> Option<AgentRequest> {
        self.request_rx.try_recv().ok()
    }

    /// Send a permission request to the UI and wait for a response.
    ///
    /// This is the primary mechanism for tool permission checks in the
    /// decoupled architecture. The core sends a request describing what
    /// the tool wants to do, and blocks until the UI responds.
    ///
    /// Returns `None` if the UI is disconnected or no client responds
    /// within the timeout (default 30s). Callers should treat `None`
    /// as denial.
    pub async fn request_permission(
        &mut self,
        tool_name: &str,
        input: serde_json::Value,
        risk_level: RiskLevel,
        description: &str,
    ) -> Option<PermissionResponse> {
        self.request_permission_with_timeout(
            tool_name,
            input,
            risk_level,
            description,
            std::time::Duration::from_secs(30),
        )
        .await
    }

    /// Like [`request_permission`](Self::request_permission) but with a
    /// custom timeout. Useful for non-interactive clients (RPC, Bridge)
    /// that may not have a permission UI.
    pub async fn request_permission_with_timeout(
        &mut self,
        tool_name: &str,
        input: serde_json::Value,
        risk_level: RiskLevel,
        description: &str,
        timeout: std::time::Duration,
    ) -> Option<PermissionResponse> {
        let perm_resp_rx = match self.perm_resp_rx.as_mut() {
            Some(rx) => rx,
            None => {
                tracing::warn!("Permission response channel was taken; cannot request_permission via BusHandle");
                return None;
            }
        };
        let request_id = Uuid::new_v4().to_string();
        let req = PermissionRequest {
            request_id: request_id.clone(),
            tool_name: tool_name.to_string(),
            input,
            risk_level,
            description: description.to_string(),
        };

        // Send request to UI
        if self.perm_req_tx.send(req).is_err() {
            return None; // UI disconnected
        }

        // Wait for matching response with timeout.
        // Without a timeout, non-interactive clients (Bridge, RPC without
        // permission handler) would hang forever.
        let wait = async {
            while let Some(resp) = perm_resp_rx.recv().await {
                if resp.request_id == request_id {
                    return Some(resp);
                }
                tracing::warn!(
                    "Received permission response for unknown request: {}",
                    resp.request_id
                );
            }
            None // Channel closed
        };

        if let Ok(result) = tokio::time::timeout(timeout, wait).await { result } else {
            tracing::warn!(
                "Permission request timed out for tool '{}' after {:?}, auto-denying",
                tool_name,
                timeout,
            );
            None
        }
    }

    /// Get the notification sender (for cloning to sub-agents).
    #[must_use] 
    pub fn notify_sender(&self) -> broadcast::Sender<AgentNotification> {
        self.notify_tx.clone()
    }

    /// Clone the permission request sender.
    ///
    /// Used by [`BusPermissionPrompter`] to broadcast permission requests to
    /// UI clients without owning the full `BusHandle`.
    #[must_use]
    pub fn perm_req_sender(&self) -> broadcast::Sender<PermissionRequest> {
        self.perm_req_tx.clone()
    }

    /// Take the permission response receiver out of this handle.
    ///
    /// After calling this, [`request_permission`](Self::request_permission) and
    /// [`request_permission_with_timeout`](Self::request_permission_with_timeout)
    /// will return `None` immediately. Use this when an external component (e.g.
    /// [`BusPermissionPrompter`]) handles the response channel instead.
    pub fn take_perm_resp_rx(&mut self) -> Option<mpsc::Receiver<PermissionResponse>> {
        self.perm_resp_rx.take()
    }

    /// Create a new `ClientHandle` connected to this bus.
    ///
    /// Multiple clients can coexist — all receive notifications (broadcast),
    /// and all share the same request channel (mpsc to core).
    /// Permission requests are broadcast to all clients for display purposes,
    /// but only the primary client can respond (secondary clients have no
    /// permission response channel to prevent spoofing).
    #[must_use] 
    pub fn new_client(&self) -> ClientHandle {
        ClientHandle {
            notify_rx: self.notify_tx.subscribe(),
            _notify_tx: self.notify_tx.clone(),
            request_tx: self.request_tx.clone(),
            perm_req_rx: self.perm_req_tx.subscribe(),
            _perm_req_tx: self.perm_req_tx.clone(),
            perm_resp_tx: None, // secondary clients cannot respond to permissions
            core_alive_rx: self.core_alive_tx.subscribe(),
            shared: Arc::clone(&self.shared),
        }
    }

    /// Get a snapshot of bus diagnostics (counters, subscriber count, history).
    #[must_use]
    pub fn diagnostics(&self) -> BusDiagnostics {
        BusDiagnostics {
            notifications_sent: self.shared.notifications_sent.load(Ordering::Relaxed),
            requests_sent: self.shared.requests_sent.load(Ordering::Relaxed),
            notification_subscribers: self.notify_tx.receiver_count(),
            history_len: self.shared.history_len(),
            history_capacity: self.shared.history_capacity,
        }
    }

    /// Get a copy of the recent notification history.
    ///
    /// Returns up to `history_capacity` most recent notifications, oldest first.
    /// Useful for replaying state to late-joining clients.
    #[must_use]
    pub fn recent_history(&self) -> Vec<AgentNotification> {
        self.shared.recent_history()
    }

    /// Create a dummy request receiver (for testing only).
    ///
    /// **Note:** `mpsc` channels are point-to-point — the actual request stream
    /// is consumed by `recv_request()` on this `BusHandle`. This method returns
    /// an independent channel that will never receive production requests.
    /// It exists solely for integration tests that need a `Receiver<AgentRequest>`.
    #[cfg(any(test, feature = "test-utils"))]
    #[must_use] 
    pub fn subscribe_requests(&self) -> mpsc::Receiver<AgentRequest> {
        let (_tx, rx) = mpsc::channel(1);
        rx
    }
}

// ── ClientHandle (UI side) ───────────────────────────────────────────────────

/// Handle held by UI clients (REPL, IDE, Web). Provides:
/// - Receive notifications from the Agent Core
/// - Send requests to the Agent Core
/// - Receive permission requests and send responses
/// - Create filtered subscriptions
/// - Query diagnostics and replay history
pub struct ClientHandle {
    notify_rx: broadcast::Receiver<AgentNotification>,
    /// Keep the sender alive so `subscribe_notifications()` can create new receivers.
    _notify_tx: broadcast::Sender<AgentNotification>,
    request_tx: mpsc::Sender<AgentRequest>,
    perm_req_rx: broadcast::Receiver<PermissionRequest>,
    /// Keep the sender alive so `perm_req_rx` doesn't get `Closed`.
    _perm_req_tx: broadcast::Sender<PermissionRequest>,
    /// Permission response channel. Only the primary client can respond;
    /// secondary clients (via `new_client()`) have `None` to prevent spoofing.
    perm_resp_tx: Option<mpsc::Sender<PermissionResponse>>,
    /// Watch receiver for core alive status. Returns None when core drops.
    core_alive_rx: watch::Receiver<bool>,
    /// Shared diagnostics and history state.
    shared: Arc<BusShared>,
}

impl ClientHandle {
    /// Receive the next notification from the Agent Core.
    ///
    /// If the client falls behind, intermediate messages are skipped
    /// (`broadcast::Lagged`) and this returns the next available message.
    /// Returns `None` when the core is disconnected (BusHandle dropped).
    pub async fn recv_notification(&mut self) -> Option<AgentNotification> {
        loop {
            tokio::select! {
                result = self.notify_rx.recv() => {
                    match result {
                        Ok(event) => return Some(event),
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            tracing::warn!("Client lagged by {} notifications, catching up", n);
                            continue;
                        }
                        Err(broadcast::error::RecvError::Closed) => return None,
                    }
                }
                result = self.core_alive_rx.changed() => {
                    if result.is_err() || !*self.core_alive_rx.borrow() {
                        return None; // Core dropped
                    }
                }
            }
        }
    }

    /// Send a request to the Agent Core.
    pub fn send_request(&self, request: AgentRequest) -> Result<(), SendError> {
        self.shared.record_request();
        self.request_tx
            .try_send(request)
            .map_err(|_| SendError::DISCONNECTED)
    }

    /// Receive the next permission request from the Agent Core.
    ///
    /// All clients receive permission requests (broadcast). Only the first
    /// client to respond with a matching `request_id` wins.
    /// Returns `None` when the core is disconnected.
    pub async fn recv_permission_request(&mut self) -> Option<PermissionRequest> {
        loop {
            tokio::select! {
                result = self.perm_req_rx.recv() => {
                    match result {
                        Ok(req) => return Some(req),
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            tracing::warn!("Client lagged by {} permission requests, catching up", n);
                            continue;
                        }
                        Err(broadcast::error::RecvError::Closed) => return None,
                    }
                }
                result = self.core_alive_rx.changed() => {
                    if result.is_err() || !*self.core_alive_rx.borrow() {
                        return None;
                    }
                }
            }
        }
    }

    /// Respond to a permission request.
    ///
    /// Only the primary client can respond; secondary clients return `Err`.
    pub fn send_permission_response(&self, response: PermissionResponse) -> Result<(), SendError> {
        match &self.perm_resp_tx {
            Some(tx) => tx.try_send(response).map_err(|_| SendError::DISCONNECTED),
            None => Err(SendError::DISCONNECTED), // secondary client — not authorized
        }
    }

    /// Convenience: submit a user message.
    pub fn submit(&self, text: impl Into<String>) -> Result<(), SendError> {
        self.send_request(AgentRequest::Submit {
            text: text.into(),
            images: vec![],
        })
    }

    /// Submit user input with image attachments.
    pub fn submit_with_images(
        &self,
        text: impl Into<String>,
        images: Vec<ImageAttachment>,
    ) -> Result<(), SendError> {
        self.send_request(AgentRequest::Submit {
            text: text.into(),
            images,
        })
    }

    /// Convenience: send abort signal.
    pub fn abort(&self) -> Result<(), SendError> {
        self.send_request(AgentRequest::Abort)
    }

    /// Convenience: send shutdown signal.
    pub fn shutdown(&self) -> Result<(), SendError> {
        self.send_request(AgentRequest::Shutdown)
    }

    /// Try to receive the next notification without blocking.
    ///
    /// Returns `Ok(Some(event))` if a message is available,
    /// `Ok(None)` if no messages are pending,
    /// or `Err(())` if the receiver fell behind (messages were skipped).
    pub fn try_recv_notification(
        &mut self,
    ) -> Result<Option<AgentNotification>, ()> {
        match self.notify_rx.try_recv() {
            Ok(event) => Ok(Some(event)),
            Err(broadcast::error::TryRecvError::Lagged(_)) => Err(()),
            Err(broadcast::error::TryRecvError::Empty | broadcast::error::TryRecvError::Closed) => Ok(None),
        }
    }

    /// Create an additional notification subscriber.
    ///
    /// Useful for spawning multiple consumers (e.g., one for display,
    /// one for logging).
    #[must_use] 
    pub fn subscribe_notifications(&self) -> broadcast::Receiver<AgentNotification> {
        self._notify_tx.subscribe()
    }

    /// Create an additional permission-request subscriber.
    ///
    /// Useful for spawning a dedicated permission-handling task alongside the
    /// main notification consumer.
    #[must_use]
    pub fn subscribe_permission_requests(&self) -> broadcast::Receiver<PermissionRequest> {
        self._perm_req_tx.subscribe()
    }

    /// Create a [`NotificationSubscription`] with RAII lifecycle and
    /// optional filtering.
    ///
    /// The subscription automatically unsubscribes when dropped.
    /// Chain filter methods for selective reception:
    ///
    /// ```rust,ignore
    /// let mut sub = client.subscribe().tools_only();
    /// while let Some(event) = sub.recv().await { /* ... */ }
    /// ```
    #[must_use]
    pub fn subscribe(&self) -> NotificationSubscription {
        NotificationSubscription {
            rx: self._notify_tx.subscribe(),
            core_alive_rx: self.core_alive_rx.clone(),
            filter: None,
        }
    }

    /// Check whether the Agent Core is still alive.
    #[must_use]
    pub fn is_core_alive(&self) -> bool {
        *self.core_alive_rx.borrow()
    }

    /// Get a snapshot of bus diagnostics.
    #[must_use]
    pub fn diagnostics(&self) -> BusDiagnostics {
        BusDiagnostics {
            notifications_sent: self.shared.notifications_sent.load(Ordering::Relaxed),
            requests_sent: self.shared.requests_sent.load(Ordering::Relaxed),
            notification_subscribers: self._notify_tx.receiver_count(),
            history_len: self.shared.history_len(),
            history_capacity: self.shared.history_capacity,
        }
    }

    /// Get a copy of the recent notification history.
    ///
    /// Returns up to `history_capacity` most recent notifications, oldest first.
    /// Useful for catching up on state after a late join or reconnection.
    #[must_use]
    pub fn recent_history(&self) -> Vec<AgentNotification> {
        self.shared.recent_history()
    }
}

// ── Errors ───────────────────────────────────────────────────────────────────

/// Error when sending to a disconnected bus.
#[derive(Debug, Clone, thiserror::Error)]
#[error("Bus disconnected: the other end has been dropped")]
pub struct SendError;

impl SendError {
    /// Sentinel value for when the bus is disconnected.
    pub const DISCONNECTED: Self = Self;
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn basic_notification_flow() {
        let (bus, mut client) = EventBus::new(16);

        bus.notify(AgentNotification::TextDelta {
            text: "Hello".into(),
        });
        bus.notify(AgentNotification::TextDelta {
            text: " world".into(),
        });

        let e1 = client.recv_notification().await.unwrap();
        let e2 = client.recv_notification().await.unwrap();

        match e1 {
            AgentNotification::TextDelta { text } => assert_eq!(text, "Hello"),
            other => panic!("Expected TextDelta, got {other:?}"),
        }
        match e2 {
            AgentNotification::TextDelta { text } => assert_eq!(text, " world"),
            other => panic!("Expected TextDelta, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn request_flow() {
        let (mut bus, client) = EventBus::new(16);

        client
            .send_request(AgentRequest::Submit {
                text: "Fix bug".into(),
                images: vec![],
            })
            .unwrap();

        let req = bus.recv_request().await.unwrap();
        match req {
            AgentRequest::Submit { text, .. } => assert_eq!(text, "Fix bug"),
            other => panic!("Expected Submit, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn permission_request_response() {
        let (mut bus, mut client) = EventBus::new(16);

        // Spawn the core side: send permission request, wait for response
        let core = tokio::spawn(async move {
            let resp = bus
                .request_permission("Bash", serde_json::json!({"cmd": "ls"}), RiskLevel::Low, "List files")
                .await
                .unwrap();
            assert!(resp.granted);
            assert!(resp.remember);
        });

        // UI side: receive permission request, send response
        let perm = client.recv_permission_request().await.unwrap();
        assert_eq!(perm.tool_name, "Bash");
        assert_eq!(perm.risk_level, RiskLevel::Low);

        client
            .send_permission_response(PermissionResponse {
                request_id: perm.request_id,
                granted: true,
                remember: true,
            })
            .unwrap();

        core.await.unwrap();
    }

    #[tokio::test]
    async fn abort_signal() {
        let (mut bus, client) = EventBus::new(16);

        client.abort().unwrap();

        let req = bus.recv_request().await.unwrap();
        assert!(matches!(req, AgentRequest::Abort));
    }

    #[tokio::test]
    async fn shutdown_signal() {
        let (mut bus, client) = EventBus::new(16);

        client.shutdown().unwrap();

        let req = bus.recv_request().await.unwrap();
        assert!(matches!(req, AgentRequest::Shutdown));
    }

    #[tokio::test]
    async fn multiple_subscribers() {
        let (bus, client) = EventBus::new(16);

        // Create a second subscriber
        let mut sub2 = client.subscribe_notifications();

        bus.notify(AgentNotification::SessionStart {
            session_id: "s1".into(),
            model: "sonnet".into(),
        });

        // Both should receive the event
        // We need to use the second subscriber directly since client's notify_rx
        // also gets the event
        let e2 = sub2.recv().await.unwrap();
        match e2 {
            AgentNotification::SessionStart { session_id, .. } => {
                assert_eq!(session_id, "s1");
            }
            other => panic!("Expected SessionStart, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn disconnected_send_error() {
        let (_bus, client) = EventBus::new(16);

        // Drop the bus handle (core disconnected)
        drop(_bus);

        // Request should fail with SendError — but mpsc channels only error
        // when the receiver is dropped. Since we dropped bus (which owns request_rx),
        // sending should fail.
        let result = client.send_request(AgentRequest::Abort);
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn submit_convenience() {
        let (mut bus, client) = EventBus::new(16);

        client.submit("Hello there").unwrap();

        let req = bus.recv_request().await.unwrap();
        match req {
            AgentRequest::Submit { text, images } => {
                assert_eq!(text, "Hello there");
                assert!(images.is_empty());
            }
            other => panic!("Expected Submit, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn try_recv_empty() {
        let (mut bus, _client) = EventBus::new(16);

        // No requests pending
        assert!(bus.try_recv_request().is_none());
    }

    #[tokio::test]
    async fn try_recv_with_pending() {
        let (mut bus, client) = EventBus::new(16);

        client.submit("test").unwrap();

        let req = bus.try_recv_request();
        assert!(req.is_some());
        assert!(matches!(req.unwrap(), AgentRequest::Submit { .. }));
    }

    #[tokio::test]
    async fn high_throughput_notifications() {
        let (bus, mut client) = EventBus::new(1024);

        // Send 500 notifications rapidly
        for i in 0..500 {
            bus.notify(AgentNotification::TextDelta {
                text: format!("chunk-{i}"),
            });
        }

        // Receive all
        for i in 0..500 {
            let event = client.recv_notification().await.unwrap();
            match event {
                AgentNotification::TextDelta { text } => {
                    assert_eq!(text, format!("chunk-{i}"));
                }
                other => panic!("Expected TextDelta, got {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn agent_lifecycle_events() {
        let (bus, mut client) = EventBus::new(16);

        bus.notify(AgentNotification::AgentSpawned {
            agent_id: "a1".into(),
            name: Some("reviewer".into()),
            agent_type: "explore".into(),
            background: true,
        });
        bus.notify(AgentNotification::AgentProgress {
            agent_id: "a1".into(),
            text: "Found 3 files".into(),
        });
        bus.notify(AgentNotification::AgentComplete {
            agent_id: "a1".into(),
            result: "Review complete".into(),
            is_error: false,
        });

        // Verify lifecycle
        let e1 = client.recv_notification().await.unwrap();
        assert!(matches!(e1, AgentNotification::AgentSpawned { .. }));

        let e2 = client.recv_notification().await.unwrap();
        assert!(matches!(e2, AgentNotification::AgentProgress { .. }));

        let e3 = client.recv_notification().await.unwrap();
        assert!(matches!(e3, AgentNotification::AgentComplete { .. }));
    }

    #[tokio::test]
    async fn permission_request_timeout_auto_denies() {
        let (mut bus, _client) = EventBus::new(16);

        // No one handles the permission request → should timeout and return None
        let result = bus
            .request_permission_with_timeout(
                "Bash",
                serde_json::json!({"cmd": "rm -rf /"}),
                RiskLevel::High,
                "Delete everything",
                std::time::Duration::from_millis(50),
            )
            .await;

        assert!(result.is_none(), "Timed-out permission request should return None (deny)");
    }

    #[tokio::test]
    async fn permission_response_within_timeout() {
        let (mut bus, mut client) = EventBus::new(16);

        let core = tokio::spawn(async move {
            let resp = bus
                .request_permission_with_timeout(
                    "FileRead",
                    serde_json::json!({}),
                    RiskLevel::Low,
                    "Read a file",
                    std::time::Duration::from_secs(5),
                )
                .await
                .unwrap();
            assert!(resp.granted);
        });

        let perm = client.recv_permission_request().await.unwrap();
        client
            .send_permission_response(PermissionResponse {
                request_id: perm.request_id,
                granted: true,
                remember: false,
            })
            .unwrap();

        core.await.unwrap();
    }

    // ── Diagnostics tests ────────────────────────────────────────────────

    #[tokio::test]
    async fn diagnostics_counters_increment() {
        let (bus, client) = EventBus::new(16);

        // Initially zero
        let d = bus.diagnostics();
        assert_eq!(d.notifications_sent, 0);
        assert_eq!(d.requests_sent, 0);

        // Send some notifications
        bus.notify(AgentNotification::TextDelta { text: "a".into() });
        bus.notify(AgentNotification::TextDelta { text: "b".into() });
        bus.notify(AgentNotification::TextDelta { text: "c".into() });

        let d = bus.diagnostics();
        assert_eq!(d.notifications_sent, 3);

        // Send a request from the client side
        client.submit("hello").unwrap();
        client.abort().unwrap();

        let d = client.diagnostics();
        assert_eq!(d.requests_sent, 2);
        assert_eq!(d.notifications_sent, 3);
    }

    #[tokio::test]
    async fn diagnostics_subscriber_count() {
        let (bus, client) = EventBus::new(16);

        let initial = bus.diagnostics().notification_subscribers;

        // Creating a subscription adds a subscriber
        let _sub = client.subscribe();
        let after_sub = bus.diagnostics().notification_subscribers;
        assert_eq!(after_sub, initial + 1);

        // Drop the subscription
        drop(_sub);
        let after_drop = bus.diagnostics().notification_subscribers;
        assert_eq!(after_drop, initial);
    }

    #[tokio::test]
    async fn new_client_shares_diagnostics() {
        let (bus, _client) = EventBus::new(16);

        bus.notify(AgentNotification::TextDelta { text: "x".into() });

        let client2 = bus.new_client();
        let d = client2.diagnostics();
        assert_eq!(d.notifications_sent, 1, "secondary client sees same counters");
    }

    // ── Event history tests ──────────────────────────────────────────────

    #[tokio::test]
    async fn event_history_records_and_replays() {
        let (bus, _client) = EventBus::new(16);

        bus.notify(AgentNotification::TextDelta { text: "one".into() });
        bus.notify(AgentNotification::TextDelta { text: "two".into() });

        let history = bus.recent_history();
        assert_eq!(history.len(), 2);
        match &history[0] {
            AgentNotification::TextDelta { text } => assert_eq!(text, "one"),
            other => panic!("Expected TextDelta, got {other:?}"),
        }
        match &history[1] {
            AgentNotification::TextDelta { text } => assert_eq!(text, "two"),
            other => panic!("Expected TextDelta, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn history_capacity_eviction() {
        // Use a tiny capacity of 3
        let (bus, _client) = EventBus::with_history(16, 3);

        for i in 0..5 {
            bus.notify(AgentNotification::TextDelta {
                text: format!("msg-{i}"),
            });
        }

        let history = bus.recent_history();
        assert_eq!(history.len(), 3, "history should be capped at capacity");

        // Should contain the 3 most recent: msg-2, msg-3, msg-4
        let texts: Vec<String> = history
            .iter()
            .map(|n| match n {
                AgentNotification::TextDelta { text } => text.clone(),
                other => panic!("Expected TextDelta, got {other:?}"),
            })
            .collect();
        assert_eq!(texts, vec!["msg-2", "msg-3", "msg-4"]);
    }

    #[tokio::test]
    async fn history_zero_capacity_disables() {
        let (bus, _client) = EventBus::with_history(16, 0);

        bus.notify(AgentNotification::TextDelta { text: "ignored".into() });

        let history = bus.recent_history();
        assert!(history.is_empty(), "zero-capacity history should store nothing");
    }

    #[tokio::test]
    async fn client_can_replay_history() {
        let (bus, _client) = EventBus::new(16);

        bus.notify(AgentNotification::SessionStart {
            session_id: "s1".into(),
            model: "sonnet".into(),
        });

        // Late-joining client can read history
        let client2 = bus.new_client();
        let history = client2.recent_history();
        assert_eq!(history.len(), 1);
        assert!(matches!(history[0], AgentNotification::SessionStart { .. }));
    }

    // ── NotificationSubscription tests ───────────────────────────────────

    #[tokio::test]
    async fn subscription_basic_recv() {
        let (bus, client) = EventBus::new(16);
        let mut sub = client.subscribe();

        bus.notify(AgentNotification::TextDelta { text: "hi".into() });

        let event = sub.recv().await.unwrap();
        match event {
            AgentNotification::TextDelta { text } => assert_eq!(text, "hi"),
            other => panic!("Expected TextDelta, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn subscription_try_recv() {
        let (bus, client) = EventBus::new(16);
        let mut sub = client.subscribe();

        // Nothing pending
        assert!(sub.try_recv().is_none());

        bus.notify(AgentNotification::TextDelta { text: "x".into() });

        // Now available
        let event = sub.try_recv().unwrap();
        assert!(matches!(event, AgentNotification::TextDelta { .. }));

        // Empty again
        assert!(sub.try_recv().is_none());
    }

    #[tokio::test]
    async fn subscription_with_custom_filter() {
        let (bus, client) = EventBus::new(16);
        let mut sub = client.subscribe().with_filter(|n| {
            matches!(n, AgentNotification::SessionStart { .. })
        });

        // Send a mix of events
        bus.notify(AgentNotification::TextDelta { text: "skip".into() });
        bus.notify(AgentNotification::SessionStart {
            session_id: "s1".into(),
            model: "sonnet".into(),
        });
        bus.notify(AgentNotification::TextDelta { text: "skip2".into() });

        // Should only get SessionStart
        let event = sub.recv().await.unwrap();
        assert!(matches!(event, AgentNotification::SessionStart { .. }));
    }

    #[tokio::test]
    async fn subscription_tools_only_filter() {
        let (bus, client) = EventBus::new(16);
        let mut sub = client.subscribe().tools_only();

        bus.notify(AgentNotification::TextDelta { text: "skip".into() });
        bus.notify(AgentNotification::ToolUseStart {
            id: "t1".into(),
            tool_name: "Bash".into(),
        });
        bus.notify(AgentNotification::ThinkingDelta { text: "skip".into() });
        bus.notify(AgentNotification::ToolUseComplete {
            id: "t1".into(),
            tool_name: "Bash".into(),
            is_error: false,
            result_preview: Some("ok".into()),
        });

        let e1 = sub.recv().await.unwrap();
        assert!(matches!(e1, AgentNotification::ToolUseStart { .. }));

        let e2 = sub.recv().await.unwrap();
        assert!(matches!(e2, AgentNotification::ToolUseComplete { .. }));
    }

    #[tokio::test]
    async fn subscription_agents_only_filter() {
        let (bus, client) = EventBus::new(16);
        let mut sub = client.subscribe().agents_only();

        bus.notify(AgentNotification::TextDelta { text: "skip".into() });
        bus.notify(AgentNotification::AgentSpawned {
            agent_id: "a1".into(),
            name: Some("reviewer".into()),
            agent_type: "explore".into(),
            background: true,
        });
        bus.notify(AgentNotification::TextDelta { text: "skip".into() });

        let event = sub.recv().await.unwrap();
        assert!(matches!(event, AgentNotification::AgentSpawned { .. }));
    }

    #[tokio::test]
    async fn subscription_content_only_filter() {
        let (bus, client) = EventBus::new(16);
        let mut sub = client.subscribe().content_only();

        bus.notify(AgentNotification::TurnStart { turn: 1 });
        bus.notify(AgentNotification::TextDelta { text: "hello".into() });
        bus.notify(AgentNotification::ThinkingDelta { text: "hmm".into() });
        bus.notify(AgentNotification::TurnStart { turn: 2 });

        let e1 = sub.recv().await.unwrap();
        assert!(matches!(e1, AgentNotification::TextDelta { .. }));

        let e2 = sub.recv().await.unwrap();
        assert!(matches!(e2, AgentNotification::ThinkingDelta { .. }));
    }

    #[tokio::test]
    async fn subscription_errors_only_filter() {
        let (bus, client) = EventBus::new(16);
        let mut sub = client.subscribe().errors_only();

        bus.notify(AgentNotification::TextDelta { text: "skip".into() });
        bus.notify(AgentNotification::Error {
            code: ErrorCode::ApiError,
            message: "rate limited".into(),
        });
        bus.notify(AgentNotification::TextDelta { text: "skip".into() });

        let event = sub.recv().await.unwrap();
        match event {
            AgentNotification::Error { code, message } => {
                assert!(matches!(code, ErrorCode::ApiError));
                assert_eq!(message, "rate limited");
            }
            other => panic!("Expected Error, got {other:?}"),
        }
    }

    // ── is_core_alive tests ──────────────────────────────────────────────

    #[tokio::test]
    async fn is_core_alive_true_while_bus_exists() {
        let (bus, client) = EventBus::new(16);
        assert!(client.is_core_alive());

        // Still alive after sending some data
        bus.notify(AgentNotification::TextDelta { text: "x".into() });
        assert!(client.is_core_alive());
    }

    #[tokio::test]
    async fn is_core_alive_false_after_bus_dropped() {
        let (bus, client) = EventBus::new(16);
        assert!(client.is_core_alive());

        drop(bus);

        // Give the watch channel time to propagate
        tokio::task::yield_now().await;
        assert!(!client.is_core_alive());
    }

    // ── with_history constructor test ────────────────────────────────────

    #[tokio::test]
    async fn with_history_custom_capacity() {
        let (bus, _client) = EventBus::with_history(16, 5);

        let d = bus.diagnostics();
        assert_eq!(d.history_capacity, 5);
        assert_eq!(d.history_len, 0);

        for i in 0..10 {
            bus.notify(AgentNotification::TextDelta {
                text: format!("n-{i}"),
            });
        }

        let d = bus.diagnostics();
        assert_eq!(d.history_len, 5, "should cap at custom capacity");
        assert_eq!(d.notifications_sent, 10);
    }

    // ── Subscription closes when core drops ──────────────────────────────

    #[tokio::test]
    async fn subscription_returns_none_on_core_drop() {
        let (bus, client) = EventBus::new(16);
        let mut sub = client.subscribe();

        drop(bus);

        let result = sub.recv().await;
        assert!(result.is_none(), "subscription should yield None when core drops");
    }
}
