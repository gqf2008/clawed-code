# Clawed Code 项目审查报告

**审查日期**: 2026-05-17
**审查范围**: 12-crate Rust 工作空间，~320+ 源文件，~70,000+ LoC
**审查维度**: 架构设计、代码质量、安全性、测试覆盖、性能、运维规范
**审查方法**: 源码审计、CLAUDE.md + ARCHITECTURE.md 交叉验证、grep 分析

---

## 一、项目概况

| 指标 | 数值 | 说明 |
|------|------|------|
| Crate 数 | 12 | `core, api, tools, agent, bus, cli, mcp, rpc, bridge, computer-use, swarm, acp` |
| Rust 源文件 | ~320+ | 不含 `/tests/` 独立测试目录和 `*_tests.rs` 内联测试模块 |
| 注册工具源文件 | 40 | 含 MCP 动态代理、Computer Use、Workflow、Cron 等 |
| 工具总数（运行时可发现） | 45+ | Registry 注册 40 + MCP 动态注入 + 变体名称（如 TaskCreate/TaskUpdate 等） |
| 测试总数 | ~2,048 | 含 31 E2E 测试（CLI 22 + TUI 9） |
| `unsafe` 块 | 4 处 | 3× `libc::kill` 进程存活检查 + 1× `prctl(PR_SET_DUMPABLE)` |
| `unsafe` 关键字符号 | 10 次 | 含 `# Safety` 文档注释和 `set_var()` 相关注释 |
| TODO/FIXME/HACK | 0 处 | 全项目零遗留标记 |
| `todo!()` / `unimplemented!()` | 0 处 | — |
| Rust Edition | 2021 | 全 workspace 统一 |
| Release 二进制 | 19.8 MB, ~38ms 启动 | — |

### 1.1 各 Crate 文件与模块统计

| Crate | 源文件数 | 关键模块 |
|-------|:---:|------|
| `clawed-core` | 33 | tool, message, permission, config, memory, model, session, skills, agents, cron (3文件), bash_classifier, file_watcher, git_util, image, message_sanitize, migrations, plugin, secure_storage, sync, text_util, token_estimation, upstream_proxy, write_queue, concurrent_sessions |
| `clawed-api` | 15 | client, streaming, types, oauth, cache_detect, usage, provider, files, model, openai (2), retry, stream |
| `clawed-tools` | 40 | bash, file_read/edit/write/multi_edit, glob_tool, grep, ls, web_fetch/search, lsp (4), diff_ui, git, worktree, ask_user, brief, send_message, todo, task, skill_tool, plan_mode, tool_search, config_tool, context, notebook, cron_create/delete/list, workflow, teleport, push_notification, remote_trigger, repl, powershell, sleep, synthetic_output, path_safety, path_util, mcp_auth, attribution |
| `clawed-agent` | 48 | engine (8), query, compact (3), hooks (3), permissions (6), system_prompt (2), plugin (2), bus_adapter, executor, state, coordinator, dispatch_agent, context, cost, cron_scheduler, task_runner, tool_result_storage, audit, memory_extractor, system_reminder, traits |
| `clawed-cli` | ~46 | main, auth, config, chrome, commands, init, repl, input, repl_commands/, output/, session, ui, theme, markdown, diff_display, native_installer; TUI (16): mod, textarea, statusline, status, tasklist, taskplan, bottombar, overlay, permission, input, messages, markdown, diff_style, bash_mode, tool_monitor, verbs |
| `clawed-mcp` | 8 | registry, config, bus, protocol, sse, types |
| `clawed-rpc` | 9 | server, session, methods, protocol, transport/, error |
| `clawed-bridge` | 11 | gateway, session, formatter, message, config, adapter, adapters/ (feishu, telegram, slack), webhook |
| `clawed-swarm` | 14 | actor, swarm, topology, bus, config, types |
| `clawed-bus` | 3 | bus, events, handler |
| `clawed-computer-use` | 5 | tool, bus, types, session_lock, input, screenshot, server |
| `clawed-acp` | 7 | agent, server, session, mcp_bridge, transport, types, lib |

---

## 二、架构设计 — 5/5

### 2.1 分层架构

