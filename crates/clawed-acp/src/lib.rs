//! ACP (Agent Client Protocol) implementation for Clawed Code.
//!
//! This crate implements the [Agent Client Protocol](https://agentclientprotocol.com),
//! allowing Clawed Code to be used as an ACP-compatible coding agent.
//!
//! # Architecture
//!
//! ```text
//!   ┌──────────┐   ACP (stdio/HTTP)   ┌─────────────────────────────────┐
//!   │  Editor   │◄────────────────────▶│        clawed-acp Agent         │
//!   │ (ACP cli) │                      │                                 │
//!   └──────────┘                      │  ┌──────────┐  ┌──────────────┐  │
//!                                     │  │ QueryEng. │  │ McpManager   │  │
//!                                     │  └──────────┘  └──────────────┘  │
//!                                     └─────────────────────────────────┘
//! ```
//!
//! # Modules
//!
//! - [`agent`] — ACP Agent implementation wrapping `QueryEngine`
//! - [`server`] — ACP server setup and transport management
//! - [`session`] — ACP session lifecycle
//! - [`mcp_bridge`] — MCP-over-ACP bridge for exposing MCP tools
//! - [`types`] — Additional ACP type conversions
//! - [`transport`] — Transport configuration

pub mod agent;
pub mod mcp_bridge;
pub mod server;
pub mod session;
pub mod transport;
pub mod types;

pub use agent::AcpAgent;
pub use server::AcpServer;
