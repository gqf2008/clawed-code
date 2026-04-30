//! QueryEngine — the core agent orchestration engine.
//!
//! The engine drives the multi-turn agentic loop: accepting user prompts,
//! streaming API responses, executing tools, and managing conversation state.

mod builder;
mod compact;
mod history;
mod impl_traits;
mod session_ops;
mod submit;
pub use builder::QueryEngineBuilder;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use clawed_api::client::ApiClient;
use clawed_api::types::{CacheControl, ThinkingConfig, ToolDefinition};
use clawed_bus::AgentNotification;
use clawed_core::message::{Message, SystemMessage};
use clawed_core::sync::lock_or_recover;
use clawed_core::tool::AbortSignal;
use clawed_tools::ToolRegistry;

use crate::compact::AutoCompactState;
use crate::coordinator::TaskNotification;
use crate::cost::CostTracker;
use crate::dispatch_agent::{AgentChannelMap, CancelTokenMap};
use crate::executor::ToolExecutor;
use crate::hooks::{HookDecision, HookEvent, HookRegistry};
use crate::query::QueryConfig;

/// Runtime thinking override state.
/// Uses a dedicated enum to avoid clippy::option_option.
#[derive(Debug, Clone)]
enum ThinkingOverride {
    /// No override — use default config.
    UseDefault,
    /// Override: disable thinking.
    Disabled,
    /// Override: enable with this config.
    Enabled(ThinkingConfig),
}
use crate::state::SharedState;
use crate::task_runner::{run_task, TaskProgress, TaskResult};
use clawed_core::permissions::PermissionMode;

pub struct QueryEngine {
    client: Arc<ApiClient>,
    executor: Arc<ToolExecutor>,
    registry: Arc<ToolRegistry>,
    state: SharedState,
    config: QueryConfig,
    hooks: Arc<HookRegistry>,
    cwd: std::path::PathBuf,
    session_id: String,
    /// Timestamp when this session was created.
    created_at: chrono::DateTime<chrono::Utc>,
    compact_threshold: u64,
    /// Shared abort signal — call `.abort()` to cancel the running task.
    abort_signal: AbortSignal,
    /// Coordinator mode: receives task notifications from background agents.
    notification_rx:
        Option<tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<TaskNotification>>>,
    /// Whether coordinator mode is active.
    coordinator_mode: bool,
    /// If non-empty, only expose these tools to the model.
    allowed_tools: Vec<String>,
    /// Tracks accumulated API usage costs per model.
    cost_tracker: CostTracker,
    /// Sub-agent cancellation tokens (coordinator mode only).
    cancel_tokens: Option<CancelTokenMap>,
    /// Sub-agent message channels (coordinator mode only).
    agent_channels: Option<AgentChannelMap>,
    /// Auto-compact state machine (circuit breaker, dynamic threshold).
    /// Shared via `Arc` so the query loop can reuse the same state across submits.
    auto_compact: Arc<tokio::sync::Mutex<AutoCompactState>>,
    /// Model context window size (for auto-compact threshold calculation).
    context_window: u64,
    /// If true, the next API request should skip prompt caching.
    break_cache_next: AtomicBool,
    /// Runtime-mutable thinking config (toggled via /think command).
    thinking_override: std::sync::Mutex<ThinkingOverride>,
    /// Receives bus `AgentNotification`s emitted by background sub-agents.
    /// Drained by `drain_agent_notifications()` and forwarded to the bus adapter.
    pub agent_notif_rx:
        Option<tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<AgentNotification>>>,
    /// Temporary tool whitelist for the next skill submission.
    skill_allowed_tools: std::sync::Mutex<Vec<String>>,
    /// Lazily-built session context (git status + date), prepended once.
    session_context: std::sync::OnceLock<String>,
}

