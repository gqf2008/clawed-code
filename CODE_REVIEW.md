# Deep Code Review: Clawed Code

> Reviewed: 2025-04-12 | Crates: clawed-cli, clawed-agent, clawed-api, clawed-tools, clawed-core, clawed-bus, clawed-mcp, clawed-rpc, clawed-swarm, clawed-bridge, clawed-computer-use

---

## 1. Security Findings

### HIGH: Bash tool — command injection via environment variables

**File:** `crates/clawed-tools/src/bash.rs:283-285`

```rust
for (k, v) in &env_overrides {
    cmd.env(k, v);
}
```

Environment variables from the LLM (which can be influenced by user prompts) are passed directly to the spawned shell without sanitization. While these go through `Command::env()` rather than shell interpolation, the LLM could inject dangerous variables like `LD_PRELOAD`, `PATH`, or `IFS`.

**Recommendation:** Maintain an allowlist of safe env var names, or block known-dangerous keys (`LD_PRELOAD`, `LD_LIBRARY_PATH`, `PATH`, `IFS`, `BASH_ENV`, `ENV`, `PROMPT_COMMAND`).

---

### MEDIUM: `unsafe env::set_var` in config

**File:** `crates/clawed-core/src/config/mod.rs:250`

```rust
unsafe { std::env::set_var(key, value); }
```

The comment says "must be called single-threaded during init" but there is no compile-time or runtime enforcement. If `apply_env()` is called from a multi-threaded context, this is UB.

The `#[allow(clippy::unsafe_derive_deserialize)]` on line 64 is misleading — it references the derive macro, but the actual unsafe code is in `apply_env()`.

**Recommendation:** Add a runtime assertion or use `std::sync::OnceLock` to gate access. Clarify the allow annotation.

---

### MEDIUM: Dangerous command detection is a blocklist, not an allowlist

**File:** `crates/clawed-tools/src/bash.rs:13-34`

The blocklist approach can never be complete. Notable gaps:

| Pattern | Risk |
|---------|------|
| `curl \| bash` | Remote code execution via pipe |
| `wget -O- \| sh` | Same pattern |
| `eval`, `exec`, `source` | Shell builtins that execute arbitrary strings |
| `python -c "import os; os.system('...')"` | Code execution via interpreter |
| `perl -e 'system(...)'` | Code execution via interpreter |
| `/dev/tcp/` redirects | Bash network connections |
| `nc`, `netcat`, `socat` | Reverse shells |

These commands all pass through `check_dangerous()` as safe. Mitigated somewhat by the permission system, but the blocklist gives a false sense of security.

**Recommendation:** Add these patterns to the blocklist, or add a second "remote execution" check for pipe-to-shell patterns.

---

### MEDIUM: `git push --force-with-lease` incorrectly blocked

**File:** `crates/clawed-tools/src/bash.rs:642-647`

Known limitation acknowledged in test: `git push --force-with-lease` is blocked because it contains the substring `git push --force`. This blocks a legitimately safe command.

**Recommendation:** Change the check to `git push --force-without-lease` or use a more precise regex that excludes `--force-with-lease`.

---

## 2. Bugs

### BUG: `normalize_path` can produce incorrect results on Windows

**File:** `crates/clawed-tools/src/path_util.rs:76`

```rust
if result.parent().is_some() && result != result.ancestors().last().unwrap_or(Path::new("")) {
    result.pop();
}
```

The `result != result.ancestors().last()` comparison is fragile. On Windows with `C:\a\..`, this might not correctly identify when you are at the drive root. The logic works for Unix paths but has edge cases with drive letters.

---

### BUG: `extract_base_command` can return incorrect results for all-env-var commands

**File:** `crates/clawed-tools/src/bash.rs:120-140`

```rust
for part in &mut parts {
    if part.contains('=') && !part.starts_with('-') {
        continue;
    }
    return first_cmd[first_cmd.find(part).unwrap_or(0)..].trim();
}
```

If all parts contain `=`, the loop falls through and returns `first_cmd` unmodified. For a command like `FOO=bar BAZ=1` (with no actual command after env vars), this returns the whole string instead of recognizing there is no command.

---

### BUG: `find_project_root` cache is stale if cwd changes

**File:** `crates/clawed-tools/src/path_util.rs:87-118`

The cache key is `cwd`, but if the process changes directories (e.g., via `cd` in bash), the cached result becomes stale. The cache should include a mechanism for invalidation.

---

### BUG: `running_count()` is racy

**File:** `crates/clawed-agent/src/coordinator.rs:154-156`

