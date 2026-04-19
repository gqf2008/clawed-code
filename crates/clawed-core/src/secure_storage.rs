//! Cross-platform secure credential storage.
//!
//! Provides a unified interface for storing sensitive data (OAuth tokens, API keys)
//! using the OS-native credential store when available, with a plaintext fallback
//! for environments where keychain/keyring is unavailable.
//!
//! Design mirrors the TypeScript `secureStorage/` module:
//! - `KeyringStorage` — macOS Keychain / Linux Secret Service / Windows Credential Manager
//! - `PlaintextStorage` — `~/.claude/.credentials.json` with `0o600` permissions
//! - `FallbackStorage` — tries keyring first, falls back to plaintext on failure

use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

use anyhow::{Context, Result};
use tracing::{debug, warn};

/// A JSON-serializable bag of credential entries.
pub type SecureStorageData = HashMap<String, String>;

/// Platform-agnostic secure storage backend.
pub trait SecureStorage: Send + Sync {
    /// Human-readable backend name (e.g. "keychain", "plaintext").
    fn name(&self) -> String;

    /// Read all stored credentials.
    fn read(&self) -> Result<Option<SecureStorageData>>;

    /// Atomically replace stored credentials.
    fn write(&self, data: &SecureStorageData) -> Result<()>;

    /// Delete all stored credentials.
    fn delete(&self) -> Result<()>;
}

// ── Keyring Storage ─────────────────────────────────────────────────────────

/// Stores credentials as a single JSON blob in the OS keyring.
///
/// Uses the `keyring` crate which maps to:
/// - macOS: Keychain (generic password)
/// - Linux:  Secret Service API (gnome-keyring, kwallet, etc.)
/// - Windows: Credential Manager
pub struct KeyringStorage {
    service: String,
    account: String,
}

impl KeyringStorage {
    pub fn new(service: impl Into<String>, account: impl Into<String>) -> Self {
        Self {
            service: service.into(),
            account: account.into(),
        }
    }

    fn entry(&self) -> Result<keyring::Entry> {
        keyring::Entry::new(&self.service, &self.account)
            .with_context(|| format!("failed to open keyring entry for {}/{}", self.service, self.account))
    }
}

impl SecureStorage for KeyringStorage {
    fn name(&self) -> String {
        "keyring".to_string()
    }

    fn read(&self) -> Result<Option<SecureStorageData>> {
        let entry = self.entry()?;
        match entry.get_password() {
            Ok(json) => {
                let data: SecureStorageData =
                    serde_json::from_str(&json).context("keyring entry contains invalid JSON")?;
                Ok(Some(data))
            }
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => {
                warn!("keyring read failed: {e}");
                Err(anyhow::Error::from(e).context("keyring read failed"))
            }
        }
    }

    fn write(&self, data: &SecureStorageData) -> Result<()> {
        let json = serde_json::to_string(data).context("failed to serialize credentials")?;
        let entry = self.entry()?;
        entry
            .set_password(&json)
            .with_context(|| format!("failed to write keyring entry for {}/{}", self.service, self.account))?;
        debug!("keyring entry updated for {}/{}", self.service, self.account);
        Ok(())
    }

    fn delete(&self) -> Result<()> {
        let entry = self.entry()?;
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => {
                debug!("keyring entry deleted for {}/{}", self.service, self.account);
                Ok(())
            }
            Err(e) => Err(anyhow::Error::from(e).context("keyring delete failed")),
        }
    }
}

// ── Plaintext Storage ───────────────────────────────────────────────────────

/// Fallback storage that writes credentials to a JSON file with `0o600` permissions.
pub struct PlaintextStorage {
    path: PathBuf,
}

impl PlaintextStorage {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Default path: `~/.claude/.credentials.json`
    pub fn default_path() -> Result<PathBuf> {
        let home = dirs::home_dir().context("could not determine home directory")?;
        Ok(home.join(".claude").join(".credentials.json"))
    }

