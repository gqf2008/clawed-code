//! Upstream proxy configuration for managed/enterprise deployments.
//!
//! Mirrors the TypeScript `upstreamproxy/` module with a simplified design:
//! the full CONNECT-over-WebSocket relay is CCR-container-specific and
//! omitted here; this module provides the portable parts:
//!
//! 1. Reading proxy configuration from environment variables
//! 2. CA-bundle path management (caller provides the downloaded bundle)
//! 3. Environment-variable injection for child subprocesses
//!
//! When `CCR_UPSTREAM_PROXY_ENABLED` is set, the module exposes proxy env
//! vars so that Bash / MCP / LSP / hooks all inherit the same recipe.
//! Every step fails open — a broken proxy never breaks the session.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use tracing::{debug, warn};

const SESSION_TOKEN_PATH: &str = "/run/ccr/session_token";

/// Hosts the proxy must NOT intercept.
static NO_PROXY_LIST: &str = concat!(
    "localhost,127.0.0.1,::1,169.254.0.0/16,",
    "10.0.0.0/8,172.16.0.0/12,192.168.0.0/16,",
    "anthropic.com,.anthropic.com,*.anthropic.com,",
    "github.com,api.github.com,*.github.com,*.githubusercontent.com,",
    "registry.npmjs.org,pypi.org,files.pythonhosted.org,",
    "index.crates.io,proxy.golang.org"
);

/// Current proxy state. Mutable only during initialization.
#[derive(Debug, Clone, Default)]
pub struct ProxyState {
    pub enabled: bool,
    pub proxy_url: Option<String>,
    pub ca_bundle_path: Option<PathBuf>,
}

static STATE: Mutex<Option<ProxyState>> = Mutex::new(None);

/// Initialize upstream proxy configuration from environment.
///
/// Safe to call when the feature is off — returns immediately.
/// Returns the resolved state (may be `enabled: false`).
pub fn init() -> ProxyState {
    // Only active in remote/managed mode.
    if !is_env_truthy("CLAUDE_CODE_REMOTE") && !is_env_truthy("CCR_UPSTREAM_PROXY_ENABLED") {
        return ProxyState::default();
    }

    let proxy_url = std::env::var("HTTPS_PROXY")
        .or_else(|_| std::env::var("https_proxy"))
        .ok();

    if proxy_url.is_none() {
        debug!("[upstreamproxy] no HTTPS_PROXY configured; proxy disabled");
        return ProxyState::default();
    }

    let ca_bundle_path = std::env::var("SSL_CERT_FILE")
        .map(PathBuf::from)
        .ok()
        .or_else(|| {
            dirs::home_dir().map(|h| h.join(".ccr").join("ca-bundle.crt"))
        });

    let state = ProxyState {
        enabled: true,
        proxy_url,
        ca_bundle_path,
    };

    store_state(state.clone());
    state
}

/// Initialize with explicit parameters (useful for tests or container setups).
pub fn init_with(proxy_url: String, ca_bundle_path: Option<PathBuf>) -> ProxyState {
    let state = ProxyState {
        enabled: true,
        proxy_url: Some(proxy_url),
        ca_bundle_path,
    };
    store_state(state.clone());
    state
}

/// Returns proxy env vars to merge into every child subprocess.
/// Empty when the proxy is disabled.
pub fn get_env() -> HashMap<String, String> {
    let state = STATE.lock().unwrap_or_else(|e| e.into_inner()).clone().unwrap_or_default();

    if !state.enabled {
        return inherit_proxy_env();
    }

    let proxy_url = state.proxy_url.unwrap_or_default();
    let ca_bundle = state
        .ca_bundle_path
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();

    let mut env = HashMap::new();
    env.insert("HTTPS_PROXY".into(), proxy_url.clone());
    env.insert("https_proxy".into(), proxy_url);
    env.insert("NO_PROXY".into(), NO_PROXY_LIST.into());
    env.insert("no_proxy".into(), NO_PROXY_LIST.into());
    if !ca_bundle.is_empty() {
        env.insert("SSL_CERT_FILE".into(), ca_bundle.clone());
        env.insert("NODE_EXTRA_CA_CERTS".into(), ca_bundle.clone());
        env.insert("REQUESTS_CA_BUNDLE".into(), ca_bundle.clone());
        env.insert("CURL_CA_BUNDLE".into(), ca_bundle);
    }
    env
}

