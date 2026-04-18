//! Chrome extension integration for Claude Code.
//!
//! Simplified Rust port of the TypeScript `utils/claudeInChrome/` and
//! `commands/chrome/` modules.
//!
//! Provides:
//! - Chrome extension installation status detection
//! - Native Messaging host manifest management
//! - `/chrome` slash command handler
//! - MCP server bridge skeleton for Chrome tab interaction
//!
//! The full `claude-for-chrome-mcp` server from the original project is a
//! private package; this module provides the open-source scaffolding.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

/// Chrome extension download URL.
pub const CHROME_EXTENSION_URL: &str = "https://claude.ai/chrome";

// ── Browser detection ───────────────────────────────────────────────────────

/// Supported Chromium-based browsers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ChromiumBrowser {
    Chrome,
    Edge,
    Brave,
    Opera,
    Arc,
}

impl ChromiumBrowser {
    pub fn app_name(&self) -> &'static str {
        match self {
            ChromiumBrowser::Chrome => "Google Chrome",
            ChromiumBrowser::Edge => "Microsoft Edge",
            ChromiumBrowser::Brave => "Brave Browser",
            ChromiumBrowser::Opera => "Opera",
            ChromiumBrowser::Arc => "Arc",
        }
    }

    /// Native Messaging host directory for this browser.
    pub fn native_messaging_dir(&self) -> Option<PathBuf> {
        let home = dirs::home_dir()?;
        #[cfg(target_os = "macos")]
        {
            let base = home.join("Library/Application Support");
            let path = match self {
                ChromiumBrowser::Chrome => base.join("Google/Chrome/NativeMessagingHosts"),
                ChromiumBrowser::Edge => base.join("Microsoft Edge/NativeMessagingHosts"),
                ChromiumBrowser::Brave => base.join("BraveSoftware/Brave-Browser/NativeMessagingHosts"),
                ChromiumBrowser::Opera => base.join("com.operasoftware.Opera/NativeMessagingHosts"),
                ChromiumBrowser::Arc => base.join("Arc/User Data/NativeMessagingHosts"),
            };
            Some(path)
        }
        #[cfg(target_os = "linux")]
        {
            let path = match self {
                ChromiumBrowser::Chrome => home.join(".config/google-chrome/NativeMessagingHosts"),
                ChromiumBrowser::Edge => home.join(".config/microsoft-edge/NativeMessagingHosts"),
                ChromiumBrowser::Brave => home.join(".config/BraveSoftware/Brave-Browser/NativeMessagingHosts"),
                ChromiumBrowser::Opera => home.join(".config/opera/NativeMessagingHosts"),
                ChromiumBrowser::Arc => return None, // Arc is macOS-only
            };
            Some(path)
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            None
        }
    }

    /// Check whether this browser appears to be installed.
    pub fn is_installed(&self) -> bool {
        self.native_messaging_dir().map_or(false, |d| d.parent().map_or(false, |p| p.exists()))
    }
}

/// Detect installed Chromium-based browsers.
pub fn detect_browsers() -> Vec<ChromiumBrowser> {
    use ChromiumBrowser::*;
    [Chrome, Edge, Brave, Opera, Arc]
        .into_iter()
        .filter(|b| b.is_installed())
        .collect()
}

// ── Native Messaging Host Manifest ──────────────────────────────────────────

/// Chrome Native Messaging host manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NativeHostManifest {
    pub name: String,
    pub description: String,
    pub path: String,
    #[serde(rename = "type")]
    pub host_type: String,
    #[serde(rename = "allowed_origins")]
    pub allowed_origins: Vec<String>,
}

impl NativeHostManifest {
    pub fn for_clawed(binary_path: &Path) -> Self {
        Self {
            name: "com.anthropic.clawed".into(),
            description: "Claude Code Chrome Native Messaging Host".into(),
            path: binary_path.to_string_lossy().to_string(),
            host_type: "stdio".into(),
            allowed_origins: vec![
                "chrome-extension://*/".into(),
                "extension://*/".into(),
            ],
        }
    }
}

/// Install the Native Messaging host manifest for all detected browsers.
pub fn install_native_host_manifest() -> Result<Vec<(ChromiumBrowser, PathBuf)>> {
    let current_exe = std::env::current_exe().context("failed to get current executable path")?;
    let manifest = NativeHostManifest::for_clawed(&current_exe);
    let json = serde_json::to_string_pretty(&manifest)?;

    let mut installed = Vec::new();
    for browser in detect_browsers() {
        if let Some(dir) = browser.native_messaging_dir() {
            fs::create_dir_all(&dir)
                .with_context(|| format!("failed to create {}", dir.display()))?;
            let path = dir.join("com.anthropic.clawed.json");
            fs::write(&path, &json)
                .with_context(|| format!("failed to write {}", path.display()))?;
            info!("installed NativeMessaging host manifest for {} at {}", browser.app_name(), path.display());
            installed.push((browser, path));
        }
    }

    if installed.is_empty() {
        warn!("no supported Chromium browsers detected; manifest not installed");
    }

    Ok(installed)
}

/// Remove the Native Messaging host manifest from all detected browsers.
pub fn uninstall_native_host_manifest() -> Result<()> {
    for browser in detect_browsers() {
        if let Some(dir) = browser.native_messaging_dir() {
            let path = dir.join("com.anthropic.clawed.json");
            if path.exists() {
                fs::remove_file(&path)?;
                info!("removed NativeMessaging host manifest for {}", browser.app_name());
            }
        }
    }
    Ok(())
}