```rust
pub fn running_count(&self) -> usize {
    self.max_concurrent - self.concurrency.available_permits()
}
```

`available_permits()` can return stale values because permits are acquired/released concurrently. This is fine for display purposes but should not be used for logic decisions.

---

## 3. Code Quality

### ISSUE: `truncate_output` in bash.rs has overlapping logic with `truncate_tool_output` in path_util.rs

- **bash.rs:65-84** — truncates at 100KB (bash-specific)
- **path_util.rs:127-161** — truncates at 30KB (general tool output)
- **executor.rs:237-247** — truncates again at `DEFAULT_MAX_TOOL_RESULT_TOKENS`

Double (triple) truncation is wasteful and the inconsistent limits could cause confusion.

**Recommendation:** Consolidate into a single truncation point, preferably in the executor layer.

---

### ISSUE: Silent error swallowing in hooks

**File:** `crates/clawed-agent/src/executor.rs:173, 192, 217, 261`

```rust
let _ = self.hooks.run(HookEvent::PermissionDenied, ctx).await;
```

Hook failures are silently swallowed. If a hook panics or errors, it is completely invisible.

**Recommendation:** At minimum, log the error with `warn!()`.

---

### ISSUE: `BypassAll` permission mode is a security bypass

`PermissionMode::BypassAll` completely skips all permission checks throughout the codebase. If this mode can be set via config or environment variable, it creates a dangerous escape hatch.

---

## 4. Performance

### ISSUE: `truncate_output` allocates unnecessarily

**File:** `crates/clawed-tools/src/bash.rs:69-83`

The truncation formatting path allocates 3 separate `String`s.

---

### ISSUE: SSE buffer uses `String::from_utf8_lossy` per chunk

**File:** `crates/clawed-api/src/client.rs:300`

Each SSE chunk goes through `String::from_utf8_lossy(&chunk)` and then `buffer.push_str()`. For long streams, this creates many temporary allocations.

**Recommendation:** Use a `Vec<u8>` buffer with delayed UTF-8 conversion.

---

## 5. Test Coverage

### Gaps

| Area | Status | Notes |
|------|--------|-------|
| OAuth PKCE flow | **Missing** | No tests for token parsing, expiry, refresh |
| WebFetch tool | **Missing** | No tests for URL validation, size limiting |
| MCP transport | **Missing** | No SSE/stdio transport tests |
| Bridge adapters | **Missing** | Feishu/Telegram adapters untested |
| Swarm conflict resolution | **Missing** | No tests for merge conflict scenarios |

### Strengths

- `bash.rs`: 30+ tests covering dangerous patterns, exit codes, command classification, truncation
- `executor.rs`: Tests for tool pairing, partitioning, permission checks, abort handling
- `coordinator.rs`: 20+ tests covering agent lifecycle, concurrency, tool interactions
- `path_util.rs`: Good coverage for path normalization, binary detection, symlink safety

---

## 6. Architecture Review

### Positive

- **Clean crate separation:** `cli → agent → {api, tools} → core` with no circular dependencies
- **Tool result validation:** `validate_tool_result_pairing()` ensures every tool_use has a corresponding tool_result
- **Symlink safety:** `resolve_symlink_safe()` correctly handles symlink chains with depth limits
- **Context-aware exit codes:** `interpret_exit_code()` handles non-error exit codes for grep, diff, test
- **Good async patterns:** Proper use of tokio, CancellationToken, mpsc channels
- **Comprehensive hooks:** PreToolUse, PostToolUse, PermissionDenied, PostToolUseFailure lifecycle hooks

### Areas for Improvement

- **No integration test for end-to-end agent loop** — the coordinator → executor → tool pipeline is not tested as a whole
- **Missing rate limiting on tool calls per turn** — a runaway LLM could spawn many tool calls
- **No structured logging** — uses `tracing!` but without span-based correlation for debugging

---

## Summary

| Severity | Count | Key Areas |
|----------|-------|-----------|
| HIGH | 2 | Bash env var injection, WebFetch SSRF via DNS rebinding |
| MEDIUM | 7 | unsafe set_var, blocklist gaps, force-with-lease, GitTool args, OAuth file perms, WebFetch body size, header allowlist |
| BUG | 8 | Path normalization, extract_base_command, stale cache, racy count, conflict_summary dead code, swarm normalize_path, GitStatus errors, html_to_markdown fragility |
| LOW | 8 | Silent hook errors, triple truncation, BypassAll, SSE allocations, OAuth state not validated, file_watcher dup dirs, dropped debouncer, slow-conn DoS |