/// Whether the upstream proxy is currently enabled.
pub fn is_enabled() -> bool {
    STATE.lock().unwrap_or_else(|e| e.into_inner()).as_ref().map_or(false, |s| s.enabled)
}

/// Read the CCR session token from the well-known path, if present.
pub fn read_session_token() -> Option<String> {
    read_token(SESSION_TOKEN_PATH)
}

/// Security helper: set `prctl(PR_SET_DUMPABLE, 0)` on Linux to block
/// same-UID ptrace of this process. Silently no-ops on other platforms.
pub fn set_non_dumpable() {
    #[cfg(target_os = "linux")]
    unsafe {
        const PR_SET_DUMPABLE: i32 = 4;
        let rc = libc::prctl(PR_SET_DUMPABLE, 0, 0, 0, 0);
        if rc != 0 {
            warn!("[upstreamproxy] prctl(PR_SET_DUMPABLE,0) returned {rc}");
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        // No-op on non-Linux platforms.
    }
}

// ── Internal helpers ────────────────────────────────────────────────────────

fn is_env_truthy(key: &str) -> bool {
    match std::env::var(key) {
        Ok(v) => !v.is_empty() && v != "0" && v.to_lowercase() != "false",
        Err(_) => false,
    }
}

fn read_token(path: &str) -> Option<String> {
    match std::fs::read_to_string(path) {
        Ok(raw) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() { None } else { Some(trimmed.to_string()) }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            warn!("[upstreamproxy] token read failed: {e}");
            None
        }
    }
}

fn store_state(state: ProxyState) {
    *STATE.lock().unwrap_or_else(|e| e.into_inner()) = Some(state);
}

/// Inherit proxy env vars from the parent process when our own relay is not running.
fn inherit_proxy_env() -> HashMap<String, String> {
    let keys = [
        "HTTPS_PROXY",
        "https_proxy",
        "HTTP_PROXY",
        "http_proxy",
        "NO_PROXY",
        "no_proxy",
        "SSL_CERT_FILE",
        "NODE_EXTRA_CA_CERTS",
        "REQUESTS_CA_BUNDLE",
        "CURL_CA_BUNDLE",
    ];
    let mut env = HashMap::new();
    for key in keys {
        if let Ok(val) = std::env::var(key) {
            env.insert(key.into(), val);
        }
    }
    env
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proxy_state_default_is_disabled() {
        let s = ProxyState::default();
        assert!(!s.enabled);
    }

    #[test]
    fn init_with_creates_enabled_state() {
        let s = init_with("http://proxy:8080".into(), None);
        assert!(s.enabled);
        assert_eq!(s.proxy_url, Some("http://proxy:8080".into()));
    }

    #[test]
    fn get_env_returns_proxy_vars_when_enabled() {
        init_with("http://127.0.0.1:8080".into(), Some(PathBuf::from("/tmp/ca.crt")));
        let env = get_env();
        assert_eq!(env.get("HTTPS_PROXY"), Some(&"http://127.0.0.1:8080".to_string()));
        assert!(env.contains_key("NO_PROXY"));
        assert_eq!(env.get("SSL_CERT_FILE"), Some(&"/tmp/ca.crt".to_string()));
    }

    #[test]
    fn is_env_truthy_logic() {
        // We can't mutate real env vars in a unit test, so just verify the logic
        // by checking that a non-existent key returns false.
        assert!(!is_env_truthy("THIS_KEY_SHOULD_NOT_EXIST_EVER_12345"));
    }
}