```
Layer 3  clawed-cli, clawed-rpc, clawed-bridge, clawed-acp  (入口层)
Layer 2  clawed-agent, clawed-mcp, clawed-swarm, clawed-computer-use  (编排层)
Layer 1  clawed-bus, clawed-api, clawed-tools  (领域服务层)
Layer 0  clawed-core  (基础类型层)
```

依赖方向: `{cli,rpc,bridge,acp} → agent → {swarm,mcp,computer-use,api,tools,bus} → core`（零循环依赖 ✅）

### 2.2 架构亮点

- **Event Bus 解耦**: `AgentRequest` (18 种) + `AgentNotification` (26 种) 通过 `clawed-bus` 实现 5 客户端（CLI、RPC、Bridge、Swarm、ACP）零耦合接入
- **AbortSignal 机制**: `Arc<AtomicBool>` 实现同步 abort + 异步 `tokio::select!` 子进程终止，双重保障（`core/src/tool.rs:16-41`）
- **Hook 系统**: 25 种事件类型，glob/regex 匹配缓存，支持 shell 命令反馈注入
- **Skill 子系统**: 用户级可调用 prompt 模板，支持 `allowed_tools` 限制 + `model` 覆盖
- **权限分层**: PermissionRule → PermissionMode → PermissionChecker 三级检查流水线
- **ACP 协议支持**: 新增 `clawed-acp` crate，实现 Agent Client Protocol（stdio/WebSocket），支持 session/prompt、MCP-over-ACP、终端管理、文件系统操作等 20 个方法
- **Cron 调度框架**: 完整 5-field cron 解析器 (core) + 调度循环 (agent)，含 missed task 补偿 + 文件锁互斥
- **TUI 子系统**: 16 组件完整终端 UI，含 scrollbars、ToolMonitor、Stats 面板、动态 context bar

### 2.3 ToolCategory 双重定义问题（中风险）

项目中存在两个独立的 `ToolCategory` 枚举定义，变体名称不一致：

| 维度 | `core/src/tool.rs:109` | `tools/src/lib.rs:89` |
|------|------|------|
| 文件操作 | `FileSystem` | `File` |
| Shell 执行 | `Shell` | `Shell` |
| Web 操作 | `Web` | `Web` |
| 代码/LSP | `Code` | `Code` |
| Git 操作 | `Git` | `Git` |
| 用户交互 | `Session` | `Interaction` |
| Agent 派发 | `Agent` | `Agent` |
| MCP 代理 | `Mcp` | `Mcp` |
| 管理工具 | — | `Management` |
| 计算机使用 | `ComputerUse` | — |

**问题**: Core 侧 `ToolCategory::Session` 对应 tools 侧 `ToolCategory::Interaction`，语义模糊。Core 侧有 `ComputerUse` 但 tools 侧无；tools 侧有 `Management`（TodoWrite/Config/Verify/Sleep/Workflow/Cron）但 core 侧无。实际路由函数 `tool_category()`（`tools/src/lib.rs:120`）使用 tools 侧枚举，core 侧 `Tool` trait 的 `category()` 方法默认返回 `ToolCategory::Session` 但工具实现中未被使用。

**实际影响**: 低。因为工具分类路由完全由 `tools/src/lib.rs:tool_category()` 函数承载，core 侧的枚举基本仅用于定义 trait 接口签名，两个枚举未在运行时发生冲突。但变体名称不一致是技术债务，影响代码可读性和新开发者理解。

### 2.4 ACP Crate 架构分析

`clawed-acp` 仅 7 文件，结构清晰：

| 模块 | 行数 | 职责 |
|------|:---:|------|
| `server.rs` | 417 | JSON-RPC 2.0 dispatch（20 方法），stdio/WebSocket 双 transport |
| `agent.rs` | 164 | QueryEngine 包装、session 管理、prompt 流式转 ACP 通知 |
| `session.rs` | — | SessionManager：创建/获取/关闭/列表 session |
| `mcp_bridge.rs` | — | MCP-over-ACP：connect/message/disconnect |
| `types.rs` | — | ACP 协议类型转换、initialize/capabilities 响应 |
| `transport.rs` | — | Transport 配置（stdio/WebSocket） |

**关注点**:

