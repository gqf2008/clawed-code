//! Plugin system — extend claude-code-rs with local plugins.
//!
//! Plugins are directories containing a `plugin.json` manifest that can:
//! - Register new slash commands (from `.md` prompt files)
//! - Add skills (from `.md` skill files)
//! - Define hooks (pre/post command scripts)
//! - Configure MCP servers
//!
//! Plugin search paths (in priority order):
//! 1. `.claude/plugins/` — project-local plugins
//! 2. `~/.claude/plugins/` — user-global plugins
//!
//! This is a simplified local-only plugin system — no marketplace or remote install.

mod loader;
mod manifest;

pub use loader::{LoadedPlugin, PluginLoader};
pub use manifest::{HookEvent, PluginCommand, PluginHook, PluginManifest, PluginSkill};
