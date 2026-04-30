//! Teleport — remote session management for CCR (Claude Code Remote).
//!
//! Provides:
//! - Environment listing (`list_environments`)
//! - Git bundle creation + upload (`upload_git_bundle`)
//! - API helpers with retry logic
//!
//! This is a simplified Rust port of the TypeScript `utils/teleport/` module.
//! The full CCR infrastructure (WebSocket relay, upstream proxy) lives in
//! other crates; this module focuses on the high-level teleport operations.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tokio::process::Command;
use tracing::debug;

static HTTP_CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();

fn shared_client() -> &'static reqwest::Client {
    HTTP_CLIENT.get_or_init(|| reqwest::Client::new())
}

// ── Types ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Environment {
    pub kind: String,
    pub environment_id: String,
    pub name: String,
    pub created_at: String,
    pub state: String,
}

#[derive(Debug, Clone, Deserialize)]
struct EnvironmentListResponse {
    environments: Vec<Environment>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BundleUploadResult {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bundle_size_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Retry configuration matching the TypeScript defaults.
const TELEPORT_RETRY_DELAYS: [Duration; 4] = [
    Duration::from_millis(2000),
    Duration::from_millis(4000),
    Duration::from_millis(8000),
    Duration::from_millis(16000),
];

// ── Environment API ─────────────────────────────────────────────────────────

/// Fetch available CCR environments from the Claude AI API.
pub async fn list_environments(access_token: &str, org_uuid: &str) -> Result<Vec<Environment>> {
    let base_url = api_base_url();
    let url = format!(
        "{}/v1/environment_providers",
        base_url.trim_end_matches('/')
    );

    let client = shared_client();

    let resp = retry_request(|| async {
        client
            .get(&url)
            .header("Authorization", format!("Bearer {}", access_token))
            .header("anthropic-version", "2023-06-01")
            .header("x-organization-uuid", org_uuid)
            .send()
            .await
    })
    .await?;

    if !resp.status().is_success() {
        anyhow::bail!(
            "Failed to fetch environments: {} {}",
            resp.status(),
            resp.text().await.unwrap_or_default()
        );
    }

    let body: EnvironmentListResponse = resp
        .json()
        .await
        .context("invalid JSON in environment list response")?;
    Ok(body.environments)
}

// ── Git Bundle ──────────────────────────────────────────────────────────────

/// Create a git bundle of the current repository and upload it to the CCR API.
///
/// Flow:
/// 1. Stash WIP (if dirty) and create a reachable ref.
/// 2. `git bundle create --all` to pack refs + objects.
/// 3. Upload to `/v1/files`.
/// 4. Clean up the temporary ref and bundle file.
pub async fn upload_git_bundle(
    access_token: &str,
    _org_uuid: &str,
    max_bytes: Option<u64>,
) -> Result<BundleUploadResult> {
    let git_root = match find_git_root().await {
        Some(p) => p,
        None => {
            return Ok(BundleUploadResult {
                success: false,
                file_id: None,
                bundle_size_bytes: None,
                error: Some("Not inside a git repository".into()),
            });
        }
    };

    let max_bytes = max_bytes.unwrap_or(100 * 1024 * 1024);
    let bundle_path =
        std::env::temp_dir().join(format!("clawed-bundle-{}.bundle", uuid::Uuid::new_v4()));

    // 1. Stash WIP if dirty
    let has_wip = git_stash_create(&git_root).await.ok().flatten().is_some();
    if has_wip {
        let _ = git_update_ref(&git_root, "refs/seed/stash", "stash").await;
    }

    // 2. Create bundle
    let bundle_result = create_git_bundle(&git_root, &bundle_path, max_bytes, has_wip).await;

    // 3. Upload if successful
    let upload_result = match bundle_result {
        Ok(size) => {
            debug!(
                "git bundle created: {} bytes at {}",
                size,
                bundle_path.display()
            );
            match upload_bundle_file(access_token, &bundle_path).await {
                Ok(file_id) => BundleUploadResult {
                    success: true,
                    file_id: Some(file_id),
                    bundle_size_bytes: Some(size),
                    error: None,
                },
                Err(e) => BundleUploadResult {
                    success: false,
                    file_id: None,
                    bundle_size_bytes: Some(size),
                    error: Some(format!("upload failed: {e}")),
                },
            }
        }
        Err(e) => BundleUploadResult {
            success: false,
            file_id: None,
            bundle_size_bytes: None,
            error: Some(format!("bundle creation failed: {e}")),
        },
    };

    // 4. Cleanup
    if git_update_ref(&git_root, "refs/seed/stash", "")
        .await
        .is_ok()
    {
        let _ = git_delete_ref(&git_root, "refs/seed/stash").await;
    }
    let _ = tokio::fs::remove_file(&bundle_path).await;

    Ok(upload_result)
}

// ── Internal helpers ────────────────────────────────────────────────────────

fn api_base_url() -> String {
    std::env::var("CLAUDE_API_BASE_URL").unwrap_or_else(|_| "https://api.claude.ai".to_string())
}

async fn retry_request<F, Fut>(mut f: F) -> Result<reqwest::Response>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = reqwest::Result<reqwest::Response>>,
{
    let mut last_err = None;
    for (attempt, delay) in TELEPORT_RETRY_DELAYS.iter().enumerate() {
        match f().await {
            Ok(resp) if resp.status().is_server_error() => {
                last_err = Some(anyhow::anyhow!("server error: {}", resp.status()));
            }
            Ok(resp) => return Ok(resp),
            Err(e) if e.is_timeout() || e.is_connect() || e.is_request() => {
                last_err = Some(anyhow::Error::from(e));
            }
            Err(e) => return Err(anyhow::Error::from(e)),
        }
        if attempt < TELEPORT_RETRY_DELAYS.len() - 1 {
            tokio::time::sleep(*delay).await;
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("all retry attempts exhausted")))
}

async fn find_git_root() -> Option<std::path::PathBuf> {
    let output = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Some(path.into())
}

async fn git_stash_create(git_root: &Path) -> Result<Option<String>> {
    let output = Command::new("git")
        .current_dir(git_root)
        .args(["stash", "create"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .await?;
    let sha = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if sha.is_empty() {
        Ok(None)
    } else {
        Ok(Some(sha))
    }
}

async fn git_update_ref(git_root: &Path, refname: &str, target: &str) -> Result<()> {
    let mut cmd = Command::new("git");
    cmd.current_dir(git_root).args(["update-ref", refname]);
    if target.is_empty() {
        cmd.arg("-d");
    } else {
        cmd.arg(target);
    }
    let output = cmd.output().await?;
    if !output.status.success() {
        anyhow::bail!(
            "git update-ref failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}

async fn git_delete_ref(git_root: &Path, refname: &str) -> Result<()> {
    let output = Command::new("git")
        .current_dir(git_root)
        .args(["update-ref", "-d", refname])
        .output()
        .await?;
    if !output.status.success() {
        anyhow::bail!(
            "git delete-ref failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}

async fn create_git_bundle(
    git_root: &Path,
    bundle_path: &Path,
    max_bytes: u64,
    has_stash: bool,
) -> Result<u64> {
    let bundle_path_str = bundle_path.to_string_lossy().to_string();
    let mut args = vec![
        "bundle".to_string(),
        "create".to_string(),
        bundle_path_str,
        "--all".to_string(),
    ];
    if has_stash {
        args.push("refs/seed/stash".to_string());
    }

    let output = Command::new("git")
        .current_dir(git_root)
        .args(&args)
        .output()
        .await
        .context("git bundle create failed")?;

    if !output.status.success() {
        anyhow::bail!(
            "git bundle create failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let meta = tokio::fs::metadata(bundle_path).await?;
    let size = meta.len();
    if size > max_bytes {
        anyhow::bail!(
            "git bundle exceeds size limit: {} > {} bytes",
            size,
            max_bytes
        );
    }
    Ok(size)
}

async fn upload_bundle_file(access_token: &str, bundle_path: &Path) -> Result<String> {
    let client = shared_client();
    let url = format!("{}/v1/files", api_base_url().trim_end_matches('/'));

    let file_bytes = tokio::fs::read(bundle_path).await?;
    let part = reqwest::multipart::Part::bytes(file_bytes)
        .file_name("repo.bundle")
        .mime_str("application/octet-stream")?;
    let form = reqwest::multipart::Form::new().part("file", part);

    let resp = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", access_token))
        .header("anthropic-version", "2023-06-01")
        .multipart(form)
        .send()
        .await?;

    if !resp.status().is_success() {
        anyhow::bail!(
            "file upload failed: {}",
            resp.text().await.unwrap_or_default()
        );
    }

    let json: serde_json::Value = resp.json().await?;
    let file_id = json
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("missing file id in upload response"))?;
    Ok(file_id.to_string())
}
