//! `clawed-bus` — Internal event bus and JSON-RPC protocol layer.
//!
//! This crate decouples the **Agent Core** (query loop, tool execution,
//! permissions, hooks) from the **UI layer** (terminal REPL, IDE extension,
//! Web UI) through a typed message bus.
//!
//! ## Architecture
//!
//! ```text
//! ┌─────────────────────────────────────┐
//! │  Agent Core (clawed-agent)           │
//! │  holds BusHandle                     │
//! │  sends: AgentNotification            │
//! │  receives: AgentRequest              │
//! │  sends: PermissionRequest            │
//! │  receives: PermissionResponse        │
//! └──────────────┬──────────────────────┘
//!                │ tokio channels
//! ┌──────────────┴──────────────────────┐
//! │  UI Client (clawed-cli / IDE / Web)  │
//! │  holds ClientHandle                  │
//! │  receives: AgentNotification         │
//! │  sends: AgentRequest                 │
//! │  receives: PermissionRequest         │
//! │  sends: PermissionResponse           │
//! └─────────────────────────────────────┘
//! ```
//!
//! ## Features
//!
//! - **Typed channels**: `AgentNotification` (broadcast 1:N),
//!   `AgentRequest` (mpsc N:1), `PermissionRequest/Response` (paired).
//! - **Diagnostics**: [`BusDiagnostics`] exposes message counters,
//!   subscriber counts, and history buffer stats.
//! - **RAII subscriptions**: [`NotificationSubscription`] with optional
//!   filtering and auto-cleanup on drop.
//! - **Event history**: Bounded ring buffer for notification replay by
//!   late-joining clients.
//!
//! ## Usage
//!
//! ```rust,ignore
//! let (bus, client) = clawed_bus::EventBus::new(256);
//!
//! // Agent core side
//! tokio::spawn(async move {
//!     bus.notify(AgentNotification::TextDelta { text: "Hello".into() });
//! });
//!
//! // UI client side — basic
//! while let Ok(event) = client.notifications().recv().await {
//!     println!("Got: {:?}", event);
//! }
//!
//! // UI client side — filtered subscription
//! let mut tools = client.subscribe().tools_only();
//! while let Some(event) = tools.recv().await {
//!     println!("Tool event: {:?}", event);
//! }
//! ```

pub mod bus;
pub mod events;

pub use bus::*;
pub use events::*;
