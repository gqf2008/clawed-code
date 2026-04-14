# Copilot Instructions — Clawed Code (Rust)

Behavioral guidelines to reduce common LLM coding mistakes. Merge with project-specific instructions as needed.

**Tradeoff:** These guidelines bias toward caution over speed. For trivial tasks, use judgment.

## 1. Think Before Coding

**Don't assume. Don't hide confusion. Surface tradeoffs.**

Before implementing:
- State your assumptions explicitly. If uncertain, ask.
- If multiple interpretations exist, present them - don't pick silently.
- If a simpler approach exists, say so. Push back when warranted.
- If something is unclear, stop. Name what's confusing. Ask.

## 2. Simplicity First

**Minimum code that solves the problem. Nothing speculative.**

- No features beyond what was asked.
- No abstractions for single-use code.
- No "flexibility" or "configurability" that wasn't requested.
- No error handling for impossible scenarios.
- If you write 200 lines and it could be 50, rewrite it.

Ask yourself: "Would a senior engineer say this is overcomplicated?" If yes, simplify.

## 3. Surgical Changes

**Touch only what you must. Clean up only your own mess.**

When editing existing code:
- Don't "improve" adjacent code, comments, or formatting.
- Don't refactor things that aren't broken.
- Match existing style, even if you'd do it differently.
- If you notice unrelated dead code, mention it - don't delete it.

When your changes create orphans:
- Remove imports/variables/functions that YOUR changes made unused.
- Don't remove pre-existing dead code unless asked.

The test: Every changed line should trace directly to the user's request.

## 4. Goal-Driven Execution

**Define success criteria. Loop until verified.**

Transform tasks into verifiable goals:
- "Add validation" → "Write tests for invalid inputs, then make them pass"
- "Fix the bug" → "Write a test that reproduces it, then make it pass"
- "Refactor X" → "Ensure tests pass before and after"

For multi-step tasks, state a brief plan:
```
1. [Step] → verify: [check]
2. [Step] → verify: [check]
3. [Step] → verify: [check]
```

Strong success criteria let you loop independently. Weak criteria ("make it work") require constant clarification.

---

**These guidelines are working if:** fewer unnecessary changes in diffs, fewer rewrites due to overcomplication, and clarifying questions come before implementation rather than after mistakes.


## Build, Test & Lint

```bash
cargo check                        # Fast type-check (no binary)
cargo build                        # Full build
cargo build --release              # Release build (~19.8 MB, ~38ms startup)
cargo test                         # All 2,048 tests
cargo test -p clawed-agent         # Single crate (483 tests)
cargo test -p clawed-core          # Single crate (452 tests)
cargo test my_function_name        # Single test by name (substring match)
cargo clippy --workspace           # Lint (pedantic + nursery; must be warning-free)
cargo fmt --check                  # Format check
```

Test counts per crate: `clawed-tools` 323, `clawed-cli` 297, `clawed-api` 180, `clawed-rpc` 84, `clawed-mcp` 73, `clawed-swarm` 65, `clawed-bridge` 52, `clawed-bus` 23, `clawed-computer-use` 16.

## Architecture

### Crate Layer Map

```
Layer 3  clawed-cli           Binary entry, REPL, themes, NDJSON output
Layer 3  clawed-rpc           JSON-RPC external interface (TCP/stdio)
Layer 3  clawed-bridge        External channel gateway (Lark/Telegram/Slack)
Layer 2  clawed-agent         Engine orchestration, sessions, hooks, permissions, compaction
Layer 2  clawed-mcp           MCP server registry, health monitoring, auto-reconnect
Layer 2  clawed-swarm         kameo Actor multi-agent network
Layer 2  clawed-computer-use  Computer Use (screenshot/mouse/keyboard)
Layer 1  clawed-bus           In-process event bus, ClientHandle, broadcast notifications
Layer 1  clawed-api           HTTP client, streaming SSE, OAuth PKCE
Layer 1  clawed-tools         28+ tool implementations, ToolRegistry, LSP
Layer 0  clawed-core          Base types, Tool trait, permissions, config, file watching
```

**Dependency rule**: `{cli,rpc,bridge} → agent → {swarm,mcp,computer-use,api,tools,bus} → core`. Zero circular dependencies.

### Event Bus Pattern (clawed-bus)

