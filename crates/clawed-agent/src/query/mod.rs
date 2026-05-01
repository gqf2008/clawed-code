mod helpers;

use futures::Stream;
use std::pin::Pin;
use std::sync::Arc;
use tracing::{info, warn};
use uuid::Uuid;

use crate::compact::{
    compact_context_message, compact_conversation, partial_compact_conversation, AutoCompactState,
};
use crate::executor::ToolExecutor;
use crate::hooks::{HookDecision, HookEvent, HookRegistry};
use crate::state::SharedState;
use clawed_api::client::ApiClient;
use clawed_api::types::*;
use clawed_core::message::{
    AssistantMessage, ContentBlock, Message, StopReason, Usage, UserMessage,
};
use clawed_core::tool::ToolContext;

use helpers::*;

/// Extract file paths from a tool's input JSON for conditional skill activation.
fn extract_tool_paths(input: &serde_json::Value) -> Vec<String> {
    let mut paths = Vec::new();
    if let Some(path) = input.get("file_path").and_then(|v| v.as_str()) {
        paths.push(path.to_string());
    } else if let Some(path) = input.get("path").and_then(|v| v.as_str()) {
        paths.push(path.to_string());
    } else if let Some(pattern) = input.get("pattern").and_then(|v| v.as_str()) {
        paths.push(pattern.to_string());
    }
    paths
}