**Overall assessment:** Well-structured Rust codebase with good test coverage and solid architectural separation. The main concerns are the bash tool's blocklist approach (inherently incomplete), the unsafe `set_var` usage, and subtle issues across the Swarm and Bridge crates that appear under concurrent load.

---

## 7. Extended Review: WebFetch Tool

**File:** `crates/clawed-tools/src/web_fetch.rs`

### POSITIVE: Strong SSRF protection

`validate_url()` (lines 9-67) is well-implemented:
- Blocks non-HTTP schemes
- Validates and blocks private/reserved IP ranges (10/8, 172.16/12, 192.168/16, 100.64/10, CGNAT)
- Blocks cloud metadata endpoints (169.254.169.254, metadata.google.internal)
- Blocks localhost, .local, .internal hostnames
- Handles IPv6 private ranges

### HIGH: DNS rebinding bypass

**File:** `crates/clawed-tools/src/web_fetch.rs:9-67`

The SSRF check validates the URL's host string, but does not validate the IP that the DNS resolves to after the `reqwest` client connects. An attacker who controls a domain can use DNS rebinding: the URL passes validation with a public IP, but DNS resolves to 127.0.0.1 at fetch time.

**Recommendation:** Either (a) use reqwest's `local_address` binding, (b) use a custom DNS resolver that blocks private IPs, or (c) validate the resolved IP after connection.

### MEDIUM: No response body size limit before processing

**File:** `crates/clawed-tools/src/web_fetch.rs:288`

```rust
let body = resp.text().await?;
```

The entire response body is loaded into memory before truncation (line 303). A malicious server could return a 10GB response. The `max_length` parameter only limits the output, not the download.

**Recommendation:** Set `reqwest::Client::builder().max_response_size()` or use a streaming approach with early truncation.

### BUG: Custom headers allow SSRF via `Referer` and `Origin`

**File:** `crates/clawed-tools/src/web_fetch.rs:264-268`

The `BLOCKED_HEADERS` list does not include `Referer`, `Origin`, or `X-Forwarded-Host`. These headers could be used for cache poisoning or bypassing CSRF protections on internal services.

### BUG: `html_to_markdown` is fragile and incomplete

**File:** `crates/clawed-tools/src/web_fetch.rs:70-174`

The custom HTML-to-markdown converter:
- Does not handle nested tags correctly (e.g., `<p><strong>bold</strong></p>`)
- Does not decode numeric entities (`&#60;`, `&#x3C;`)
- Does not handle attributes on block-level tags
- Would panic on malformed HTML with unclosed tags

**Recommendation:** Use `html2md` or `readability` crate instead of a hand-rolled parser.

### POSITIVE: Good test coverage

20+ tests covering SSRF validation, HTML conversion, entity decoding, and content extraction.

---

## 8. Extended Review: OAuth Module

**File:** `crates/clawed-api/src/oauth.rs`

### MEDIUM: OAuth token file has no permission restrictions

**File:** `crates/clawed-api/src/oauth.rs:217-225`

```rust
std::fs::write(&path, json)?;
```

The token file is written with default permissions (typically 644 on Unix), making it readable by all users on the system. OAuth tokens often have broad scopes and should be restricted to owner-only (600).

**Recommendation:** Use `std::fs::File::create()` + `set_permissions()` or the `libc::open()` with `O_CREAT | O_WRONLY` + `0o600`.

### LOW: OAuth state parameter not validated

**File:** `crates/clawed-api/src/oauth.rs:112, 341`

A random state parameter is generated (line 112) and included in the auth URL, but it is never validated when the callback is received (line 341). This makes the flow vulnerable to CSRF attacks where an attacker forces a user to complete an OAuth flow.

**Recommendation:** Store the state value, then validate it when parsing the callback URL.

### POSITIVE: Good PKCE implementation

- Uses SHA-256 with S256 method (not plain)
- 32-byte random verifier (256 bits of entropy)
- Proper token expiry with proactive refresh (5-minute window)
- Good test coverage for expiry logic

### BUG: `wait_for_code` loops forever on a malicious connection

**File:** `crates/clawed-api/src/oauth.rs:326-363`

The function accepts up to 10 connections, but each connection has a 5-second read timeout. A slow attacker could hold the listener for up to 50 seconds before the loop exits. While there's a 5-minute outer timeout, this is a waste of resources.

---

## 9. Extended Review: Git Tool

**File:** `crates/clawed-tools/src/git.rs`

### POSITIVE: Subcommand allowlist approach