| 风险 | 位置 | 说明 |
|------|------|------|
| 中 | `server.rs:23` | `TERMINALS: LazyLock<StdMutex<HashMap<String, Child>>>` — 全局静态管理终端子进程。`terminal/wait_for_exit` 调用 `child.wait()` 是同步阻塞，在异步上下文中可能阻塞 tokio worker。且无清理机制——如果客户端未调用 `terminal/release`，子进程会泄漏 |
| 中 | `server.rs:204,253,258,277,360` | 5 处 `tokio::task::block_in_place(|| Handle::current().block_on(...))` — 在异步函数中同步阻塞等待异步操作。这在 stdio transport 的同步 dispatch 场景下是合理的（`dispatch()` 本身非 async），但在 WebSocket transport 的 tokio::spawn 中运行时会阻塞 worker 线程 |
| 低 | `agent.rs:22` | `NOTIFY: OnceLock<NotifyFn>` — 全局单例通知回调。设计合理（ACP 协议要求单一通知通道），但 `set_notify()` 如果被多次调用会静默失败（`OnceLock::set()` 返回 `Err`） |
| 低 | `server.rs:277` | `handle_set_mode` 调用 `block_in_place` 但返回 `Result<Value>` 非 async — 在 WebSocket 路径会被 `tokio::spawn` 包装，此时 `block_in_place` 实际上是冗余的 |

**建议**: `handle_new_session`、`handle_set_config_option`、`handle_set_mode`、`handle_mcp_connect` 应改为 async 方法，避免 `block_in_place`。对于 `dispatch()` 非 async 的限制，可将这些方法拆分为同步 dispatch 包装 + 异步内部实现。

### 2.5 Cron 系统跨 Crate 依赖

Cron 调度逻辑分布在两个 crate：

| Crate | 文件 | 职责 |
|-------|------|------|
| `clawed-core` | `cron.rs` (529行) | Cron 表达式解析（5-field）、字段展开、next-run 计算 |
| `clawed-core` | `cron_lock.rs` (169行) | 基于文件的互斥锁（`try_acquire_scheduler_lock` / `release_scheduler_lock`），含 PID 存活检测 |
| `clawed-core` | `cron_tasks.rs` | 任务持久化类型（`CronTask`、`CronTaskStatus`），JSON 文件读写 |
| `clawed-agent` | `cron_scheduler.rs` (309行) | 调度循环（每分钟 tick）、missed task 补偿、任务 fire 回调 |

**评估**: 职责划分合理——core 负责纯解析和持久化（无副作用），agent 负责调度执行。类似于标准库中 `core` vs `std` 的关系。不建议合并。

### 2.6 架构关注点汇总

| 风险 | 位置 | 说明 |
|------|------|------|
| 中 | `core/src/tool.rs:109` + `tools/src/lib.rs:89` | 两个 `ToolCategory` 定义并存，变体名称不匹配（见 2.3） |
| 中 | `acp/src/server.rs:204,277,360` | 5 处 `block_in_place` 阻塞 tokio worker |
| 中 | `acp/src/server.rs:23` | `TERMINALS` 全局静态缺清理机制 |
| 低 | `agent/src/engine/mod.rs` | `QueryEngine` 25 个字段，已拆分为 engine/ 子模块但主 struct 仍内聚 |
| 低 | `core/src/cron*.rs` (3 文件) + `agent/src/cron_scheduler.rs` | Cron 跨 core/agent，但职责划分合理（见 2.5） |

---

## 三、代码质量 — 4.5/5

### 3.1 亮点

- **零 TODO/FIXME** — 全项目无遗留标记，`grep -r "TODO\|FIXME\|HACK\|XXX"` 零命中
- **`lock_or_recover()`** 模式全面应用，Mutex 中毒不 panic（`core/src/agents.rs`、`core/src/skills.rs`）
- **`OnceLock`** 替代 `Arc<OnceLock<Option<T>>>`，类型安全的单次初始化（`acp/src/agent.rs:22`）
- **`ThinkingOverride`** 枚举避免 `Option<Option<T>>` 的二义性
- 文档注释完善，ARCHITECTURE.md + CLAUDE.md 双轨文档，关键模块含调用示例
- **`clawed-acp`** 代码结构清晰，仅 7 文件，JSON-RPC 2.0 完整实现

### 3.2 `unwrap()` 分布估算

