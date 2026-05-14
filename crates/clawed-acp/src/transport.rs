//! ACP transport configuration.
//!
//! ACP supports stdio for local agents and HTTP/WebSocket for remote agents.
//! This module provides transport setup helpers.

use std::path::Path;

use anyhow::{Context, Result};
use tokio::io::BufReader;
use tokio::process::{Child, Command};

/// ACP transport types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcpTransportKind {
    /// JSON-RPC over stdio (for local subprocess agents).
    Stdio,
    /// JSON-RPC over HTTP (for remote agents — WIP).
    Http,
    /// JSON-RPC over WebSocket (for remote agents — WIP).
    WebSocket,
}

/// Configuration for ACP transport.
#[derive(Debug, Clone)]
pub struct AcpTransportConfig {
    /// Transport type.
    pub kind: AcpTransportKind,
    /// Host to bind (for HTTP/WS).
    pub host: String,
    /// Port to bind (for HTTP/WS).
    pub port: u16,
    /// Working directory for the agent.
    pub cwd: Option<String>,
}

impl Default for AcpTransportConfig {
    fn default() -> Self {
        Self {
            kind: AcpTransportKind::Stdio,
            host: "127.0.0.1".to_string(),
            port: 0,
            cwd: None,
        }
    }
}

/// Spawn an ACP agent as a subprocess connected via stdio.
///
/// This is the standard local editor→agent setup. The child process
/// communicates using JSON-RPC 2.0 over its stdin/stdout.
pub async fn spawn_stdio_agent(
    binary: &Path,
    cwd: &Path,
) -> Result<(Child, tokio::io::BufReader<tokio::process::ChildStdout>)> {
    let mut child = Command::new(binary)
        .args(["acp"])
        .current_dir(cwd)
        .stdout(std::process::Stdio::piped())
        .stdin(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .context("Failed to spawn ACP agent subprocess")?;

    let stdout = child
        .stdout
        .take()
        .context("Failed to capture ACP agent stdout")?;
    let reader = BufReader::new(stdout);

    Ok((child, reader))
}

/// Build ACP server arguments for the `clawed acp` subcommand.
#[must_use]
pub fn acp_server_args(config: &AcpTransportConfig) -> Vec<String> {
    let mut args = vec!["acp".to_string()];
    match config.kind {
        AcpTransportKind::Stdio => {
            args.push("--stdio".to_string());
        }
        AcpTransportKind::Http => {
            args.push("--http".to_string());
            args.push(config.host.clone());
            args.push(config.port.to_string());
        }
        AcpTransportKind::WebSocket => {
            args.push("--ws".to_string());
            args.push(config.host.clone());
            args.push(config.port.to_string());
        }
    }
    if let Some(ref cwd) = config.cwd {
        args.push("--cwd".to_string());
        args.push(cwd.clone());
    }
    args
}
