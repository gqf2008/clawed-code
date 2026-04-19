#![allow(dead_code)]

//! Native binary installer for `clawed`.
//!
//! Simplified Rust port of the TypeScript `utils/nativeInstaller/` module.
//! Manages installation into `~/.local/bin/clawed` with:
//! - Atomic symlink-based activation
//! - PID-based version locking (prevents concurrent installs)
//! - Shell PATH configuration
//! - Cleanup of old versions

use std::fs;
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

/// Standard install directory and binary path.
const INSTALL_DIR: &str = ".local/bin";
const BINARY_NAME: &str = "clawed";
const VERSION_RETENTION: usize = 2;

/// Lock file content for PID-based locking.
#[derive(Debug, Serialize, Deserialize)]
struct VersionLock {
    pid: u32,
    version: String,
    exec_path: String,
    acquired_at: u64,
}

/// Install the current running binary into `~/.local/bin/clawed`.
///
/// 1. Copy current executable to `~/.local/bin/clawed-{version}`.
/// 2. Update `~/.local/bin/clawed` symlink to point to it.
/// 3. Ensure `~/.local/bin` is in shell PATH.
pub fn install() -> Result<InstallReport> {
    let home = dirs::home_dir().context("could not determine home directory")?;
    let bin_dir = home.join(INSTALL_DIR);
    let target = bin_dir.join(BINARY_NAME);

    fs::create_dir_all(&bin_dir)
        .with_context(|| format!("failed to create {}", bin_dir.display()))?;

    let current_exe = std::env::current_exe().context("failed to get current executable path")?;
    let version = get_version_from_binary(&current_exe).unwrap_or_else(|_| "unknown".into());
    let versioned = bin_dir.join(format!("{}-{}", BINARY_NAME, version));

    // Acquire lock
    let lock_path = version_lock_path(&versioned);
    let _lock = acquire_lock(&lock_path, &version)?;

    // Copy binary
    fs::copy(&current_exe, &versioned)
        .with_context(|| format!("failed to copy {} → {}", current_exe.display(), versioned.display()))?;

    #[cfg(unix)]
    {
        let mut perms = fs::metadata(&versioned)?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&versioned, perms)?;
    }

    // Atomic symlink update
    let tmp_link = bin_dir.join(format!("{}.tmp", BINARY_NAME));
    #[cfg(unix)]
    {
        if tmp_link.exists() {
            fs::remove_file(&tmp_link)?;
        }
        std::os::unix::fs::symlink(&versioned, &tmp_link)
            .with_context(|| format!("failed to create temp symlink {}", tmp_link.display()))?;
        fs::rename(&tmp_link, &target)
            .with_context(|| format!("failed to activate symlink {}", target.display()))?;
    }
    #[cfg(not(unix))]
    {
        // Windows: copy instead of symlink for simplicity
        if target.exists() {
            fs::remove_file(&target)?;
        }
        fs::copy(&versioned, &target)?;
    }

    // Clean up old versions
    cleanup_old_versions(&bin_dir, &version)?;

    let shell_hint = check_shell_path(&bin_dir);

    info!("installed {} → {}", versioned.display(), target.display());
    Ok(InstallReport {
        version,
        path: target,
        shell_path_hint: shell_hint,
    })
}

/// Uninstall `clawed` from `~/.local/bin`.
pub fn uninstall() -> Result<()> {
    let home = dirs::home_dir().context("could not determine home directory")?;
    let bin_dir = home.join(INSTALL_DIR);
    let target = bin_dir.join(BINARY_NAME);

    if target.exists() {
        fs::remove_file(&target)
            .with_context(|| format!("failed to remove {}", target.display()))?;
        info!("removed {}", target.display());
    }

    // Remove versioned binaries
    if bin_dir.exists() {
        for entry in fs::read_dir(&bin_dir)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with(&format!("{}-", BINARY_NAME)) {
                fs::remove_file(entry.path())?;
            }
        }
    }

    Ok(())
}

/// Check whether a native installation exists and is on PATH.
pub fn check_install() -> InstallStatus {
    let home = match dirs::home_dir() {
        Some(h) => h,
        None => return InstallStatus::NotInstalled,
    };
    let bin_dir = home.join(INSTALL_DIR);
    let target = bin_dir.join(BINARY_NAME);

    if !target.exists() {
        return InstallStatus::NotInstalled;
    }

    let on_path = std::env::var("PATH")
        .unwrap_or_default()
        .split(':')
        .any(|p| Path::new(p) == bin_dir);

    let version = get_version_from_binary(&target).ok();

    if on_path {
        InstallStatus::Installed { path: target, version }
    } else {
        InstallStatus::InstalledButNotOnPath { path: target, version }
    }
}

// ── Types ───────────────────────────────────────────────────────────────────

#[derive(Debug)]
pub struct InstallReport {
    pub version: String,
    pub path: PathBuf,
    pub shell_path_hint: Option<String>,
}

#[derive(Debug)]
pub enum InstallStatus {
    NotInstalled,
    Installed { path: PathBuf, version: Option<String> },
    InstalledButNotOnPath { path: PathBuf, version: Option<String> },
}

// ── Internal helpers ────────────────────────────────────────────────────────