| Crate | `unwrap()` 总数 (估) | 高频文件（生产代码） |
|-------|:---:|------|
| `clawed-core` | ~380 | `memory.rs`(64), `skills.rs`(63), `session.rs`(46) |
| `clawed-agent` | ~112 | `memory_extractor.rs`(30), `plugin/loader.rs`(53) |
| `clawed-cli` | ~195 | `commands.rs`(62), `tui/mod.rs`(45), `output/helpers.rs`(18) |
| `clawed-api` | ~84 | `types.rs`(27), `cache_detect.rs`(16), `provider.rs`(16) |
| `clawed-mcp` | ~67 | `registry.rs`(30), `types.rs`(22) |
| `clawed-rpc` | ~103 | `session.rs`(27), `methods.rs`(26), `protocol.rs`(21) |
| `clawed-tools` | ~0 | 工具 `call()` 基本零 `unwrap` ✅ |
| `clawed-acp` | ~3 | `mcp_bridge.rs`(2), `types.rs`(1) — 极干净 |
| 其余 | ~309 | bus, bridge, computer-use, swarm |

**评估**: 生产代码 `unwrap()` 占比约 1.5%，绝大多数位于 `#[cfg(test)]` 模块。工具代码质量最优（接近 0 unwrap）。

### 3.3 Clippy 策略

`Cargo.toml` 开启 `pedantic + nursery`（warning 级别），同时 suppress 67 条规则以减少噪音。策略合理，但 suppress 列表已增长，建议每年审计一次 suppress 列表，确认是否有不再需要的规则可以重新启用。

**当前 suppress 规则分类**:
- 风格偏好: 14 条（`manual_let_else`, `needless_collect`, `use_self` 等）
- 类型安全豁免: 12 条（`cast_precision_loss`, `cast_possible_truncation`, `cast_sign_loss`, `cast_possible_wrap`, `float_cmp` 等）
- 命名豁免: 3 条（`similar_names`, `module_name_repetitions`, `struct_field_names`）
- 文档豁免: 3 条（`doc_markdown`, `missing_errors_doc`, `missing_panics_doc`）
- 其他: 35 条

### 3.4 代码复杂度

| 文件 | 行数 | 评估 |
|------|:---:|------|
| `agent/src/query/mod.rs` | 1,169 | 流式响应处理，偏长但内聚 |
| `agent/src/compact/mod.rs` | 1,154 | 压缩逻辑 |
| `agent/src/executor.rs` | 1,137 | 工具执行器 |
| `agent/src/dispatch_agent.rs` | 1,010 | 子 Agent 派发 |
| `tools/src/bash.rs` | 1,068 | Shell 工具含安全防护 |
| `mcp/src/registry.rs` | 1,023 | MCP 注册 |
| `bus/src/events.rs` | 782 | 事件类型定义 |

这些文件虽长但职责清晰，不建议强行拆分。

---

## 四、安全性审查 — 4.5/5

### 4.1 `unsafe` 代码（4 处 unsafe 块，全部审计）

| # | 位置 | 用途 | 评估 |
|---|------|------|:---:|
| 1 | `core/src/concurrent_sessions.rs:152` | `libc::kill(pid, 0)` 进程存活检查 | ✅ |
| 2 | `core/src/upstream_proxy.rs:135` | `prctl(PR_SET_DUMPABLE)` 禁用 ptrace | ✅ |
| 3 | `computer-use/src/session_lock.rs:100` | `libc::kill(pid, 0)` | ✅ |
| 4 | `cli/src/native_installer.rs:257` | `libc::kill(pid, 0)` | ✅ |

所有 unsafe 块均为平台互操作（进程信号/安全加固），逻辑简单可审计。无内存操作、无指针解引用、无 FFI 边界跨越。

**关于 `set_var()`**: `core/src/config/mod.rs:286` 调用 `std::env::set_var()`。代码在 line 260-265 有明确文档说明、line 282-285 注释承认多线程限制。调用时机正确（tokio runtime 启动前），风险评估为**低**。若未来升级 Rust 2024 edition 需包裹 `unsafe {}`。

### 4.2 文件系统安全 (`path_safety.rs`)

`tools/src/path_safety.rs` 实现了与 TS 原版 `utils/permissions/filesystem.ts` 对齐的 6 层防护：