Unlike the Bash tool's blocklist, `GitTool` uses an allowlist of subcommands (lines 66-71). This is the correct security model — only explicitly allowed operations can run.

### MEDIUM: Args can still inject dangerous git config

**File:** `crates/clawed-tools/src/git.rs:56-96`

While `--no-verify` and `--force` are blocked, the following are not:
- `git fetch --upload-pack='malicious'` (arbitrary command execution)
- `git clone` is not in the allowlist, but `git pull` is, which can clone from arbitrary remotes
- `git pull` can be used with arbitrary repository URLs, including `file://` or `ssh://`

### BUG: `GitStatusTool` silently swallows errors

**File:** `crates/clawed-tools/src/git.rs:173-228`

Both git commands (branch and status) use `if let Ok(Ok(...))` patterns. If both fail (e.g., git not installed, not a git repo), the tool returns an empty string at line 223, which becomes `"Not a git repository..."`. This conflates "not a git repo" with "git command failed to execute" — different failure modes with the same message.

### POSITIVE: Good safety design

- Uses `tokio::process::Command` with explicit args (no shell)
- 30-second timeout per command
- stdin set to `null()` (no interactive prompts)
- Output truncation via `truncate_output()`

---

## 10. Extended Review: Swarm Conflict Tracker

**File:** `crates/clawed-swarm/src/conflict.rs`

### BUG: `conflict_summary()` can never report conflicts

**File:** `crates/clawed-swarm/src/conflict.rs:151-163`

The `conflict_summary()` method iterates over `locks.values()` — but since `locks` is a `HashMap<String, FileLock>` (one entry per file path), a file can only have one lock at a time. The `try_lock()` method prevents multiple agents from holding the same file lock. Therefore, `conflict_summary()` will always return an empty map.

This method appears to be dead code.

### BUG: `normalize_path` doesn't handle `..` or `.` components

**File:** `crates/clawed-swarm/src/conflict.rs:179-184`

```rust
fn normalize_path(path: &str) -> String {
    let normalized = path.replace('\\', "/");
    normalized.trim_end_matches('/').to_string()
}
```

This only normalizes backslashes and trailing slashes. Two paths like `/src/../main.rs` and `/main.rs` would be treated as different files, leading to missed conflict detection.

### POSITIVE: Thread-safe design

Uses `Arc<RwLock<HashMap>>` for concurrent access — appropriate for multi-agent scenarios. Good test coverage (10 tests).

---

## 11. Extended Review: File Watcher

**File:** `crates/clawed-core/src/file_watcher.rs`

### BUG: Debouncer is dropped when handle is not kept alive

**File:** `crates/clawed-core/src/file_watcher.rs:25-29`

The `ConfigWatchHandle` holds `_debouncer` — if the caller discards the handle, the watcher silently stops. There's no warning or error. This is by design but could be confusing.

### LOW: Duplicate directory watches possible

**File:** `crates/clawed-core/src/file_watcher.rs:177-193`

The code uses a `HashSet` to track watched directories, which prevents duplicate watches of the same directory. However, if two files are in different paths that resolve to the same directory after canonicalization (symlinks), duplicate watches could occur.

### POSITIVE: Good design choices

- OS-native notifications via `notify` (not polling)
- 500ms debounce to handle editor write patterns
- Watches directories, not files (more reliable for atomic saves)
- Non-blocking `try_send` prevents blocking the notify callback

---

## 12. Extended Test Coverage Gaps

| Area | Crate | Status | Risk |
|------|-------|--------|------|
| OAuth PKCE flow | clawed-api | **Missing** | Medium — token parsing, expiry, refresh |
| WebFetch tool (live) | clawed-tools | **Missing** | Medium — URL validation, size limiting |
| MCP transport | clawed-mcp | **Missing** | Medium — SSE/stdio transport |
| Bridge adapters | clawed-bridge | **Missing** | Low — Feishu/Telegram |
| Swarm conflict | clawed-swarm | **Partial** | Low — 10 tests but misses edge cases |
| Swarm team create/delete | clawed-swarm | **Missing** | Low |
| Swarm network/actors | clawed-swarm | **Missing** | Low |
| File watcher (e2e) | clawed-core | **Partial** | Low — 5 tests |
| Cron scheduler | clawed-agent | **Missing** | Medium |
| Memory extractor | clawed-agent | **Missing** | Low |
| Plugin loader | clawed-agent | **Missing** | Low |
| REPL commands | clawed-cli | **Missing** | Low |
| TUI components | clawed-cli | **Missing** | Low |
| RPC server | clawed-rpc | **Missing** | Medium |