impl QueryEngine {
    pub fn builder(
        api_key: impl Into<String>,
        cwd: impl Into<std::path::PathBuf>,
    ) -> QueryEngineBuilder {
        QueryEngineBuilder::new(api_key, cwd)
    }

    fn tool_definitions(&self, permission_mode: PermissionMode) -> Vec<ToolDefinition> {
        let skill_tools_guard = lock_or_recover(&self.skill_allowed_tools);
        if skill_tools_guard.is_empty() {
            self.build_tool_definitions(permission_mode, &self.allowed_tools)
        } else {
            let skill_tools = skill_tools_guard.clone();
            self.build_tool_definitions(permission_mode, &skill_tools)
        }
    }

    fn build_tool_definitions(
        &self,
        permission_mode: PermissionMode,
        effective_allowed: &[String],
    ) -> Vec<ToolDefinition> {
        let mut defs: Vec<ToolDefinition> = self
            .registry
            .all()
            .iter()
            .filter(|t| t.is_enabled())
            .filter(|t| {
                effective_allowed.is_empty()
                    || effective_allowed
                        .iter()
                        .any(|a| a.eq_ignore_ascii_case(t.name()))
            })
            // In plan mode, only expose read-only tools to the model
            .filter(|t| {
                if permission_mode == PermissionMode::Plan {
                    clawed_tools::plan_mode::is_plan_mode_tool(t.name()) || t.is_read_only()
                } else {
                    true
                }
            })
            .map(|t| ToolDefinition {
                name: t.name().to_string(),
                description: t.description().to_string(),
                input_schema: t.input_schema(),
                cache_control: None,
            })
            .collect();

        // Enable prompt caching on the last tool definition (mirrors TS behavior)
        if let Some(last) = defs.last_mut() {
            last.cache_control = Some(CacheControl::ephemeral());
        }
        defs
    }

    pub fn state(&self) -> &SharedState {
        &self.state
    }

    /// Build a `QueryConfig` with shared auto-compact state from this engine.
    /// Consumes the one-shot break-cache flag if set.
    fn build_query_config(&self) -> QueryConfig {
        let session_context = self
            .session_context
            .get()
            .filter(|s| !s.is_empty())
            .cloned();
        QueryConfig {
            system_prompt: self.config.system_prompt.clone(),
            max_turns: self.config.max_turns,
            max_tokens: self.config.max_tokens,
            temperature: self.config.temperature,
            thinking: self.thinking_config(),
            token_budget: self.config.token_budget,
            context_window: self.context_window,
            auto_compact_state: Some(Arc::clone(&self.auto_compact)),
            break_cache: self.take_break_cache(),
            session_context,
        }
    }

    /// Set the temporary tool whitelist for the next skill submission.
    /// Call `clear_skill_allowed_tools()` after the skill turn completes.
    pub fn set_skill_allowed_tools(&self, allowed_tools: Vec<String>) {
        let n = allowed_tools.len();
        *lock_or_recover(&self.skill_allowed_tools) = allowed_tools;
        tracing::info!("[skill] set_skill_allowed_tools: {n} tools");
    }

    /// Clear the temporary skill tool whitelist.
    pub fn clear_skill_allowed_tools(&self) {
        let mut guard = lock_or_recover(&self.skill_allowed_tools);
        let had = !guard.is_empty();
        guard.clear();
        tracing::info!("[skill] clear_skill_allowed_tools (had_tools={had})");
    }

    /// Get the cost tracker for displaying usage stats.
    pub fn cost_tracker(&self) -> &CostTracker {
        &self.cost_tracker
    }

    /// Number of tools registered in the tool registry.
    pub fn tool_count(&self) -> usize {
        self.registry.len()
    }

    /// Whether this engine is in coordinator (multi-agent) mode.
    pub fn is_coordinator(&self) -> bool {
        self.coordinator_mode
    }

