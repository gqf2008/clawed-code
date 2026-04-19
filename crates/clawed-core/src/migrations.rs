//! Settings & state migration framework.
//!
//! Provides a registry of versioned migrations that run once at application startup.
//! Applied migrations are tracked in `~/.claude/migrations.json` so each migration
//! only executes once per user environment.
//!
//! Design inspired by the TypeScript `migrations/` module in the original project.

use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tracing::{debug, error, info, warn};

/// A single state migration.
pub trait Migration: Send + Sync {
    /// Unique human-readable name (e.g. `migrate_plaintext_oauth_to_keyring`).
    fn name(&self) -> &str;

    /// Monotonic version number. Higher versions run after lower ones.
    fn version(&self) -> u32;

    /// Execute the migration. Must be idempotent (safe to re-run).
    fn run(&self) -> Result<()>;
}

/// Tracks which migrations have already been applied.
#[derive(Debug, Default, Serialize, Deserialize)]
struct MigrationState {
    /// Set of applied migration names.
    applied: HashSet<String>,
    /// Highest version that has been executed.
    last_version: u32,
}

/// Runner that discovers, orders, and executes pending migrations.
pub struct MigrationRunner {
    state: MigrationState,
    state_path: PathBuf,
    migrations: Vec<Box<dyn Migration>>,
}

impl MigrationRunner {
    /// Load or create the migration runner with the default state file path.
    pub fn load() -> Result<Self> {
        let path = Self::default_state_path()?;
        Self::load_from(path)
    }

    /// Load from an explicit state file path (useful for tests).
    pub fn load_from(state_path: PathBuf) -> Result<Self> {
        let state = if state_path.exists() {
            let contents = fs::read_to_string(&state_path)
                .with_context(|| format!("failed to read migration state {}", state_path.display()))?;
            serde_json::from_str(&contents)
                .with_context(|| format!("invalid migration state JSON in {}", state_path.display()))?
        } else {
            MigrationState::default()
        };

        Ok(Self {
            state,
            state_path,
            migrations: Vec::new(),
        })
    }

    /// Default state file: `~/.claude/migrations.json`
    fn default_state_path() -> Result<PathBuf> {
        let home = dirs::home_dir().context("could not determine home directory")?;
        Ok(home.join(".claude").join("migrations.json"))
    }

    /// Register a migration. Migrations are sorted by version at run time.
    pub fn register(&mut self, migration: Box<dyn Migration>) {
        self.migrations.push(migration);
    }

    /// Register multiple migrations at once.
    pub fn register_many(&mut self, migrations: Vec<Box<dyn Migration>>) {
        self.migrations.extend(migrations);
    }

    /// Run all pending migrations in version order, skipping already-applied ones.
    /// Returns the list of migration names that were executed.
    pub fn run_pending(&mut self) -> Result<Vec<String>> {
        self.migrations.sort_by_key(|m| m.version());

        let mut executed = Vec::new();
        for migration in &self.migrations {
            if self.state.applied.contains(migration.name()) {
                debug!("migration {} already applied, skipping", migration.name());
                continue;
            }

            info!(
                "running migration v{}: {}",
                migration.version(),
                migration.name()
            );

            match migration.run() {
                Ok(()) => {
                    self.state.applied.insert(migration.name().to_string());
                    if migration.version() > self.state.last_version {
                        self.state.last_version = migration.version();
                    }
                    executed.push(migration.name().to_string());
                    info!("migration {} completed", migration.name());
                }
                Err(e) => {
                    // Log but do not abort — a failed migration should not brick the app.
                    // The migration can be retried on next startup.
                    error!("migration {} failed: {e}", migration.name());
                }
            }
        }

        self.save_state()?;
        Ok(executed)
    }

    fn save_state(&self) -> Result<()> {
        let dir = self.state_path.parent().context("migration state path has no parent")?;
        fs::create_dir_all(dir)?;
        let json = serde_json::to_string_pretty(&self.state)?;
        fs::write(&self.state_path, json)
            .with_context(|| format!("failed to write migration state {}", self.state_path.display()))?;
        Ok(())
    }

    /// Returns the number of registered migrations.
    pub fn len(&self) -> usize {
        self.migrations.len()
    }

    pub fn is_empty(&self) -> bool {
        self.migrations.is_empty()
    }
}

// ── Built-in Migrations ─────────────────────────────────────────────────────

/// Ensures the `~/.claude` directory exists with sensible default structure.
pub struct EnsureClaudeDirMigration;

impl Migration for EnsureClaudeDirMigration {
    fn name(&self) -> &str {
        "ensure_claude_dir_structure"
    }

    fn version(&self) -> u32 {
        1
    }

    fn run(&self) -> Result<()> {
        let home = dirs::home_dir().context("no home directory")?;
        let claude_dir = home.join(".claude");
        fs::create_dir_all(&claude_dir)?;
        fs::create_dir_all(claude_dir.join("skills"))?;
        fs::create_dir_all(claude_dir.join("rules"))?;
        fs::create_dir_all(claude_dir.join("hooks"))?;
        Ok(())
    }
}

/// Migrates legacy `oauth_token.json` naming if present.
/// The original TypeScript project used various filenames over time.
pub struct MigrateLegacyOAuthTokenFilename;