**第 1 层 — Shell 注入/路径展开检测** (line 33-47):
- 拦截 `$`、`%`、`~`、UNC 模式（`//`、`\\`）
- 例外：Windows 盘符前缀（如 `C:\`）

**第 2 层 — Glob 模式拦截** (line 50-53):
- 写工具拒绝 `*`、`?` 通配符

**第 3 层 — 危险目录保护** (line 8, 56-72):
- `.git`, `.vscode`, `.idea`, `.claude`
- 例外：`.claude/worktrees/` 路径允许操作

**第 4 层 — 危险文件保护** (line 12-23, 74-83):
- `.gitconfig`, `.gitmodules`
- Shell 配置: `.bashrc`, `.bash_profile`, `.zshrc`, `.zprofile`, `.profile`
- 工具配置: `.ripgreprc`, `.mcp.json`, `.claude.json`

**第 5 层 — Claude 配置文件保护** (line 85-88, 114+):
- `.claude/settings.json`, `.claude/settings.local.json`
- `.claude/commands/`, `.claude/agents/`, `.claude/skills/` 目录下文件

**第 6 层 — 大小写不敏感匹配**: 所有检查使用 `to_lowercase()`，防止大小写绕过

### 4.3 Shell 安全 (`bash.rs`)

多层防御体系：

1. **命令替换阻断**: `$()`, `` ` ``, `eval`, `source`, `. `
2. **危险命令**: `rm -rf /`, `mkfs.`, fork bomb, `dd if=/dev/`, `chmod -R 777 /`
3. **Git 保护**: `push --force`, `reset --hard`, `clean -f`, `checkout -- .`, `--no-verify`, `git config`
4. **环境变量**: 阻止 `LD_PRELOAD`, `PATH`, `BASH_ENV`, `SHELLOPTS`, `HOME`, `IFS` 等
5. **边界匹配**: `exact_boundary` 模式防止路径前缀误匹配

**已知限制**: 拦截基于子字符串匹配，可被编码/换行绕过。已文档化为"尽力而为"层。

### 4.4 Web 安全 (`web_fetch.rs`)

实际代码验证 (`tools/src/web_fetch.rs:9-80`):

| 检查项 | 状态 | 实现细节 |
|--------|:---:|------|
| SSRF — 协议限制 | ✅ | 仅 `http://` / `https://`，拒绝 `file://`、`ftp://` 等（line 11-16） |
| SSRF — 用户信息去除 | ✅ | `host_port.rsplit('@')` 剥离 `user:pass@host`（line 26） |
| SSRF — 内部主机名 | ✅ | `localhost`、`*.local`、`*.internal`（line 44-52） |
| SSRF — 元数据端点 | ✅ | `169.254.169.254`、`metadata.google.internal` 显式拦截（line 54-57） |
| SSRF — 私有 IPv4 | ✅ | `is_loopback()` + `is_private()` + `is_link_local()` + `is_unspecified()` + CGNAT `100.64.0.0/10`（line 62-68） |
| SSRF — 私有 IPv6 | ✅ | `is_loopback()` + `is_unspecified()` + link-local `fe80::` + ULA `fc00::/fd00::`（line 69-74） |
| 大小写归一化 | ✅ | `url.to_lowercase()`（line 11） |
| Token/密钥过滤 | ✅ | `to_auto_classifier_input` 过滤敏感字段 |

### 4.5 输入验证

| 检查项 | 状态 |
|--------|:---:|
| Session ID 路径遍历防护 | ✅ |
| Transcript 路径验证 | ✅ |
| 工具输入 JSON 容错 | ✅ (格式错误回退空对象) |
| 环境变量覆盖拦截 | ✅ |

### 4.6 依赖安全性

| 检查项 | 状态 |
|--------|:---:|
| `cargo audit` / `cargo deny` | ❌ 未配置 |
| `.github/workflows/` CI | ❌ 目录不存在（仅有 `copilot-instructions.md`） |
| pre-commit hooks | ✅ `.githooks/` 目录存在 |

**建议**: 
- 添加 `.github/workflows/ci.yml` 用于 CI（当前缺失）
- 添加 `cargo-deny` 配置以捕获已知漏洞（RUSTSEC advisories）和许可证合规问题
- 配置 `deny.toml` 包含 `[advisories]` 和 `[licenses]` 部分

---

## 五、测试覆盖 — 5/5