// ── Chrome status ───────────────────────────────────────────────────────────

/// Current state of the Chrome extension integration.
#[derive(Debug, Clone)]
pub struct ChromeStatus {
    pub extension_installed: bool,
    pub native_host_installed: bool,
    pub browsers: Vec<ChromiumBrowser>,
}

impl ChromeStatus {
    pub fn check() -> Self {
        let browsers = detect_browsers();
        let native_host_installed = browsers.iter().any(|b| {
            b.native_messaging_dir()
                .map_or(false, |d| d.join("com.anthropic.clawed.json").exists())
        });

        // Extension installation is hard to detect locally without the extension
        // manifest. We approximate by checking whether the user has ever
        // installed the native host.
        let extension_installed = native_host_installed;

        Self {
            extension_installed,
            native_host_installed,
            browsers,
        }
    }

    pub fn summary(&self) -> String {
        if self.browsers.is_empty() {
            return "No Chromium browser detected.".into();
        }
        let browser_names: Vec<_> = self.browsers.iter().map(|b| b.app_name()).collect();
        format!(
            "Browsers: {} | Extension: {} | Native host: {}",
            browser_names.join(", "),
            if self.extension_installed { "installed" } else { "not installed" },
            if self.native_host_installed { "installed" } else { "not installed" }
        )
    }
}

// ── /chrome command handler ─────────────────────────────────────────────────

/// Handle the `/chrome` slash command.
pub fn handle_chrome_command(args: &[&str]) -> String {
    match args.first().copied() {
        Some("install") | Some("setup") => {
            match install_native_host_manifest() {
                Ok(installed) => {
                    let paths: Vec<_> = installed
                        .iter()
                        .map(|(b, p)| format!("  {} → {}", b.app_name(), p.display()))
                        .collect();
                    format!(
                        "Chrome Native Messaging host installed.\n\n{}\n\nNext steps:\n1. Install the Chrome extension from {}\n2. Refresh any open Claude Code tabs.",
                        paths.join("\n"),
                        CHROME_EXTENSION_URL
                    )
                }
                Err(e) => format!("Failed to install Chrome Native Messaging host: {e}"),
            }
        }
        Some("uninstall") | Some("remove") => {
            match uninstall_native_host_manifest() {
                Ok(()) => "Chrome Native Messaging host removed.".into(),
                Err(e) => format!("Failed to remove Chrome Native Messaging host: {e}"),
            }
        }
        Some("status") | None => {
            let status = ChromeStatus::check();
            format!(
                "Chrome Integration Status\n{}\n\n{}",
                "─".repeat(40),
                status.summary()
            )
        }
        Some(other) => format!(
            "Unknown /chrome subcommand: '{}'.\nUsage: /chrome [install | uninstall | status]",
            other
        ),
    }
}

// ── MCP Bridge Skeleton ─────────────────────────────────────────────────────

/// Minimal JSON-RPC message for Chrome Native Messaging.
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ChromeMessage {
    Request {
        id: u64,
        method: String,
        #[serde(default)]
        params: serde_json::Value,
    },
    Response {
        id: u64,
        #[serde(default)]
        result: Option<serde_json::Value>,
        #[serde(default)]
        error: Option<serde_json::Value>,
    },
    Notification {
        method: String,
        #[serde(default)]
        params: serde_json::Value,
    },
}

/// Read a single Native Messaging message from stdin (length-prefixed JSON).
#[allow(dead_code)]
pub fn read_native_message() -> Result<Option<ChromeMessage>> {
    use std::io::{Read, stdin};

    let mut len_buf = [0u8; 4];
    let mut handle = stdin().lock();
    match handle.read_exact(&mut len_buf) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e.into()),
    }
    let len = u32::from_le_bytes(len_buf) as usize;
    if len == 0 || len > 1024 * 1024 {
        anyhow::bail!("invalid Native Messaging message length: {}", len);
    }

    let mut buf = vec![0u8; len];
    handle.read_exact(&mut buf)?;
    let msg: ChromeMessage = serde_json::from_slice(&buf)?;
    Ok(Some(msg))
}

/// Write a single Native Messaging message to stdout (length-prefixed JSON).
#[allow(dead_code)]
pub fn write_native_message(msg: &ChromeMessage) -> Result<()> {
    use std::io::{Write, stdout};

    let json = serde_json::to_vec(msg)?;
    let len = json.len() as u32;
    let mut handle = stdout().lock();
    handle.write_all(&len.to_le_bytes())?;
    handle.write_all(&json)?;
    handle.flush()?;
    Ok(())
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn manifest_serialization_roundtrip() {
        let m = NativeHostManifest::for_clawed(Path::new("/tmp/clawed"));
        let json = serde_json::to_string(&m).unwrap();
        assert!(json.contains("com.anthropic.clawed"));
        assert!(json.contains("stdio"));
    }

    #[test]
    fn chrome_message_roundtrip() {
        let msg = ChromeMessage::Request {
            id: 1,
            method: "getPageContent".into(),
            params: serde_json::json!({"url": "https://example.com"}),
        };
        let json = serde_json::to_vec(&msg).unwrap();
        let decoded: ChromeMessage = serde_json::from_slice(&json).unwrap();
        match decoded {
            ChromeMessage::Request { id, method, .. } => {
                assert_eq!(id, 1);
                assert_eq!(method, "getPageContent");
            }
            _ => panic!("expected Request variant"),
        }
    }

    #[test]
    fn handle_chrome_status_no_args() {
        let out = handle_chrome_command(&[]);
        assert!(out.contains("Chrome Integration Status"));
    }
}