All communication between the agent core and UI clients flows through `EventBus` via two typed enums:
- **`AgentRequest`** (18 variants) — clients → agent core (e.g., `Submit`, `Abort`, `McpConnect`)
- **`AgentNotification`** (26 variants) — agent core → clients broadcast (e.g., `TextDelta`, `TurnComplete`, `ToolUseStart`)

Clients hold a `ClientHandle` and send/receive through the bus, never calling `QueryEngine` directly.

### Agent Loop Data Flow

```
User input → REPL → ClientHandle.submit()
  → AgentRequest::Submit (mpsc)
  → AgentCoreAdapter → QueryEngine.submit()
  → AnthropicClient.messages_stream() → SSE Parser
  → ToolUse detected → PermissionChecker.check()
  → Hooks.pre_tool_use() → Executor.run() → Hooks.post_tool_use()
  → ToolResult appended → loop (auto-compact if needed)
  → StopReason::EndTurn → AgentNotification::TurnComplete broadcast
```

### Tool System (clawed-core / clawed-tools)

Every tool implements the `Tool` trait (`clawed-core/src/tool.rs`):

```rust
#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn input_schema(&self) -> Value;
    async fn call(&self, input: Value, context: &ToolContext) -> anyhow::Result<ToolResult>;
    fn category(&self) -> ToolCategory { ToolCategory::Session }
    fn is_read_only(&self) -> bool { false }
    fn is_concurrency_safe(&self) -> bool { self.is_read_only() }
}
```

`ToolRegistry` in `clawed-tools/src/lib.rs` centrally registers all tools; tools are looked up by name and can be filtered by `ToolCategory`. MCP tools are dynamically injected.

## Key Conventions

### Error Handling
- Use `anyhow::Result` / `anyhow::Error` for all fallible functions.
- Use `thiserror` for typed crate-level error enums where callers need to pattern-match.
- Never use `.unwrap()` on `Mutex::lock()` — use `lock_or_recover()` instead (defined in `clawed-core/src/agents.rs` and `skills.rs`), which recovers from mutex poisoning.

### Safety Expectations
- Avoid introducing new `unsafe` blocks; existing low-level or platform interop sites should stay isolated and explicitly justified.
- Do not add `panic!` calls in production code paths.
- Do not introduce new `.lock().unwrap()` calls; prefer `lock_or_recover()` where available.
- Do not introduce new Clippy warnings — pedantic + nursery are enabled workspace-wide.

### Concurrency
- Async runtime: `tokio` with `features = ["full"]`.
- Read-only tools run with `join_all` (parallel); write tools run sequentially.
- `AbortSignal` (`Arc<AtomicBool>`) is used to cancel in-flight tool executions.

### Serialization
- `serde` with derive macros throughout; JSON is `serde_json`, YAML is `serde_yaml`.
- Session snapshots are serialized via `SessionSnapshot` in `clawed-core/src/session.rs`.

### Clippy Configuration
Clippy pedantic + nursery are enabled workspace-wide (`Cargo.toml [workspace.lints.clippy]`) with a large explicit allow-list for noisy rules. When adding new code, run `cargo clippy --workspace` — all new warnings must be resolved, not suppressed unless already in the allow-list.

### Adding a New Tool
1. Create `crates/clawed-tools/src/<tool_name>.rs` implementing the `Tool` trait.
2. Register it in `crates/clawed-tools/src/lib.rs` (`ToolRegistry::new()`).
3. Assign an appropriate `ToolCategory` and set `is_read_only()` correctly (affects both permission checks and parallelism).

### Provider / Model Validation
Model names are validated against a strict provider allow-list unless `--base-url` is set (which relaxes validation for compatible APIs like LiteLLM or DashScope).

### Config & Hooks
- Per-project config: `.claude/settings.json` (MCP servers, hooks, permissions).
- Project instructions: `CLAUDE.md` (or `.claude/CLAUDE.md`); injected into every session system prompt.
- Hook events (`PreToolUse`, `PostToolUse`, `Stop`, etc.) are configured there and matched using glob patterns with compiled-regex caching.

## API Setup

```bash
export ANTHROPIC_API_KEY="sk-ant-..."  # Anthropic (default)
# Or: OPENAI_API_KEY, DEEPSEEK_API_KEY, etc. for alternative providers
```

`--provider` options: `anthropic`, `openai`, `deepseek`, `ollama`, `together`, `groq`, `bedrock`, `vertex`.
Use `--base-url` to override endpoint (e.g., LiteLLM, DashScope, local Ollama).