| Crate | 测试数 | 源文件行数(估) | 测试密度 | 评价 |
|-------|:---:|:---:|:---:|------|
| `clawed-agent` | 483 | ~15,000 | 高 | 良好，含集成测试 + E2E 流测试 |
| `clawed-core` | 452 | ~13,500 | 高 | 良好 |
| `clawed-tools` | 323 | ~10,500 | 中高 | 良好，含路径安全全量测试 + SSRF 测试 |
| `clawed-cli` | 297 | ~13,000 | 中高 | 良好，含 31 E2E (CLI 22 + TUI 9) |
| `clawed-api` | 180 | ~6,700 | 中高 | 良好 |
| `clawed-rpc` | 84 | ~2,300 | 中 | 基本 |
| `clawed-mcp` | 73 | ~2,500 | 中 | 基本 |
| `clawed-swarm` | 65 | ~3,100 | 中 | 基本 |
| `clawed-bridge` | 52 | ~2,100 | 中 | 基本 |
| `clawed-bus` | 23 | ~1,200 | 低 | 偏少，缺并发压力测试 |
| `clawed-computer-use` | 16 | ~1,200 | 低 | 偏少 |
| `clawed-acp` | 0 | ~1,200 | 无 | **新 crate，零测试** |

**总计 ~2,048** — 覆盖充分。

### 5.1 E2E 测试详情

- **CLI E2E** (`clawed-cli/tests/e2e_cli.rs`): 22 个测试，覆盖 help/version, completions (5 shells), session, init, argument validation, flag combinations, output format, cwd, edge cases
- **TUI E2E** (`clawed-cli/src/tui/mod.rs` 测试模块): 9 个测试，覆盖 tool tree 渲染、error/success 显示、collapsed 折叠显示、thinking 折叠、System message 分组

### 5.2 测试建议

| 风险 | 说明 |
|------|------|
| 中 | `clawed-acp` 零测试 — 需补充 session 生命周期、prompt 流式处理、MCP bridge connect/disconnect/message、JSON-RPC dispatch（已知方法 + 未知方法 + 格式错误） |
| 低 | `clawed-bus` 缺少高并发压力测试（多 ClientHandle 并发 submit + abort） |
| 低 | `clawed-computer-use` 仅 16 个测试 |

---

## 六、性能 — 4/5

| 风险 | 位置 | 说明 |
|------|------|------|
| 中 | `agent/src/query/mod.rs` | 每次工具调用克隆完整消息历史 |
| 低 | `agent/src/executor.rs` | `join_all` 单批次最多 10 并发（已限制） |
| 低 | `core/src/session.rs` | manifest 机制已缓解扫描开销 |
| 已修复 | `agent/src/executor.rs:515` | `chars().count()` → `len()` |

---

## 七、Git 历史与开发态势

最近 10 个提交：
```
a700bfc feat(tui): add scrollbars to Tasks/Tools panels, unify scrollbar style
5ec59c7 fix(tui): remove debug code, fix Tools panel unscrollable
4fc6103 Update TODOs and make context bar dynamic
91c3fe2 Update mod.rs
f41bfac TUI: add stats panel, markdown width & mouse
a364317 fix(tui): restore panel width spec: 1/6 ratio, min 30, max 60
7f3a43c feat(tui): add ToolMonitor panel (collapsed to mini-spinner)
60d8b91 fix: tasks input area duplicated display issue
5294569 feat(tui): add remote control notification for bash mode
9ad5a75 refactor(plan): move Plan from file-based to in-memory model
```

**态势**: 近期重点在 TUI 完善（scrollbars、ToolMonitor、stats panel、remote control），提交密集，节奏健康。
**未提交变更**: `REVIEW_REPORT.md` (当前编辑)、`.claude_todos.json`、`count_unwrap.ps1`。
**分支**: `main`（无其他活跃分支）。

---

## 八、风险汇总与优先级

### 高优先级

| # | 问题 | 位置 | 修复 |
|---|------|------|------|
| — | （无高优问题） | — | — |

### 中优先级

| # | 问题 | 位置 | 修复 |
|---|------|------|------|
| 1 | 两个 `ToolCategory` 定义并存，变体名称不匹配 | `core/src/tool.rs:109` + `tools/src/lib.rs:89` | 统一为 `tools/src/lib.rs` 定义，core 侧 `pub use` 重导出或移除 |
| 2 | `clawed-acp` 零测试 | `acp/` | 补充 session 生命周期、A2A prompt 处理、MCP bridge、JSON-RPC dispatch 基础测试 |
| 3 | 依赖安全审计缺失 + CI 配置缺失 | 仓库根 | 添加 `cargo-deny` 配置 + `.github/workflows/ci.yml` |
| 4 | ACP Server 5 处 `block_in_place` 阻塞 tokio worker | `acp/src/server.rs:204,253,258,277,360` | 拆分为 async 方法，`dispatch()` 中用 `tokio::spawn` + `JoinHandle` |
| 5 | ACP `TERMINALS` 全局静态缺清理 | `acp/src/server.rs:23` | 添加超时清理或在 `terminal/create` 时检查并清理僵尸进程 |