impl Migration for MigrateLegacyOAuthTokenFilename {
    fn name(&self) -> &str {
        "migrate_legacy_oauth_token_filename"
    }

    fn version(&self) -> u32 {
        2
    }

    fn run(&self) -> Result<()> {
        let home = dirs::home_dir().context("no home directory")?;
        let claude_dir = home.join(".claude");

        // Old names that may exist from earlier versions
        let legacy_names = ["auth_token.json", "token.json", "claude_token.json"];
        for name in &legacy_names {
            let legacy = claude_dir.join(name);
            let current = claude_dir.join("oauth_token.json");
            if legacy.exists() && !current.exists() {
                fs::rename(&legacy, &current)
                    .with_context(|| format!("failed to rename {} to oauth_token.json", legacy.display()))?;
                info!("renamed legacy token file {} → oauth_token.json", legacy.display());
            }
        }
        Ok(())
    }
}

/// Removes stale session export files older than 7 days from `~/.claude/exports/`.
pub struct CleanupStaleSessionExports;

impl Migration for CleanupStaleSessionExports {
    fn name(&self) -> &str {
        "cleanup_stale_session_exports"
    }

    fn version(&self) -> u32 {
        3
    }

    fn run(&self) -> Result<()> {
        let home = dirs::home_dir().context("no home directory")?;
        let exports_dir = home.join(".claude").join("exports");
        if !exports_dir.exists() {
            return Ok(());
        }

        let now = std::time::SystemTime::now();
        let max_age = std::time::Duration::from_secs(7 * 24 * 60 * 60); // 7 days

        for entry in fs::read_dir(&exports_dir)? {
            let entry = entry?;
            let path = entry.path();
            if let Ok(meta) = entry.metadata() {
                if let Ok(modified) = meta.modified() {
                    if let Ok(age) = now.duration_since(modified) {
                        if age > max_age {
                            if let Err(e) = fs::remove_file(&path) {
                                warn!("failed to remove stale export {}: {e}", path.display());
                            } else {
                                debug!("removed stale session export {}", path.display());
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

/// Convenience: create a runner with all built-in migrations pre-registered.
pub fn default_runner() -> Result<MigrationRunner> {
    let mut runner = MigrationRunner::load()?;
    runner.register(Box::new(EnsureClaudeDirMigration));
    runner.register(Box::new(MigrateLegacyOAuthTokenFilename));
    runner.register(Box::new(CleanupStaleSessionExports));
    Ok(runner)
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use tempfile::TempDir;

    struct CountingMigration {
        name: String,
        version: u32,
        counter: Arc<AtomicUsize>,
    }

    impl Migration for CountingMigration {
        fn name(&self) -> &str {
            &self.name
        }
        fn version(&self) -> u32 {
            self.version
        }
        fn run(&self) -> Result<()> {
            self.counter.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    #[test]
    fn runner_executes_pending_migrations() {
        let dir = TempDir::new().unwrap();
        let state_path = dir.path().join("migrations.json");
        let mut runner = MigrationRunner::load_from(state_path).unwrap();

        let counter = Arc::new(AtomicUsize::new(0));
        runner.register(Box::new(CountingMigration {
            name: "m1".into(),
            version: 1,
            counter: counter.clone(),
        }));
        runner.register(Box::new(CountingMigration {
            name: "m2".into(),
            version: 2,
            counter: counter.clone(),
        }));

        let executed = runner.run_pending().unwrap();
        assert_eq!(executed.len(), 2);
        assert_eq!(counter.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn runner_skips_already_applied() {
        let dir = TempDir::new().unwrap();
        let state_path = dir.path().join("migrations.json");
        let mut runner = MigrationRunner::load_from(state_path.clone()).unwrap();

        let counter = Arc::new(AtomicUsize::new(0));
        runner.register(Box::new(CountingMigration {
            name: "m1".into(),
            version: 1,
            counter: counter.clone(),
        }));

        // First run
        let _ = runner.run_pending().unwrap();
        assert_eq!(counter.load(Ordering::SeqCst), 1);

        // Second run — should skip
        let mut runner2 = MigrationRunner::load_from(state_path).unwrap();
        runner2.register(Box::new(CountingMigration {
            name: "m1".into(),
            version: 1,
            counter: counter.clone(),
        }));
        let executed = runner2.run_pending().unwrap();
        assert!(executed.is_empty());
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn runner_runs_in_version_order() {
        let dir = TempDir::new().unwrap();
        let state_path = dir.path().join("migrations.json");
        let mut runner = MigrationRunner::load_from(state_path).unwrap();

        let order: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

        for v in [3, 1, 2] {
            let o = order.clone();
            let name = format!("m{v}");
            runner.register(Box::new(OrderedMigration { name: name.clone(), version: v, order: o }));
        }

        let executed = runner.run_pending().unwrap();
        assert_eq!(executed, vec!["m1", "m2", "m3"]);
    }

    struct OrderedMigration {
        name: String,
        version: u32,
        order: Arc<Mutex<Vec<String>>>,
    }

    impl Migration for OrderedMigration {
        fn name(&self) -> &str {
            &self.name
        }
        fn version(&self) -> u32 {
            self.version
        }
        fn run(&self) -> Result<()> {
            crate::sync::lock_or_recover(&self.order).push(self.name.clone());
            Ok(())
        }
    }
}
