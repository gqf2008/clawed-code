# Copilot Instructions — Clawed Code (Rust)

Non-official Rust port of `@anthropic-ai/claude-code` (v2.1.88). 11-crate Cargo workspace, ~69,500 LoC, 2,048 tests.

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

### Safety Invariants (maintained strictly)
- **0 `unsafe` blocks** in the entire codebase.
- **0 `panic!` calls** in production code paths.
- **0 `.lock().unwrap()`** — all mutex locks use `lock_or_recover()`.
- **0 Clippy warnings** — pedantic + nursery lints enabled workspace-wide.

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