    /// Drain any pending task notifications from background agents.
    /// Returns them as user-role messages containing `<task-notification>` XML.
    /// Call this between turns in the REPL to inject notifications into the conversation.
    pub async fn drain_notifications(&self) -> Vec<Message> {
        let rx = match &self.notification_rx {
            Some(rx) => rx,
            None => return Vec::new(),
        };
        let mut rx = rx.lock().await;
        let mut messages = Vec::new();
        while let Ok(notification) = rx.try_recv() {
            messages.push(notification.to_message());
        }
        messages
    }

    /// Inject a context note into the conversation as a system message without triggering an API call.
    /// Used by `/btw` to add side-channel notes visible to the model on the next turn.
    pub async fn inject_context(&self, text: &str) {
        let msg = Message::System(SystemMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            message: format!("[btw] {text}"),
        });
        self.state().write().await.messages.push(msg);
    }

    /// Drain any pending bus `AgentNotification`s emitted by background sub-agents.
    /// Called by the bus adapter to forward these notifications to all TUI clients.
    pub fn drain_agent_notifications(&self) -> Vec<AgentNotification> {
        let Some(rx) = &self.agent_notif_rx else {
            return Vec::new();
        };
        let Ok(mut guard) = rx.try_lock() else {
            return Vec::new();
        };
        let mut result = Vec::new();
        while let Ok(n) = guard.try_recv() {
            result.push(n);
        }
        result
    }

    /// Get a clone of the abort signal so callers can cancel the running task.
    /// Call `.abort()` on the returned signal to interrupt tool execution and
    /// stop the agent loop at the next opportunity.
    pub fn abort_signal(&self) -> AbortSignal {
        self.abort_signal.clone()
    }

    /// Abort the current task (equivalent to Ctrl-C in the TS implementation).
    pub fn abort(&self) {
        self.abort_signal.abort();
    }

    /// Send a message to a running background sub-agent.
    ///
    /// Returns an error if not in coordinator mode or if the agent is not found.
    pub async fn send_to_agent(&self, agent_id: &str, message: &str) -> anyhow::Result<()> {
        let channels = self
            .agent_channels
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Not in coordinator mode"))?;
        let channels = channels.read().await;
        let tx = channels
            .get(agent_id)
            .ok_or_else(|| anyhow::anyhow!("Agent '{}' not found or not running", agent_id))?;
        tx.send(message.to_string())
            .map_err(|_| anyhow::anyhow!("Agent '{}' channel closed", agent_id))?;
        Ok(())
    }

    /// Cancel a running background sub-agent.
    ///
    /// Returns an error if not in coordinator mode or if the agent is not found.
    pub async fn cancel_agent(&self, agent_id: &str) -> anyhow::Result<()> {
        let tokens = self
            .cancel_tokens
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Not in coordinator mode"))?;
        let tokens = tokens.read().await;
        let token = tokens
            .get(agent_id)
            .ok_or_else(|| anyhow::anyhow!("Agent '{}' not found or not running", agent_id))?;
        token.cancel();
        Ok(())
    }

    /// Return tool names and descriptions for the ListTools bus request.
    pub fn tool_list(&self) -> Vec<(String, String, bool)> {
        self.registry
            .all()
            .iter()
            .map(|t| {
                (
                    t.name().to_string(),
                    t.description().to_string(),
                    t.is_enabled(),
                )
            })
            .collect()
    }

    /// Access the hook registry (for firing lifecycle events from task_runner, etc.)
    pub(crate) fn hooks(&self) -> &Arc<HookRegistry> {
        &self.hooks
    }

    /// Called by the settings watcher after a write to settings.json is detected.
    pub async fn fire_config_change_hook(&self) {
        if self.hooks.has_hooks(HookEvent::ConfigChange) {
            let ctx = self.hooks.lifecycle_ctx(HookEvent::ConfigChange);
            let _ = self.hooks.run(HookEvent::ConfigChange, ctx).await;
        }
    }

    /// Called by the file watcher when a watched file is modified.
    pub async fn fire_file_changed_hook(&self, path: &str) {
        if self.hooks.has_hooks(HookEvent::FileChanged) {
            let ctx = self.hooks.tool_ctx(
                HookEvent::FileChanged,
                "file_watcher",
                Some(serde_json::json!({ "path": path })),
                None,
                None,
            );
            let _ = self.hooks.run(HookEvent::FileChanged, ctx).await;
        }
    }

    /// Fire the CwdChanged hook with the new directory path.
    pub async fn fire_cwd_changed_hook(&self, new_cwd: &str) {
        if self.hooks.has_hooks(HookEvent::CwdChanged) {
            let ctx = self.hooks.tool_ctx(
                HookEvent::CwdChanged,
                "cwd",
                Some(serde_json::json!({ "cwd": new_cwd })),
                None,
                None,
            );
            let _ = self.hooks.run(HookEvent::CwdChanged, ctx).await;
        }
    }

    /// Fire an Elicitation hook before showing a prompt to the user.
    /// Returns the hook decision (hooks can block or append context).
    pub async fn fire_elicitation_hook(&self, message: &str, schema: &serde_json::Value) -> HookDecision {
        if !self.hooks.has_hooks(HookEvent::Elicitation) {
            return HookDecision::Continue;
        }
        let ctx = self.hooks.tool_ctx(
            HookEvent::Elicitation,
            "elicitation",
            Some(serde_json::json!({ "message": message, "requestedSchema": schema })),
            None,
            None,
        );
        self.hooks.run(HookEvent::Elicitation, ctx).await
    }

    /// Fire an ElicitationResult hook after the user responds.
    pub async fn fire_elicitation_result_hook(&self, action: &str, content: Option<&serde_json::Value>) {
        if !self.hooks.has_hooks(HookEvent::ElicitationResult) {
            return;
        }
        let mut input = serde_json::json!({ "action": action });
        if let Some(c) = content {
            input["content"] = c.clone();
        }
        let ctx = self.hooks.tool_ctx(
            HookEvent::ElicitationResult,
            "elicitation",
            Some(input),
            None,
            None,
        );
        let _ = self.hooks.run(HookEvent::ElicitationResult, ctx).await;
    }

    /// Run a task autonomously to completion, streaming progress events.
    ///
    /// This is the primary entry point for non-interactive / programmatic use.
    /// It drives the full multi-turn agentic loop (planning → tool execution →
    /// verification → delivery) and returns a structured `TaskResult`.
    ///
    /// # Arguments
    /// - `task` — natural-language task description
    /// - `on_progress` — callback invoked for each `TaskProgress` event
    ///
    /// # Example
    /// ```rust,ignore
    /// let result = engine.run_task("Add a README.md with project description", |p| {
    ///     if let TaskProgress::Text(t) = p { print!("{}", t); }
    /// }).await;
    /// println!("Done in {} turns: {}", result.turns, result.reason);
    /// ```
    pub async fn run_task<F>(&self, task: &str, on_progress: F) -> TaskResult
    where
        F: FnMut(TaskProgress) + Send,
    {
        run_task(self, task, on_progress).await
    }

    /// Return the session ID (used by hooks).
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Install a bus-based permission prompter so tool permission checks go
    /// through the event bus instead of directly to the terminal. Call this
    /// before running any queries when running in TUI or RPC mode.
    pub fn set_permission_prompter(
        &self,
        prompter: Arc<dyn crate::permissions::PermissionPrompter>,
    ) {
        self.executor.set_prompter(prompter);
    }

    /// Run SessionStart hooks — call once at startup.
    /// Also fires Setup hook on first use of this project.
    pub async fn run_session_start(&self) -> Option<String> {
        // ── Setup hook (first use only) ─────────────────────────────────────
        let setup_marker = self.cwd.join(".claude").join("setup_complete");
        if !setup_marker.exists() {
            if self.hooks.has_hooks(HookEvent::Setup) {
                let ctx = self.hooks.lifecycle_ctx(HookEvent::Setup);
                let _ = self.hooks.run(HookEvent::Setup, ctx).await;
            }
            // Create marker so Setup never fires again for this project
            if let Some(parent) = setup_marker.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(&setup_marker, "done");
        }

        let mut appended = None;
        if self.hooks.has_hooks(HookEvent::SessionStart) {
            let ctx = self.hooks.prompt_ctx(HookEvent::SessionStart, None);
            match self.hooks.run(HookEvent::SessionStart, ctx).await {
                HookDecision::AppendContext { text } => appended = Some(text),
                _ => {}
            }
        }
        // Fire InstructionsLoaded after session start — system prompt is now assembled.
        if self.hooks.has_hooks(HookEvent::InstructionsLoaded) {
            let ctx = self.hooks.lifecycle_ctx(HookEvent::InstructionsLoaded);
            let _ = self.hooks.run(HookEvent::InstructionsLoaded, ctx).await;
        }
        appended
    }

    // ── Accessors and runtime config ─────────────────────────────────────────

    /// Lazily build and return the session-start context as a formatted
    /// `<system-reminder>` message, ready to prepend to the message list.
    /// Returns `None` if no context could be collected.
    pub async fn session_context_message(&self) -> Option<String> {
        if let Some(cached) = self.session_context.get() {
            if cached.is_empty() {
                return None;
            }
            return Some(crate::context::format_context_message(cached));
        }
        let ctx = crate::context::build_session_context(&self.cwd).await;
        match ctx {
            Some(text) => {
                let _ = self.session_context.set(text.clone());
                Some(crate::context::format_context_message(&text))
            }
            None => {
                let _ = self.session_context.set(String::new());
                None
            }
        }
    }

    /// Get the working directory.
    pub fn cwd(&self) -> &std::path::Path {
        &self.cwd
    }

    /// Update the CLAUDE.md portion of the system prompt (for /reload-context).
    pub async fn update_system_prompt_context(&self, claude_md: &str) {
        let mut s = self.state.write().await;
        // Store the refreshed CLAUDE.md content for next query_stream call.
        // The system prompt is rebuilt each query from config, so we just
        // note that context was reloaded.
        s.context_reloaded = true;
        s.claude_md_content = claude_md.to_string();
        drop(s);
        // Fire InstructionsLoaded hook — instructions were reloaded at runtime.
        if self.hooks.has_hooks(HookEvent::InstructionsLoaded) {
            let ctx = self.hooks.lifecycle_ctx(HookEvent::InstructionsLoaded);
            let _ = self.hooks.run(HookEvent::InstructionsLoaded, ctx).await;
        }
    }

    /// Get the current thinking configuration.
    pub fn thinking_config(&self) -> Option<ThinkingConfig> {
        if let Ok(guard) = self.thinking_override.lock() {
            match &*guard {
                ThinkingOverride::UseDefault => {}
                ThinkingOverride::Disabled => return None,
                ThinkingOverride::Enabled(cfg) => return Some(cfg.clone()),
            }
        }
        self.config.thinking.clone()
    }

    /// Set the thinking configuration at runtime (/think command).
    pub fn set_thinking(&self, config: Option<ThinkingConfig>) {
        if let Ok(mut guard) = self.thinking_override.lock() {
            *guard = match config {
                None => ThinkingOverride::Disabled,
                Some(cfg) => ThinkingOverride::Enabled(cfg),
            };
        }
    }

    /// Check if prompt cache breaking is requested for the next turn, and clear the flag.
    pub fn take_break_cache(&self) -> bool {
        self.break_cache_next.swap(false, Ordering::SeqCst)
    }

    /// Request that the next API call skips prompt caching.
    pub fn set_break_cache(&self) {
        self.break_cache_next.store(true, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests;