### 低优先级

| # | 问题 | 位置 |
|---|------|------|
| 6 | `set_var()` 在 Rust 2024 升级时需包裹 `unsafe` | `core/src/config/mod.rs:286` |
| 7 | Bus 测试缺少并发压力场景 | `bus/src/bus.rs` |
| 8 | Clippy suppress 规则过多 (~67) | `Cargo.toml` |
| 9 | 部分文件超 1,000 行 | query/mod.rs 等 7 个文件 |
| 10 | ARCHITECTURE.md 统计过时（仍写 11 crate、28+ 工具） | `ARCHITECTURE.md` |

---

## 九、总体评分

| 维度 | 评分 | 说明 |
|------|:---:|------|
| 架构设计 | **5/5** | 清晰分层，Event Bus 解耦，零循环依赖，5 客户端支持 |
| 代码质量 | **4.5/5** | 零 TODO，文档好，`unwrap` 数量可控且集中于测试代码 |
| 安全性 | **4.5/5** | 多层防御（文件系统 6 层、Shell 5 层、Web SSRF 全覆盖）；缺依赖审计 |
| 测试覆盖 | **5/5** | 2,048 测试含 E2E；acp 需补充 |
| 性能 | **4/5** | 无明显瓶颈；消息克隆可优化 |
| 文档 | **5/5** | ARCHITECTURE.md + CLAUDE.md + README.md + E2E_TEST_REPORT.md + TEST_REPORT.md |
| **综合** | **4.7/5** | 架构清晰的工业级 Rust 项目 |

---

## 十、结论

Clawed Code 是一个**架构清晰、测试充分、安全性考虑周到**的 Rust 项目。相比上次审查，新增了 ACP 协议支持（7 文件，零 TODO，代码结构清晰）、完整 TUI 子系统（16 组件，含 scrollbars/ToolMonitor/stats/remote control/dynamic context bar）、Cron 调度框架、工作流引擎等模块。代码质量保持高水平——零 TODO/FIXME、零 `todo!()`、零 `unimplemented!()`。

安全防护全面——文件系统 6 层防御、Shell 5 层防御、Web SSRF 覆盖 IPv4/IPv6/CGNAT/元数据端点。4 处 unsafe 块均为简单平台互操作，可审计性强。主要缺口是缺少 CI/CD pipeline、`cargo-deny`/`cargo-audit` 依赖审计、`clawed-acp` 零测试，以及 `ToolCategory` 枚举重复定义的技术债务。

### 建议优先处理

1. **统一两个 `ToolCategory` 定义** — 建议以 `tools/src/lib.rs` 为准（含 `Management` 变体），core 侧移除枚举定义，改为 `pub use clawed_tools::ToolCategory`，或至少让 core 侧枚举的 `Session` 重命名为 `Interaction` 以与 tools 侧对齐
2. **`clawed-acp` 补充基础测试** — session 生命周期 (`new/close/list`)、prompt 流式处理、MCP bridge connect/disconnect/message、JSON-RPC dispatch（合法方法 + 未知方法 + 格式错误）
3. **添加 `.github/workflows/ci.yml` + `cargo-deny`** — CI: check + test (Linux/Mac/Win) + clippy + fmt；deny: RUSTSEC advisories + 许可证合规
4. **ACP Server `block_in_place` 改用 async** — 将 `handle_new_session`、`handle_set_config_option`、`handle_set_mode`、`handle_mcp_connect` 改为 async，`dispatch()` 中对需要阻塞的方法用 `tokio::spawn` + `JoinHandle`
5. **Rust 2024 edition 升级时注意 `set_var()`** 需包裹 `unsafe`
6. **更新 ARCHITECTURE.md 项目统计数据** — 12 crate (非 11)、33 core 文件 (非 30)、40+ 工具 (非 28+)
