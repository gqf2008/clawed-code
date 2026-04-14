# CLAUDE.md

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

## Build & Test

```bash
cargo check                # Type-check only
cargo build                # Full build
cargo build --release      # Release build
cargo test                 # Run all workspace tests
cargo test -p clawed-agent # Test a specific crate
cargo test -p clawed-core  # Test a specific crate
cargo clippy --workspace   # Lint the whole workspace
cargo fmt --check          # Format check
```

## API Setup

Required environment variables (one of):
- `ANTHROPIC_API_KEY` — Anthropic API key
- `OPENAI_API_KEY`, `DEEPSEEK_API_KEY`, etc. — For alternative providers

Or OAuth via `~/.claude/.credentials.json` (official CLI login).

## Run

```bash
cargo run -- --help
cargo run -- "your prompt here"
cargo run -- -m claude-opus-4-20250514 --thinking
```

## Providers

`--provider` supports: `anthropic`, `openai`, `deepseek`, `ollama`, `together`, `groq`, `bedrock`, `vertex`

Use `--base-url` to override API endpoint (e.g., LiteLLM, DashScope).

## Architecture

11-crate workspace with zero circular dependencies:

| Crate | Role |
|-------|------|
| `clawed-cli` | Binary entry, REPL, themes, NDJSON output |
| `clawed-rpc` | JSON-RPC external interface (TCP/stdio) |
| `clawed-bridge` | External channel gateway (Lark/Telegram/Slack) |
| `clawed-agent` | Engine orchestration, sessions, hooks, permissions, compaction |
| `clawed-mcp` | MCP server registry, health monitoring, auto-reconnect |
| `clawed-swarm` | kameo Actor multi-agent network |
| `clawed-computer-use` | Computer Use (screenshot/mouse/keyboard) |
| `clawed-bus` | In-process event bus, ClientHandle, broadcast notifications |
| `clawed-api` | HTTP client, SSE streaming, OAuth PKCE |
| `clawed-tools` | 28+ tool implementations, ToolRegistry, LSP |
| `clawed-core` | Base types, Tool trait, permissions, config, file watching |

Dependency flow: `{cli,rpc,bridge} → agent → {swarm,mcp,computer-use,api,tools,bus} → core`

See `ARCHITECTURE.md` for the current detailed breakdown, test counts, and crate-level responsibilities.

## Gotchas

- **Model validation**: Strict model/provider matching unless `--base-url` is set (for compatible APIs like DashScope).
- **OAuth tokens**: Stored with millisecond expiry in `~/.claude/.credentials.json`.
- **Session resume**: `--resume` restores latest; `--session-id <id>` restores specific.
- **MCP servers**: Auto-discovered from `.mcp.json` at project/user roots.