#[derive(Debug, Clone)]
pub enum AgentEvent {
    TextDelta(String),
    ThinkingDelta(String),
    ToolUseStart {
        id: String,
        name: String,
    },
    /// Emitted when tool input is fully parsed (at ContentBlockStop).
    ToolUseReady {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    /// Live output line from a running tool.
    ToolOutputLine {
        id: String,
        name: String,
        line: String,
    },
    ToolResult {
        id: String,
        is_error: bool,
        text: Option<String>,
    },
    AssistantMessage(AssistantMessage),
    TurnComplete {
        stop_reason: StopReason,
    },
    UsageUpdate(Usage),
    /// Per-turn token counts for budget tracking.
    TurnTokens {
        input_tokens: u64,
        output_tokens: u64,
    },
    /// Prompt is getting too large — may need compaction soon.
    ContextWarning {
        usage_pct: f64,
        message: String,
    },
    /// Auto-compaction triggered.
    CompactStart,
    /// Compaction finished successfully.
    CompactComplete {
        summary_len: usize,
    },
    /// Max turns limit reached.
    MaxTurns {
        limit: u32,
    },
    /// Conditional skills were activated based on file paths touched by tools.
    SkillsActivated { names: Vec<String> },
    Error(String),
}

pub struct QueryConfig {
    pub system_prompt: String,
    pub max_turns: u32,
    pub max_tokens: u32,
    pub temperature: Option<f32>,
    pub thinking: Option<clawed_api::types::ThinkingConfig>,
    /// Token budget for this query (0 = unlimited).
    pub token_budget: u64,
    /// Model context window size (for accurate percentage display).
    /// Overridden at runtime by engine builder from model capabilities.
    pub context_window: u64,
    /// Auto-compact state for proactive in-loop compaction.
    /// When `Some`, the query loop will trigger compaction when token usage
    /// approaches the context window limit (between tool-use turns).
    pub auto_compact_state: Option<Arc<tokio::sync::Mutex<AutoCompactState>>>,
    /// If true, skip cache_control markers on this query (one-shot /break-cache).
    pub break_cache: bool,
    /// Session context (date + git status) for post-compact re-injection.
    /// If set, this is appended after compact boundary messages so the model
    /// retains time/repo awareness across compaction.
    pub session_context: Option<String>,
}

impl Default for QueryConfig {
    fn default() -> Self {
        Self {
            system_prompt: String::new(),
            max_turns: 100,
            max_tokens: 16384,
            temperature: None,
            thinking: None,
            token_budget: 0,
            context_window: 200_000, // fallback; prefer runtime value from model capabilities
            auto_compact_state: None,
            break_cache: false,
            session_context: None,
        }
    }
}

/// Core agent loop: send messages → process stream → execute tools → repeat
#[allow(clippy::too_many_arguments)]
pub fn query_stream(
    client: Arc<ApiClient>,
    executor: Arc<ToolExecutor>,
    state: SharedState,
    tool_context: ToolContext,
    config: QueryConfig,
    initial_messages: Vec<Message>,
    tools: Vec<ToolDefinition>,
    hooks: Arc<HookRegistry>,
) -> Pin<Box<dyn Stream<Item = AgentEvent> + Send>> {
    query_stream_with_injection(
        client,
        executor,
        state,
        tool_context,
        config,
        initial_messages,
        tools,
        hooks,
        None,
    )
}

/// Attempt to trim the system prompt before triggering expensive API compaction.
///
/// Returns `Some((trimmed_prompt, new_total_tokens))` if trimming saved tokens,
/// or `None` if no trim was possible or beneficial.
fn try_trim_system_prompt(
    current_system_prompt: &str,
    current_tokens: u64,
    msg_tokens: u64,
    context_window: u64,
) -> Option<(String, u64)> {
    use crate::system_prompt::{suggest_trim_level, trim_system_prompt, SectionTrimLevel};
    let trim_level = suggest_trim_level(current_tokens, context_window);
    if trim_level == SectionTrimLevel::Full {
        return None;
    }
    let (trimmed, _) = trim_system_prompt(current_system_prompt, trim_level);
    let new_sys_tokens = clawed_core::token_estimation::estimate_system_tokens(&trimmed);
    let new_tokens = msg_tokens + new_sys_tokens;
    if new_tokens >= current_tokens {
        return None;
    }
    tracing::info!(
        before = current_tokens,
        after = new_tokens,
        level = ?trim_level,
        "Section trim: removed guidance sections"
    );
    Some((trimmed, new_tokens))
}

/// Like [`query_stream`] but accepts an optional channel for mid-stream message
/// injection. Messages received on `inject_rx` are appended as user-role
/// messages at the start of each turn, enabling SendMessage follow-ups.
#[allow(clippy::too_many_arguments)]
pub fn query_stream_with_injection(
    client: Arc<ApiClient>,
    executor: Arc<ToolExecutor>,
    state: SharedState,
    tool_context: ToolContext,
    config: QueryConfig,
    initial_messages: Vec<Message>,
    tools: Vec<ToolDefinition>,
    hooks: Arc<HookRegistry>,
    inject_rx: Option<tokio::sync::mpsc::UnboundedReceiver<String>>,
) -> Pin<Box<dyn Stream<Item = AgentEvent> + Send>> {
    let stream = async_stream::stream! {
        let mut current_system_prompt = config.system_prompt.clone();
        let mut messages = initial_messages;
        let mut tool_context = tool_context;
        let mut turn_count: u32 = 0;
        let mut stop_hook_retries: u32 = 0;
        const MAX_STOP_HOOK_RETRIES: u32 = 3;
        const MAX_STREAM_TIMEOUT_RETRIES: u32 = 3;
        const COMPACT_HIGH_WATER_RATIO: f64 = 0.90;
        const COMPACT_CLEAR_RETAIN_COUNT: usize = 3;
        let mut inject_rx = inject_rx;
        let mut reminders = crate::system_reminder::ReminderCollector::default();

        // Channel for streaming tool output lines during execution.
        // The executor's output_tx is set to this channel; during tool execution,
        // the executor creates a wrapped callback that sends (id, name, line).
        let (output_tx, mut output_rx) = tokio::sync::mpsc::unbounded_channel::<(String, String, String)>();
        executor.set_output_tx(output_tx);
        // Clear the output_line field since the executor handles it directly
        tool_context.output_line = None;

        // ── Recovery state (aligned with TS query.ts) ────────────────────────
        let mut max_tokens_recovery_count: u32 = 0;
        const MAX_TOKENS_RECOVERY_LIMIT: u32 = 3;
        let mut effective_max_tokens = config.max_tokens;
        let mut has_attempted_reactive_compact = false;
        let mut consecutive_errors: u32 = 0;
        let mut retry_delay_ms: u64 = 1_000; // exponential backoff: 1s → 2s → 4s → … → 32s max

        // Track last emitted warning level to avoid spamming the same warning every turn
        let mut last_warning_level: Option<crate::compact::TokenWarningState> = None;

        // Look up model capabilities for smart max_tokens escalation
        let model_name = { state.read().await.model.clone() };
        let caps = clawed_core::model::model_capabilities(&model_name);
        let escalated_max_tokens = caps.upper_max_output;

        loop {
            // Drain any externally injected messages (from SendMessage)
            if let Some(ref mut rx) = inject_rx {
                while let Ok(msg_text) = rx.try_recv() {
                    messages.push(Message::User(UserMessage {
                        uuid: Uuid::new_v4().to_string(),
                        content: vec![ContentBlock::Text {
                            text: format!("[Follow-up message from coordinator]\n{}", msg_text),
                        }],
                    }));
                }
            }

            // Drain any streaming tool output lines from tool executions
            while let Ok((id, name, line)) = output_rx.try_recv() {
                yield AgentEvent::ToolOutputLine { id, name, line };
            }

            // Check abort at the top of every turn
            if tool_context.abort_signal.is_aborted() {
                state.write().await.messages = messages.clone();
                yield AgentEvent::TurnComplete { stop_reason: clawed_core::message::StopReason::EndTurn };
                break;
            }

            if turn_count >= config.max_turns {
                yield AgentEvent::MaxTurns { limit: config.max_turns };
                break;
            }

            let has_thinking = config.thinking.is_some();
            let api_messages = messages_to_api(&messages, config.break_cache, has_thinking);
            let effective_system = if tool_context.permission_mode == clawed_core::permissions::PermissionMode::Plan {
                format!("{}\n\n{}", current_system_prompt, crate::system_prompt::sections::section_plan_mode_constraints())
            } else {
                current_system_prompt.clone()
            };
            let system = build_system_blocks(&effective_system, config.break_cache);

            let request = MessagesRequest {
                model: { state.read().await.model.clone() },
                max_tokens: effective_max_tokens,
                messages: api_messages,
                system,
                tools: if tools.is_empty() { None } else { Some(tools.clone()) },
                stream: true,
                stop_sequences: None,
                temperature: config.temperature,
                top_p: None,
                thinking: config.thinking.clone(),
                tool_choice: None,
            };

            let event_stream = match client.messages_stream(&request).await {
                Ok(s) => s,
                Err(e) => {
                    let err_str = format!("{}", e);
                    consecutive_errors += 1;
                    state.write().await.record_error(error_category(&err_str));

                    match classify_api_error(&err_str, has_attempted_reactive_compact, consecutive_errors, retry_delay_ms) {
                        ApiErrorAction::ReactiveCompact => {
                            has_attempted_reactive_compact = true;
                            yield AgentEvent::TextDelta(
                                "\n\x1b[33m[Prompt too long — trimming context…]\x1b[0m\n".to_string()
                            );
                            // First pass: truncate large tool results
                            let truncated = crate::compact::truncate_large_tool_results(
                                &mut messages, crate::compact::MAX_TOOL_RESULT_CHARS / 2,
                            );
                            // Second pass: snip oldest message pairs, keep last 5
                            let snipped = crate::compact::snip_old_messages(&mut messages, 5);
                            if truncated + snipped > 0 {
                                yield AgentEvent::TextDelta(format!(
                                    "\x1b[33m[Trimmed {} tool result(s), snipped {} message(s)]\x1b[0m\n",
                                    truncated, snipped,
                                ));
                            }
                            // Always retry after reactive compact attempt
                            continue;
                        }
                        ApiErrorAction::Retry { wait_ms } => {
                            if turn_count + 1 < config.max_turns {
                                let jittered = with_jitter(wait_ms, consecutive_errors);
                                yield AgentEvent::TextDelta(format!(
                                    "\n\x1b[33m[Retrying after API error ({}) in {}ms: {}]\x1b[0m\n",
                                    consecutive_errors, jittered, err_str
                                ));
                                tokio::time::sleep(std::time::Duration::from_millis(jittered)).await;
                                retry_delay_ms = (retry_delay_ms * 2).min(32_000);
                                continue;
                            }
                        }
                        ApiErrorAction::Fatal => {}
                    }
                    state.write().await.messages = messages.clone();
                    // Fire StopFailure hook before breaking
                    if hooks.has_hooks(HookEvent::StopFailure) {
                        let ctx = hooks.prompt_ctx(HookEvent::StopFailure, Some(format!("{:#}", e)));
                        let _ = hooks.run(HookEvent::StopFailure, ctx).await;
                    }
                    yield AgentEvent::Error(format!("API error: {:#}", e));
                    break;
                }
            };

            // Wrap the raw stream with idle watchdog
            let watchdog_config = clawed_api::stream::StreamWatchdogConfig::from_env();
            let event_stream = clawed_api::stream::with_idle_watchdog(event_stream, watchdog_config);

            let mut assistant_text = String::new();
            let mut assistant_blocks: Vec<ContentBlock> = Vec::new();
            let mut tool_uses: Vec<(String, String, serde_json::Value)> = Vec::new();
            let mut tool_paths: Vec<String> = Vec::new();
            let mut current_tool_input = String::new();
            let mut current_tool_id = String::new();
            let mut current_tool_name = String::new();
            let mut stop_reason = None;
            let mut usage: Option<Usage> = None;
            let mut should_retry_turn = false;

            use tokio_stream::StreamExt;
            let mut event_stream = event_stream;
            // Spawn tool execution early when stop_reason=tool_use is known, so
            // the tool starts running while we finish consuming the API stream
            // tail (MessageStop, stream close) and do post-processing. This
            // eliminates the structural gap between ToolUseReady and the first
            // ToolOutputLine.
            let mut early_exec: Option<tokio::task::JoinHandle<Vec<ContentBlock>>> = None;
            while let Some(event_result) = event_stream.next().await {
                // Allow user abort (Esc / Ctrl-C) to interrupt the SSE stream
                // mid-flight so output stops immediately rather than waiting for
                // the API to finish the current turn.
                if tool_context.abort_signal.is_aborted() {
                    break;
                }
                match event_result {
                    Ok(event) => match event {
                        StreamEvent::ContentBlockStart { content_block, .. } => {
                            match &content_block {
                                ResponseContentBlock::Text { .. } => {
                                    // Text content arrives via ContentBlockDelta::TextDelta
                                }
                                ResponseContentBlock::ToolUse { id, name, .. } => {
                                    current_tool_id = id.clone();
                                    current_tool_name = name.clone();
                                    current_tool_input.clear();
                                    yield AgentEvent::ToolUseStart { id: id.clone(), name: name.clone() };
                                }
                                ResponseContentBlock::Thinking { thinking } => {
                                    yield AgentEvent::ThinkingDelta(thinking.clone());
                                }
                            }
                        }
                        StreamEvent::ContentBlockDelta { delta, .. } => match delta {
                            DeltaBlock::TextDelta { text } => {
                                assistant_text.push_str(&text);
                                yield AgentEvent::TextDelta(text);
                            }
                            DeltaBlock::InputJsonDelta { partial_json } => {
                                current_tool_input.push_str(&partial_json);
                            }
                            DeltaBlock::ThinkingDelta { thinking } => {
                                yield AgentEvent::ThinkingDelta(thinking);
                            }
                            DeltaBlock::SignatureDelta { .. } => {
                                // Safely ignored — signature verification is not user-facing
                            }
                        },
                        StreamEvent::ContentBlockStop { .. } => {
                            if !current_tool_id.is_empty() {
                                let input: serde_json::Value = match serde_json::from_str(&current_tool_input) {
                                    Ok(v) => v,
                                    Err(e) => {
                                        tracing::warn!(
                                            "Malformed tool input JSON for {}: {} (raw: {}…)",
                                            current_tool_name,
                                            e,
                                            &current_tool_input[..current_tool_input.len().min(200)],
                                        );
                                        yield AgentEvent::TextDelta(format!(
                                            "\n\x1b[33m[Warning: malformed tool input for {}, using empty object]\x1b[0m\n",
                                            current_tool_name,
                                        ));
                                        serde_json::Value::Object(Default::default())
                                    }
                                };
                                yield AgentEvent::ToolUseReady {
                                    id: current_tool_id.clone(),
                                    name: current_tool_name.clone(),
                                    input: input.clone(),
                                };
                                assistant_blocks.push(ContentBlock::ToolUse {
                                    id: current_tool_id.clone(),
                                    name: current_tool_name.clone(),
                                    input: input.clone(),
                                });
                                tool_paths.extend(extract_tool_paths(&input));
                                tool_uses.push((current_tool_id.clone(), current_tool_name.clone(), input));
                                current_tool_id.clear();
                                current_tool_name.clear();
                                current_tool_input.clear();
                            }
                        }
                        StreamEvent::MessageDelta { delta, usage: delta_usage } => {
                            stop_reason = delta.stop_reason.as_deref().map(|r| {
                                r.parse::<StopReason>().unwrap_or_else(|_| {
                                    warn!("Unknown stop_reason from API: {}", r);
                                    StopReason::EndTurn
                                })
                            });
                            // MessageDelta carries the final output_tokens count for the turn.
                            // MessageStart only has input_tokens (output_tokens is 0 there).
                            if let Some(du) = delta_usage {
                                if let Some(ref mut u) = usage {
                                    u.output_tokens = du.output_tokens;
                                }
                            }
                            // Eagerly start tool execution once stop_reason is known.
                            // The API stream tail (MessageStop, close) carries no
                            // information needed for execution; starting now lets
                            // permission prompts and tool output flow immediately
                            // while we finish consuming the remaining stream events.
                            if stop_reason == Some(StopReason::ToolUse)
                                && !tool_uses.is_empty()
                                && early_exec.is_none()
                            {
                                tool_context.messages = messages.clone();
                                let exec = Arc::clone(&executor);
                                let ctx = tool_context.clone();
                                let tools = std::mem::take(&mut tool_uses);
                                early_exec = Some(tokio::spawn(async move {
                                    exec.execute_many(tools, &ctx).await
                                }));
                            }
                        }
                        StreamEvent::MessageStart { message } => {
                            usage = Some(Usage {
                                input_tokens: message.usage.input_tokens,
                                output_tokens: message.usage.output_tokens,
                                cache_creation_input_tokens: message.usage.cache_creation_input_tokens,
                                cache_read_input_tokens: message.usage.cache_read_input_tokens,
                            });
                        }
                        StreamEvent::Error { error } => {
                            yield AgentEvent::Error(format!("{}: {}", error.error_type, error.message));
                            break;
                        }
                        StreamEvent::MessageStop => {
                            // End-of-message marker — no action needed here.
                            // The stop_reason was already captured in MessageDelta;
                            // the stream will close naturally on the next iteration.
                        }
                        _ => {}
                    },
                    Err(e) => {
                        let err_str = format!("{}", e);
                        let is_timeout = err_str.contains("idle timeout")
                            || err_str.contains("stall timeout")
                            || err_str.contains("timed out")
                            || err_str.contains("connection reset");

                        if is_timeout {
                            consecutive_errors += 1;
                            state.write().await.record_error("stream_timeout");
                            if consecutive_errors <= MAX_STREAM_TIMEOUT_RETRIES {
                                let wait_ms = with_jitter(retry_delay_ms.min(8_000), consecutive_errors);
                                yield AgentEvent::TextDelta(format!(
                                    "\n\x1b[33m[Stream timeout — retrying ({}/{}) in {}ms]\x1b[0m\n",
                                    consecutive_errors, MAX_STREAM_TIMEOUT_RETRIES, wait_ms,
                                ));
                                tokio::time::sleep(std::time::Duration::from_millis(wait_ms)).await;
                                retry_delay_ms = (retry_delay_ms * 2).min(32_000);
                                // Don't push partial assistant message; retry the entire turn
                                should_retry_turn = true;
                                break; // break inner while loop
                            }
                        }

                        state.write().await.messages = messages.clone();
                        // Fire StopFailure hook before breaking
                        if hooks.has_hooks(HookEvent::StopFailure) {
                            let ctx = hooks.prompt_ctx(HookEvent::StopFailure, Some(format!("Stream error: {}", e)));
                            let _ = hooks.run(HookEvent::StopFailure, ctx).await;
                        }
                        yield AgentEvent::Error(format!("Stream error: {}", e));
                        break;
                    }
                }

                // While waiting for the API stream to close, drain any early
                // tool output that has already started flowing.
                while let Ok((id, name, line)) = output_rx.try_recv() {
                    yield AgentEvent::ToolOutputLine { id, name, line };
                }
            }

            // User aborted mid-stream — don't persist a partial assistant message.
            // Check before should_retry_turn so abort always wins over timeout-retry.
            if tool_context.abort_signal.is_aborted() {
                state.write().await.messages = messages.clone();
                yield AgentEvent::TurnComplete { stop_reason: StopReason::EndTurn };
                break;
            }

            // If a stream timeout triggered a retry, skip message processing
            // and go straight to the next outer-loop iteration (re-call API).
            if should_retry_turn {
                continue;
            }

            // Ensure text block is present
            if !assistant_text.is_empty() && !assistant_blocks.iter().any(|b| matches!(b, ContentBlock::Text { .. })) {
                assistant_blocks.insert(0, ContentBlock::Text { text: assistant_text.clone() });
            }

            // Capture tool names from assistant_blocks before they are moved
            // into AssistantMessage, so plan-mode transitions can still reference them.
            let called_tool_names: Vec<String> = assistant_blocks.iter().filter_map(|b| {
                if let ContentBlock::ToolUse { name, .. } = b { Some(name.clone()) } else { None }
            }).collect();

            let assistant_msg = AssistantMessage {
                uuid: Uuid::new_v4().to_string(),
                content: assistant_blocks,
                stop_reason: stop_reason.clone(),
                usage: usage.clone(),
            };
            messages.push(Message::Assistant(assistant_msg.clone()));
            yield AgentEvent::AssistantMessage(assistant_msg);

            // ── PostSampling hook ────────────────────────────────────────────
            // Fires after model response, before tool execution. Allows
            // observation or modification of the assistant's output.
            if hooks.has_hooks(HookEvent::PostSampling) {
                let ctx = hooks.prompt_ctx(
                    HookEvent::PostSampling,
                    if assistant_text.is_empty() { None } else { Some(assistant_text.clone()) },
                );
                match hooks.run(HookEvent::PostSampling, ctx).await {
                    HookDecision::Block { reason } => {
                        reminders.push(crate::system_reminder::SystemReminder::HookResult {
                            success: false,
                            feedback: Some(format!("PostSampling hook blocked: {reason}")),
                        });
                        yield AgentEvent::Error(format!("[PostSampling hook blocked]: {}", reason));
                        state.write().await.messages = messages.clone();
                        break;
                    }
                    HookDecision::FeedbackAndContinue { feedback } => {
                        reminders.push(crate::system_reminder::SystemReminder::HookResult {
                            success: true,
                            feedback: Some(format!("PostSampling hook feedback: {feedback}")),
                        });
                    }
                    HookDecision::AppendContext { text } => {
                        reminders.push(crate::system_reminder::SystemReminder::Custom(
                            format!("PostSampling hook appended context: {text}"),
                        ));
                    }
                    HookDecision::ModifyInput { .. } => {
                        // PostSampling doesn't operate on tool input; ignore.
                    }
                    HookDecision::Allow => {
                        // Allow means proceed normally (used by PermissionRequest hooks)
                    }
                    HookDecision::Continue => {}
                }
            }

            // Successful API response — reset error tracking
            consecutive_errors = 0;
            retry_delay_ms = 1_000;

            if let Some(ref u) = usage {
                let mut s = state.write().await;
                s.total_input_tokens = s.total_input_tokens.saturating_add(u.input_tokens);
                s.total_output_tokens = s.total_output_tokens.saturating_add(u.output_tokens);
                s.total_cache_read_tokens = s.total_cache_read_tokens
                    .saturating_add(u.cache_read_input_tokens.unwrap_or(0));
                s.total_cache_creation_tokens = s.total_cache_creation_tokens
                    .saturating_add(u.cache_creation_input_tokens.unwrap_or(0));

                // Per-model usage tracking
                let model_name = s.model.clone();
                let cost = crate::cost::calculate_cost(&model_name, u);
                s.record_model_usage(
                    &model_name,
                    u.input_tokens,
                    u.output_tokens,
                    u.cache_read_input_tokens.unwrap_or(0),
                    u.cache_creation_input_tokens.unwrap_or(0),
                    cost,
                );
                drop(s);

                yield AgentEvent::UsageUpdate(u.clone());

                // Queue token usage reminder for injection into next tool result
                reminders.push_token_usage(u, config.context_window);

                // Emit per-turn token event for budget tracking
                yield AgentEvent::TurnTokens {
                    input_tokens: u.input_tokens,
                    output_tokens: u.output_tokens,
                };

                // Context usage warning (only emit when level escalates).
                // Use u.input_tokens (current turn's context size from the API, which
                // equals the actual tokens in the context window for this request)
                // rather than state.total_input_tokens (accumulated sum across all turns,
                // which grows far beyond the window size and fires warnings too early).
                if let Some((level, warning_event)) = build_context_warning(u.input_tokens, config.context_window) {
                    if last_warning_level != Some(level) {
                        last_warning_level = Some(level);
                        yield warning_event;
                    }
                }

                // Budget enforcement
                if config.token_budget > 0 {
                    let total_tokens = {
                        let s = state.read().await;
                        s.total_input_tokens + s.total_output_tokens
                    };
                    if total_tokens >= config.token_budget {
                        yield AgentEvent::Error(format!(
                            "Token budget exceeded ({}/{}) — stopping",
                            total_tokens, config.token_budget
                        ));
                        state.write().await.messages = messages.clone();
                        break;
                    }
                }
            }

            let actual_stop = stop_reason.unwrap_or(StopReason::EndTurn);
            match actual_stop {
                StopReason::ToolUse if early_exec.is_some() || !tool_uses.is_empty() => {
                    // called_tool_names was already captured from assistant_blocks above.

                    let mut results = if let Some(handle) = early_exec.take() {
                        // Tool execution was already started eagerly — drain output
                        // while waiting for it to complete.
                        let mut handle = std::pin::pin!(handle);
                        let r = loop {
                            tokio::select! {
                                r = &mut handle => {
                                    // Drain any remaining output
                                    while let Ok((id, name, line)) = output_rx.try_recv() {
                                        yield AgentEvent::ToolOutputLine { id, name, line };
                                    }
                                    break r.unwrap_or_else(|e| {
                                        tracing::error!("Early tool execution task panicked: {}", e);
                                        vec![]
                                    });
                                }
                                Some((id, name, line)) = output_rx.recv() => {
                                    yield AgentEvent::ToolOutputLine { id, name, line };
                                }
                            }
                        };
                        r
                    } else {
                        // Fallback: no early execution (shouldn't normally happen,
                        // but covers edge cases like tool_uses populated without
                        // stop_reason being set to ToolUse in MessageDelta).
                        tool_context.messages = messages.clone();
                        let exec_fut = executor.execute_many(tool_uses, &tool_context);
                        let mut exec_fut = std::pin::pin!(exec_fut);
                        loop {
                            tokio::select! {
                                results = &mut exec_fut => {
                                    while let Ok((id, name, line)) = output_rx.try_recv() {
                                        yield AgentEvent::ToolOutputLine { id, name, line };
                                    }
                                    break results;
                                }
                                Some((id, name, line)) = output_rx.recv() => {
                                    yield AgentEvent::ToolOutputLine { id, name, line };
                                }
                            }
                        }
                    };

                    // Inject pending system reminders into tool results
                    reminders.inject_into(&mut results);

                    let tool_result_msg = UserMessage {
                        uuid: Uuid::new_v4().to_string(),
                        content: results.clone(),
                    };
                    messages.push(Message::User(tool_result_msg));

                    // Check for plan mode transitions from successful tool results
                    for (idx, result) in results.iter().enumerate() {
                        if let ContentBlock::ToolResult { tool_use_id, is_error, content } = result {
                            let result_text = content.first().and_then(|c| {
                                if let clawed_core::message::ToolResultContent::Text { text } = c {
                                    Some(text.clone())
                                } else {
                                    None
                                }
                            });

                            // Apply plan mode transitions on successful tool calls
                            if !is_error {
                                if let Some(name) = called_tool_names.get(idx) {
                                    if name == "EnterPlanMode" {
                                        let mut s = state.write().await;
                                        s.enter_plan_mode();
                                        tool_context.permission_mode = clawed_core::permissions::PermissionMode::Plan;
                                        reminders.push(crate::system_reminder::SystemReminder::PlanModeChange { active: true });
                                        info!("Plan mode activated via EnterPlanMode tool");
                                    } else if name == "ExitPlanMode" {
                                        let mut s = state.write().await;
                                        let restored = s.exit_plan_mode();
                                        tool_context.permission_mode = restored;
                                        reminders.push(crate::system_reminder::SystemReminder::PlanModeChange { active: false });
                                        info!("Plan mode deactivated via ExitPlanMode tool, restored to {:?}", restored);
                                    }
                                }
                            }

                            yield AgentEvent::ToolResult { id: tool_use_id.clone(), is_error: *is_error, text: result_text };
                        }
                    }

                    // ── Conditional skill activation (based on files touched) ──
                    if !tool_paths.is_empty() {
                        let paths: Vec<&str> = tool_paths.iter().map(|s| s.as_str()).collect();
                        let activated = clawed_core::skills::activate_conditional_skills(
                            &paths,
                            &tool_context.cwd,
                        );
                        if !activated.is_empty() {
                            yield AgentEvent::SkillsActivated { names: activated };
                        }
                        tool_paths.clear();
                    }

                    turn_count += 1;
                    stop_hook_retries = 0;
                    { let mut s = state.write().await; s.turn_count = turn_count; }

                    // ── Proactive auto-compact (between tool-use turns) ──────
                    // Like TS query.ts: check should_auto_compact at each turn
                    // boundary and compact before next API call to avoid hitting
                    // context limits mid-conversation.
                    if let Some(ref ac_state) = config.auto_compact_state {
                        let msg_tokens = clawed_core::token_estimation::token_count_with_estimation(&messages);
                        let sys_tokens = clawed_core::token_estimation::estimate_system_tokens(&current_system_prompt);
                        let current_tokens = msg_tokens + sys_tokens;

                        let needs_compact = {
                            let ac = ac_state.lock().await;
                            ac.should_auto_compact(current_tokens, config.context_window)
                        };

                        // Try section trimming before expensive API compaction.
                        let needs_compact = if needs_compact {
                            if let Some((trimmed, new_tokens)) = try_trim_system_prompt(
                                &current_system_prompt,
                                current_tokens,
                                msg_tokens,
                                config.context_window,
                            ) {
                                current_system_prompt = trimmed;
                                let ac = ac_state.lock().await;
                                if !ac.should_auto_compact(new_tokens, config.context_window) {
                                    tracing::info!("Section trim avoided API compaction");
                                    continue;
                                }
                                true
                            } else {
                                true
                            }
                        } else {
                            false
                        };

                        if needs_compact {
                            yield AgentEvent::CompactStart;

                            // ── Pre-clean: trim history before sending to Claude ──────────
                            // This reduces the payload size and prevents the compaction API
                            // call itself from hitting context limits on very large histories.
                            let pre_trunc = crate::compact::truncate_large_tool_results(
                                &mut messages,
                                crate::compact::MAX_TOOL_RESULT_CHARS / 2,
                            );
                            // If context is very full (>90%), also clear stale results.
                            let high_water = (config.context_window as f64 * COMPACT_HIGH_WATER_RATIO) as u64;
                            let pre_cleared = if current_tokens >= high_water {
                                crate::compact::clear_old_tool_results(&mut messages, COMPACT_CLEAR_RETAIN_COUNT)
                            } else {
                                0
                            };
                            if pre_trunc + pre_cleared > 0 {
                                info!(
                                    "Pre-compact trim: {} tool result(s) truncated, {} cleared",
                                    pre_trunc, pre_cleared
                                );
                            }

                            let model = { state.read().await.model.clone() };
                            // Try partial compaction first (keep last 10 messages),
                            // fall back to full compaction if partial fails or is insufficient.
                            let compact_result = if messages.len() > 10 {
                                partial_compact_conversation(&client, &messages, &model, 10, None).await
                            } else {
                                // Too few messages for partial — go straight to full
                                compact_conversation(&client, &messages, &model, None)
                                    .await
                                    .map(|summary| {
                                        let context_msg = compact_context_message(&summary, None);
                                        vec![Message::User(UserMessage {
                                            uuid: Uuid::new_v4().to_string(),
                                            content: vec![ContentBlock::Text { text: context_msg }],
                                        })]
                                    })
                            };

                            match compact_result {
                                Ok(mut new_messages) => {
                                    let summary_len = new_messages.iter().map(|m| match m {
                                        Message::User(u) => u.content.iter().map(|c| match c {
                                            ContentBlock::Text { text } => text.len(),
                                            _ => 0,
                                        }).sum::<usize>(),
                                        _ => 0,
                                    }).sum::<usize>();
                                    // Re-inject session context (date + git status) post-compact
                                    if let Some(ref ctx) = config.session_context {
                                        if !ctx.is_empty() {
                                            new_messages.push(Message::User(UserMessage {
                                                uuid: Uuid::new_v4().to_string(),
                                                content: vec![ContentBlock::Text {
                                                    text: crate::context::format_context_message(ctx),
                                                }],
                                            }));
                                        }
                                    }
                                    messages = new_messages;
                                    // Re-estimate tokens so warning system stays accurate
                                    let new_tokens = clawed_core::token_estimation::token_count_with_estimation(&messages);
                                    {
                                        let mut s = state.write().await;
                                        s.messages = messages.clone();
                                        s.total_input_tokens = new_tokens;
                                        s.total_output_tokens = 0;
                                    }
                                    ac_state.lock().await.record_success();
                                    last_warning_level = None; // reset after compaction
                                    has_attempted_reactive_compact = false;
                                    info!("Proactive auto-compact succeeded (summary {} chars)", summary_len);
                                    reminders.push(crate::system_reminder::SystemReminder::Custom(
                                        "Conversation history has been compacted. Earlier messages are now summarized. \
                                         If you need to re-read specific files, use the Read tool.".into(),
                                    ));
                                    yield AgentEvent::CompactComplete { summary_len };
                                }
                                Err(e) => {
                                    ac_state.lock().await.record_failure();
                                    warn!("Proactive auto-compact failed: {}", e);
                                    // ── Emergency micro-compact fallback ──────────────────────
                                    // Summarization failed (network error, model error, etc.).
                                    // Aggressively trim the history to prevent unbounded growth
                                    // while the circuit breaker tracks consecutive failures.
                                    let emg_snipped = crate::compact::snip_old_messages(&mut messages, 8);
                                    let emg_trunc = crate::compact::truncate_large_tool_results(
                                        &mut messages,
                                        crate::compact::MAX_TOOL_RESULT_CHARS / 4, // half the normal limit — emergency
                                    );
                                    {
                                        let mut s = state.write().await;
                                        s.messages = messages.clone();
                                    }
                                    warn!(
                                        "Emergency micro-compact: snipped {} msg(s), truncated {} result(s)",
                                        emg_snipped, emg_trunc
                                    );
                                    yield AgentEvent::TextDelta(format!(
                                        "\n\x1b[33m[Auto-compact failed: {} — emergency trim applied]\x1b[0m\n",
                                        e
                                    ));
                                }
                            }
                        }
                    }
                }

                StopReason::MaxTokens => {
                    // Strategy 1: Escalate max_tokens to model's upper limit,
                    // but cap so input + output doesn't exceed the context window.
                    if effective_max_tokens < escalated_max_tokens {
                        let max_allowed = if config.context_window > 0 {
                            let input_tokens = usage.as_ref().map(|u| u.input_tokens).unwrap_or(0);
                            (config.context_window.saturating_sub(input_tokens) as u32)
                                .max(effective_max_tokens)
                        } else {
                            escalated_max_tokens
                        };
                        effective_max_tokens = escalated_max_tokens.min(max_allowed);
                        yield AgentEvent::TextDelta(format!(
                            "\n\x1b[33m[Output truncated — escalating max_tokens to {}K]\x1b[0m\n",
                            escalated_max_tokens / 1000
                        ));
                        messages.push(Message::User(make_continuation_message(0, MAX_TOKENS_RECOVERY_LIMIT)));
                        turn_count += 1;
                        { let mut s = state.write().await; s.turn_count = turn_count; }
                        continue;
                    }

                    // Strategy 2: Multi-turn continuation (up to 3 attempts)
                    if max_tokens_recovery_count < MAX_TOKENS_RECOVERY_LIMIT {
                        max_tokens_recovery_count += 1;
                        yield AgentEvent::TextDelta(format!(
                            "\n\x1b[33m[Output truncated — recovery attempt {}/{}]\x1b[0m\n",
                            max_tokens_recovery_count, MAX_TOKENS_RECOVERY_LIMIT
                        ));
                        messages.push(Message::User(make_continuation_message(max_tokens_recovery_count, MAX_TOKENS_RECOVERY_LIMIT)));
                        turn_count += 1;
                        { let mut s = state.write().await; s.turn_count = turn_count; }
                        continue;
                    }

                    // Exhausted recovery
                    yield AgentEvent::TextDelta(
                        "\n\x1b[31m[Max output tokens recovery exhausted]\x1b[0m\n".to_string()
                    );
                    state.write().await.messages = messages.clone();
                    yield AgentEvent::TurnComplete { stop_reason: StopReason::MaxTokens };
                    break;
                }

                other => {
                    // ── Stop hooks ───────────────────────────────────────────
                    if hooks.has_hooks(HookEvent::Stop) {
                        // Pass the last assistant text as context so hook scripts
                        // can inspect what Claude just said.
                        let last_text = if assistant_text.is_empty() { None } else { Some(assistant_text.clone()) };
                        let ctx = hooks.prompt_ctx(HookEvent::Stop, last_text);
                        match hooks.run(HookEvent::Stop, ctx).await {
                            HookDecision::FeedbackAndContinue { feedback } if stop_hook_retries < MAX_STOP_HOOK_RETRIES => {
                                stop_hook_retries += 1;
                                // Check max_turns before injecting feedback turn
                                if turn_count + 1 >= config.max_turns {
                                    yield AgentEvent::TextDelta("\n[Stop hook: at max turns — stopping]\n".to_string());
                                } else {
                                    // exit 2: inject feedback as a new user message and loop
                                    let feedback_msg = UserMessage {
                                        uuid: Uuid::new_v4().to_string(),
                                        content: vec![ContentBlock::Text { text: feedback.clone() }],
                                    };
                                    messages.push(Message::User(feedback_msg));
                                    yield AgentEvent::TextDelta(format!("\n[Stop hook feedback ({}/{})]: {}\n", stop_hook_retries, MAX_STOP_HOOK_RETRIES, feedback));
                                    turn_count += 1;
                                    { let mut s = state.write().await; s.turn_count = turn_count; }
                                    continue; // restart the query loop
                                }
                            }
                            HookDecision::FeedbackAndContinue { .. } => {
                                yield AgentEvent::TextDelta("\n[Stop hook retry limit reached — stopping]\n".to_string());
                            }
                            HookDecision::Block { reason } => {
                                // Non-zero exit: warn but still stop
                                yield AgentEvent::TextDelta(format!("\n[Stop hook warning]: {}\n", reason));
                            }
                            _ => {}
                        }
                    }

                    // Persist conversation history so the next submit() continues the session
                    state.write().await.messages = messages.clone();
                    yield AgentEvent::TurnComplete { stop_reason: other };
                    break;
                }
            }
        }
    };
    Box::pin(stream)
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod e2e_tests;