fn get_version_from_binary(path: &Path) -> Result<String> {
    // Try running `clawed --version` to extract the version
    let output = std::process::Command::new(path)
        .arg("--version")
        .output()
        .with_context(|| format!("failed to run {} --version", path.display()))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    // Expected format: "clawed X.Y.Z"
    let version = stdout
        .split_whitespace()
        .nth(1)
        .unwrap_or("unknown")
        .to_string();
    Ok(version)
}

fn version_lock_path(versioned_binary: &Path) -> PathBuf {
    let mut path = versioned_binary.as_os_str().to_os_string();
    path.push(".lock");
    PathBuf::from(path)
}

fn acquire_lock(lock_path: &Path, version: &str) -> Result<LockGuard> {
    if lock_path.exists() {
        if let Ok(content) = fs::read_to_string(lock_path) {
            if let Ok(lock) = serde_json::from_str::<VersionLock>(&content) {
                if is_process_running(lock.pid) {
                    anyhow::bail!(
                        "installation of version {} is already in progress (PID {})",
                        version,
                        lock.pid
                    );
                }
                // Stale lock — proceed
                warn!("removing stale lock for PID {}", lock.pid);
            }
        }
    }

    let lock = VersionLock {
        pid: std::process::id(),
        version: version.into(),
        exec_path: std::env::current_exe()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string(),
        acquired_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
    };

    let json = serde_json::to_string_pretty(&lock)?;
    let mut file = fs::File::create(lock_path)
        .with_context(|| format!("failed to create lock file {}", lock_path.display()))?;
    file.write_all(json.as_bytes())?;

    Ok(LockGuard {
        path: lock_path.to_path_buf(),
    })
}

fn is_process_running(pid: u32) -> bool {
    #[cfg(unix)]
    {
        // Signal 0 is a no-op that checks process existence.
        unsafe { libc::kill(pid as i32, 0) == 0 }
    }
    #[cfg(not(unix))]
    {
        // On Windows, check via OpenProcess (simplified — always true to be conservative).
        true
    }
}

struct LockGuard {
    path: PathBuf,
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn cleanup_old_versions(bin_dir: &Path, current_version: &str) -> Result<()> {
    let mut versions: Vec<(String, PathBuf)> = Vec::new();

    if bin_dir.exists() {
        for entry in fs::read_dir(bin_dir)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with(&format!("{}-", BINARY_NAME)) && !name.ends_with(".lock") {
                let ver = name.trim_start_matches(&format!("{}-", BINARY_NAME)).to_string();
                versions.push((ver, entry.path()));
            }
        }
    }

    // Sort by version descending (simple string sort — good enough for semver-like strings)
    versions.sort_by(|a, b| b.0.cmp(&a.0));

    for (_idx, (_ver, path)) in versions.iter().enumerate().skip(VERSION_RETENTION) {
        if _ver != current_version {
            debug!("removing old version: {}", path.display());
            let _ = fs::remove_file(path);
            let _ = fs::remove_file(path.with_extension("lock"));
        }
    }

    Ok(())
}

fn check_shell_path(bin_dir: &Path) -> Option<String> {
    let shell = std::env::var("SHELL").unwrap_or_default();
    let shell_name = Path::new(&shell).file_name()?.to_string_lossy();

    let config_file = if shell_name.contains("zsh") {
        dirs::home_dir().map(|h| h.join(".zshrc"))
    } else if shell_name.contains("bash") {
        dirs::home_dir().map(|h| h.join(".bashrc"))
    } else {
        None
    }?;

    let path_entry = format!("export PATH=\"{}:$PATH\"", bin_dir.display());

    if config_file.exists() {
        if let Ok(contents) = fs::read_to_string(&config_file) {
            if contents.contains(&path_entry) || contents.contains(&bin_dir.to_string_lossy().to_string()) {
                return None; // Already configured
            }
        }
    }

    Some(format!(
        "echo 'export PATH=\"{}:$PATH\"' >> {} && source {}",
        bin_dir.display(),
        config_file.display(),
        config_file.display()
    ))
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn version_lock_path_format() {
        let p = PathBuf::from("/tmp/clawed-v1.0.0");
        assert_eq!(version_lock_path(&p), PathBuf::from("/tmp/clawed-v1.0.0.lock"));
    }

    #[test]
    fn cleanup_keeps_recent_versions() {
        let dir = TempDir::new().unwrap();
        let bin_dir = dir.path();

        // Create fake versioned binaries
        for v in ["0.1.0", "0.2.0", "0.3.0", "0.4.0"] {
            fs::write(bin_dir.join(format!("clawed-{}", v)), "fake").unwrap();
        }

        cleanup_old_versions(bin_dir, "0.4.0").unwrap();

        // Should keep 0.4.0 and 0.3.0
        assert!(bin_dir.join("clawed-0.4.0").exists());
        assert!(bin_dir.join("clawed-0.3.0").exists());
        // 0.2.0 and 0.1.0 should be removed
        assert!(!bin_dir.join("clawed-0.2.0").exists());
        assert!(!bin_dir.join("clawed-0.1.0").exists());
    }

    #[test]
    fn lock_guard_removes_file_on_drop() {
        let dir = TempDir::new().unwrap();
        let lock_path = dir.path().join("test.lock");

        {
            let _guard = LockGuard {
                path: lock_path.clone(),
            };
            fs::write(&lock_path, "locked").unwrap();
            assert!(lock_path.exists());
        }

        assert!(!lock_path.exists());
    }
}