    /// Create with the default path.
    pub fn new_default() -> Result<Self> {
        Ok(Self::new(Self::default_path()?))
    }
}

impl SecureStorage for PlaintextStorage {
    fn name(&self) -> String {
        "plaintext".to_string()
    }

    fn read(&self) -> Result<Option<SecureStorageData>> {
        match fs::read_to_string(&self.path) {
            Ok(contents) => {
                let data: SecureStorageData =
                    serde_json::from_str(&contents).context("credentials file contains invalid JSON")?;
                Ok(Some(data))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    fn write(&self, data: &SecureStorageData) -> Result<()> {
        let dir = self.path.parent().context("credentials path has no parent directory")?;
        fs::create_dir_all(dir).with_context(|| format!("failed to create directory {}", dir.display()))?;

        let json = serde_json::to_string_pretty(data).context("failed to serialize credentials")?;
        let mut file = fs::File::create(&self.path)
            .with_context(|| format!("failed to create credentials file {}", self.path.display()))?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = file.metadata()?.permissions();
            perms.set_mode(0o600);
            fs::set_permissions(&self.path, perms)?;
        }

        file.write_all(json.as_bytes())
            .with_context(|| format!("failed to write credentials file {}", self.path.display()))?;

        warn!("credentials stored in plaintext at {} — consider using keyring storage", self.path.display());
        Ok(())
    }

    fn delete(&self) -> Result<()> {
        match fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}

// ── Fallback Storage ────────────────────────────────────────────────────────

/// Tries the primary storage first; on read/write failure falls back to the secondary.
/// On a successful write to primary when primary was previously empty, deletes secondary
/// (migration from plaintext to keyring). On a successful write to secondary when primary
/// held stale data, deletes primary to avoid shadowing.
pub struct FallbackStorage {
    primary: Box<dyn SecureStorage>,
    secondary: Box<dyn SecureStorage>,
}

impl FallbackStorage {
    pub fn new(primary: Box<dyn SecureStorage>, secondary: Box<dyn SecureStorage>) -> Self {
        Self { primary, secondary }
    }
}

impl SecureStorage for FallbackStorage {
    fn name(&self) -> String {
        format!("{}-with-{}-fallback", self.primary.name(), self.secondary.name())
    }

    fn read(&self) -> Result<Option<SecureStorageData>> {
        match self.primary.read() {
            Ok(Some(data)) => Ok(Some(data)),
            Ok(None) => self.secondary.read(),
            Err(e) => {
                warn!("primary storage {} read failed: {e}, trying fallback", self.primary.name());
                self.secondary.read()
            }
        }
    }

    fn write(&self, data: &SecureStorageData) -> Result<()> {
        let primary_before = self.primary.read().ok().flatten();

        if matches!(self.primary.write(data), Ok(())) {
            // Migration: if this is the first successful primary write, delete the stale fallback.
            if primary_before.is_none() {
                let _ = self.secondary.delete();
            }
            return Ok(());
        }

        warn!(
            "primary storage {} write failed, falling back to {}",
            self.primary.name(),
            self.secondary.name()
        );
        self.secondary.write(data)?;

        // Prevent stale primary data from shadowing the fresh fallback write.
        if primary_before.is_some() {
            let _ = self.primary.delete();
        }
        Ok(())
    }

    fn delete(&self) -> Result<()> {
        let _ = self.primary.delete();
        let _ = self.secondary.delete();
        Ok(())
    }
}

// ── Platform Default ────────────────────────────────────────────────────────

/// Returns the platform-appropriate secure storage.
///
/// - macOS: Keyring with plaintext fallback
/// - Linux:  Keyring with plaintext fallback (libsecret or kwallet required)
/// - Windows: Credential Manager with plaintext fallback
pub fn default_storage() -> Result<Box<dyn SecureStorage>> {
    let keyring = KeyringStorage::new("clawed-code", whoami::username());
    let plaintext = PlaintextStorage::new_default()?;
    Ok(Box::new(FallbackStorage::new(Box::new(keyring), Box::new(plaintext))))
}

// ── Convenience API ─────────────────────────────────────────────────────────

/// Global singleton for the default secure storage.
static DEFAULT_STORAGE: Mutex<Option<Box<dyn SecureStorage>>> = Mutex::new(None);

/// Initialize the global default storage. Idempotent.
pub fn init_default_storage() -> Result<()> {
    let mut guard = DEFAULT_STORAGE.lock().unwrap_or_else(|e| e.into_inner());
    if guard.is_none() {
        *guard = Some(default_storage()?);
    }
    Ok(())
}

/// Read from the global default storage.
pub fn read() -> Result<Option<SecureStorageData>> {
    let guard = DEFAULT_STORAGE.lock().unwrap_or_else(|e| e.into_inner());
    match guard.as_ref() {
        Some(s) => s.read(),
        None => Err(anyhow::anyhow!("secure storage not initialized")),
    }
}

/// Write to the global default storage.
pub fn write(data: &SecureStorageData) -> Result<()> {
    let guard = DEFAULT_STORAGE.lock().unwrap_or_else(|e| e.into_inner());
    match guard.as_ref() {
        Some(s) => s.write(data),
        None => Err(anyhow::anyhow!("secure storage not initialized")),
    }
}

/// Delete from the global default storage.
pub fn delete() -> Result<()> {
    let guard = DEFAULT_STORAGE.lock().unwrap_or_else(|e| e.into_inner());
    match guard.as_ref() {
        Some(s) => s.delete(),
        None => Err(anyhow::anyhow!("secure storage not initialized")),
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use tempfile::TempDir;

    #[test]
    fn plaintext_storage_roundtrip() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("creds.json");
        let storage = PlaintextStorage::new(&path);

        assert!(storage.read().unwrap().is_none());

        let mut data = HashMap::new();
        data.insert("api_key".into(), "secret123".into());
        storage.write(&data).unwrap();

        let read_back = storage.read().unwrap().unwrap();
        assert_eq!(read_back.get("api_key"), Some(&"secret123".to_string()));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::metadata(&path).unwrap().permissions();
            assert_eq!(perms.mode() & 0o777, 0o600);
        }
    }

    #[test]
    fn plaintext_storage_delete() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("creds.json");
        let storage = PlaintextStorage::new(&path);

        let mut data = HashMap::new();
        data.insert("k".into(), "v".into());
        storage.write(&data).unwrap();
        assert!(path.exists());

        storage.delete().unwrap();
        assert!(!path.exists());

        // Deleting again is idempotent
        storage.delete().unwrap();
    }

    #[test]
    fn fallback_storage_uses_primary_when_available() {
        let dir = TempDir::new().unwrap();
        let primary = PlaintextStorage::new(dir.path().join("primary.json"));
        let secondary = PlaintextStorage::new(dir.path().join("secondary.json"));
        let fallback = FallbackStorage::new(Box::new(primary), Box::new(secondary));

        let mut data = HashMap::new();
        data.insert("token".into(), "abc".into());
        fallback.write(&data).unwrap();

        // Should be in primary
        assert!(dir.path().join("primary.json").exists());

        let read_back = fallback.read().unwrap().unwrap();
        assert_eq!(read_back.get("token"), Some(&"abc".to_string()));
    }

    #[test]
    fn fallback_storage_falls_back_when_primary_empty() {
        let dir = TempDir::new().unwrap();
        let primary = PlaintextStorage::new(dir.path().join("primary.json"));
        let secondary = PlaintextStorage::new(dir.path().join("secondary.json"));
        let fallback = FallbackStorage::new(Box::new(primary), Box::new(secondary));

        // Write to secondary directly
        let mut data = HashMap::new();
        data.insert("token".into(), "fallback_value".into());
        fallback.secondary.write(&data).unwrap();

        // Fallback read should find secondary
        let read_back = fallback.read().unwrap().unwrap();
        assert_eq!(read_back.get("token"), Some(&"fallback_value".to_string()));
    }
}
