# Clawed-Code vs. Official Managed Agents — 对比文档

> 面向本项目的技术视角，对比 clawed-code 的本地 subagent/coordinator/swarm 架构与 Anthropic 官方 Managed Agents API 的设计差异。

---

## 1. 架构模型总览

| 维度 | Clawed-Code（本项目） | Anthropic Managed Agents |
|------|----------------------|--------------------------|
| **执行位置** | 本地进程内 `tokio::spawn` | Anthropic 编排层 + 远程容器 |
| **配置持久化** | 内存结构体 / 本地 `.md` 文件 | `POST /v1/agents` 持久化 Agent 对象 |
| **会话生命周期** | 同步等待或异步 `tokio::spawn`，进程结束即销毁 | 独立 Session 对象，有 `idle/running/terminated` 状态机 |
| **环境隔离** | 共享进程，无沙箱 | 每个 Session 分配独立 Container（sandbox） |
| **版本管理** | 无版本概念 | Agent 每次 update 创建不可变版本，Session 可 pin 版本 |
| **多进程/分布式** | 有 `clawed-swarm`（kameo actor 模型）可跨进程 | Session 本身即远程运行，天然分布式 |
| **对外协议** | **JSON-RPC 2.0 over stdio/TCP** + 多通道桥接（飞书等） | **REST API** (`/v1/agents`, `/v1/sessions`) + SSE 事件流 |
| **端点数量** | 17 个 RPC 方法 + 33+ 种通知类型 | 50+ 个 REST 端点 |

```
Clawed-Code:                          Managed Agents:
┌─ Main Agent Loop ─┐                 ┌─ Client (your app) ─┐
│  tokio::spawn     │                 │                     │
│  ├─ sub-agent 1   │                 │  POST /v1/agents    │──▶ Agent (versioned config)
│  ├─ sub-agent 2   │                 │  POST /v1/sessions  │──▶ Session (lifecycle)
│  └─ sub-agent N   │                 │  GET  /events/stream│◀── SSE event stream
└────────┬──────────┘                 └─────────────────────┘
         │ JSON-RPC 2.0               ┌─ Anthropic orchestration ─┐
         │ (stdio / TCP)              │  Agent loop runs here      │
┌────────┴──────────┐                 │  Tool calls → Container     │
│  clawed-rpc       │                 └─────────────────────────────┘
│  ┌──────────────┐ │
│  │StdioTransport│ │
│  │TcpTransport  │ │
│  └──────────────┘ │
└────────┬──────────┘
         │ EventBus
┌────────┴──────────┐
│  clawed-bridge    │
│  ┌──────────────┐ │
│  │Feishu Adapter│ │
│  │Slack Adapter │ │
│  └──────────────┘ │
└───────────────────┘
```

---

## 2. RPC 协议与端点

> Clawed-Code 内置了完整的 JSON-RPC 2.0 协议层（`clawed-rpc`），支持 stdio 和 TCP 两种传输方式，另有 `clawed-bridge` 多通道桥接层（飞书等平台适配器）。Managed Agents 只有 REST API，没有 RPC 抽象层。

### 2.1 传输层

| 维度 | Clawed-Code | Managed Agents |
|------|-------------|----------------|
| **协议** | JSON-RPC 2.0 | REST + SSE |
| **传输** | Stdio (换行分隔 JSON) / TCP | HTTPS |
| **帧格式** | `{"jsonrpc":"2.0","id":1,"method":"agent.submit","params":{...}}\n` | HTTP 请求/响应 + Server-Sent Events |
| **请求-响应** | 双向（`Request` → `Response`） | 单向（HTTP request → response） |
| **服务端推送** | `Notification`（无 id，不期待响应） | SSE `event` stream |

### 2.2 入站方法（Client → Server，17 个）

| RPC 方法 | 内部 `AgentRequest` | 说明 |
|----------|---------------------|------|
| `agent.submit` | `Submit { text, images }` | 提交用户消息 |
| `agent.abort` | `Abort` | 中断当前操作 |
| `agent.compact` | `Compact { instructions }` | 手动触发上下文压缩 |
| `agent.setModel` | `SetModel { model }` | 切换模型 |
| `agent.clearHistory` | `ClearHistory` | 清空对话历史 |
| `agent.permission` | `PermissionResponse { request_id, granted, remember }` | 响应权限请求 |
| `agent.sendMessage` | `SendAgentMessage { agent_id, message }` | 向子 agent 发送消息 |
| `agent.stopAgent` | `StopAgent { agent_id }` | 停止后台子 agent |
| `agent.listModels` | `ListModels` | 列出可用模型 |
| `agent.listTools` | `ListTools` | 列出可用工具 |
| `session.save` | `SaveSession` | 保存会话到磁盘 |
| `session.status` | `GetStatus` | 查询会话状态 |
| `session.shutdown` | `Shutdown` | 优雅关闭 |
| `session.load` | `LoadSession { session_id }` | 加载已保存会话 |
| `mcp.connect` | `McpConnect { name, command, args, env }` | 连接 MCP 服务器（带命令白名单校验） |
| `mcp.disconnect` | `McpDisconnect { name }` | 断开 MCP 服务器 |
| `mcp.listServers` | `McpListServers` | 列出已连接 MCP 服务器 |

### 2.3 出站通知（Server → Client，33+ 种）

按命名空间分组：

| 命名空间 | 通知类型 | 说明 |
|----------|----------|------|
| **agent** | `textDelta`, `thinkingDelta` | 流式文本/思考增量 |
| **agent** | `toolStart`, `toolReady`, `toolComplete` | 工具生命周期三阶段 |
| **agent** | `turnStart`, `turnComplete`, `assistantMessage` | 轮次生命周期 |
| **agent** | `historyCleared`, `modelChanged`, `contextWarning` | 会话状态变更 |
| **agent** | `compactStart`, `compactComplete` | 上下文压缩进度 |
| **agent** | `spawned`, `progress`, `complete`, `terminated` | 子 agent 生命周期 |
| **agent** | `tool_selected`, `conflict_detected` | 工具选择/文件冲突 |
| **agent** | `modelList`, `toolList`, `memoryExtracted`, `thinking_changed`, `cache_break_set` | 查询响应 |
| **agent** | `error` | 错误通知 |
| **session** | `start`, `end`, `saved`, `status` | 会话生命周期 |
| **mcp** | `connected`, `disconnected`, `error`, `serverList` | MCP 生命周期 |
| **swarm** | `team_created`, `team_deleted`, `agent_spawned`, `agent_terminated`, `agent_query`, `agent_reply` | Swarm 分布式 agent 事件 |

### 2.4 权限请求（双向）

```
Agent Core ──[agent.permissionRequest]──▶ Client
  │  { request_id, tool_name, input, risk_level, description }
  │
Client ──[agent.permission]────────────▶ Agent Core
  │  { request_id, granted, remember }
```

Managed Agents 的等效流程是：Session 进入 `idle` + `stop_reason.requires_action` → Client 发送 `user.tool_confirmation` 事件。

### 2.5 RpcSession 架构

```
┌─ RpcSession ───────────────────────────────────────────┐
│  tokio::select! {                                      │
│    // Inbound: transport → parse_request → bus         │
│    msg = transport.read_message() →                    │
│      parse_request(method, params) → AgentRequest →    │
│        client.send_request() → Event Bus               │
│                                                        │
│    // Outbound: bus notifications → transport          │
│    notif = client.subscribe_notifications().recv() →   │
│      notification_to_jsonrpc() → transport.write()     │
│                                                        │
│    // Permission: bus → transport → wait for response  │
│    perm = client.recv_permission_request() →           │
│      "agent.permissionRequest" → transport.write()     │
│  }                                                     │
└─────────────────────────────────────────────────────────┘
```

**与 Managed Agents 对比：** Managed Agents 的 Client 需要自己实现事件轮询/SSE 消费、消息发送、权限确认等逻辑。Clawed-Code 通过 JSON-RPC 把这些模式标准化了——Client 只需发 JSON 行、收 JSON 行。

### 2.6 clawed-bridge 多通道桥接

`clawed-bridge` 在 RPC 层之上提供了**平台适配器框架**：

| 组件 | 说明 |
|------|------|
| `ChannelGateway` | 核心网关，路由 inbound message → Event Bus → outbound notification |
| `ChannelAdapter` trait | 平台适配器接口（飞书、Slack 等实现） |
| `SessionRouter` | ChannelId → Agent Session 映射，支持 idle timeout 自动回收 |
| `MessageFormatter` | 将 `AgentNotification` 流聚合为平台友好的消息格式 |
| MCP 命令白名单 | `mcp.connect` 仅允许 `npx/node/python/uvx/deno/bun/cargo/docker` 等安全命令 |

**Managed Agents 无等效层**——MA 的 Client 直接操作 REST API，没有内置的多平台消息路由。

---

## 3. Agent 配置管理

| 维度 | Clawed-Code | Managed Agents |
|------|-------------|----------------|
| **定义方式** | `AgentDefinition` 结构体 + `.md` 文件 frontmatter | `POST /v1/agents` JSON body |
| **持久化** | 文件系统 `.claude/agents/*.md` | Anthropic 平台持久化，返回 `agent_id` |
| **版本控制** | 无（覆盖写入即更新） | 每次 update 自动创建新版本（时间戳），Sessions 可 pin |
| **发现机制** | 从 cwd → parent → HOME 递归扫描 `.claude/agents/` | `GET /v1/agents` 列表 |
| **内置类型** | `General/Explore/Plan/CodeReview/Verification/Worker` 6 种硬编码 profile | 无内置类型，通过 `system` prompt + `tools` 自定义 |
| **最大数量** | 无限制（本地文件） | 无明确限制（create RPM 60） |

### Clawed-Code AgentDefinition 关键字段

```rust
pub struct AgentDefinition {
    pub agent_type: String,       // 唯一标识
    pub description: String,
    pub system_prompt: String,
    pub allowed_tools: Vec<String>,
    pub disallowed_tools: Vec<String>,
    pub model: Option<String>,    // "inherit" / 具体模型名
    pub max_turns: Option<u32>,
    pub background: bool,
    pub skills: Vec<String>,
    pub memory: Option<AgentMemoryScope>,  // user/project/local
    pub source: AgentSource,      // built-in/user/project/local/plugin
}
```

### Managed Agents CreateAgent 关键字段

```json
{
  "name": "string (required)",
  "model": "claude-opus-4-6 (required)",
  "system": "string (up to 100K chars)",
  "tools": [{ "type": "agent_toolset_20260401" }],
  "mcp_servers": [{ "type": "url", "name": "github", "url": "..." }],
  "skills": [{ "type": "anthropic", "skill_id": "xlsx" }],
  "metadata": {}
}
```

**核心差异：** Managed Agents 强制 Agent ONCE → Session MANY 的两阶段模式；Clawed-Code 的 Agent 是即插即用的工具调用，每次调用 `Agent` tool 时现场构建配置。

---

## 4. Subagent 执行机制对比

| 维度 | Clawed-Code `DispatchAgentTool` | Managed Agents Session |
|------|-------------------------------|------------------------|
| **触发方式** | 模型调用 `Agent` tool | Client 调用 `sessions.create()` |
| **执行模式** | 同步（默认）或 `tokio::spawn` 后台 | 始终远程异步，SSE 事件流 |
| **后台并发** | Semaphore 限制，默认 `DEFAULT_MAX_CONCURRENT_AGENTS = 8` | 无明确并发限制（受 RPM 约束） |
| **结果传递** | 同步返回 `ToolResult::text` / 后台通过 `TaskNotification` XML 注入 | SSE `agent.message`, `agent.tool_use` 等事件 |
| **中断机制** | `TaskStop` tool → `CancellationToken::cancel()` | `user.interrupt` 事件 |
| **后续消息** | `SendMessage` tool → `mpsc::unbounded_channel` | `user.message` 事件（消息队列） |
| **上下文压缩** | 无（sub-agent 不主动 compact） | 自动 `agent.thread_context_compacted` |
| **Prompt 缓存** | 无显式利用 | 自动 prompt caching |
| **扩展思考** | 无显式控制 | 默认开启 extended thinking |

### 后台执行 vs Session 状态机

**Clawed-Code** 的后台 agent 只有三种状态：
- `Running` → `Completed` / `Failed` / `Killed`

**Managed Agents** Session 有更丰富的生命周期：
- `rescheduling → running ↔ idle → terminated`

Managed Agents 的 `idle` 状态特别重要——agent 可能等待 `user.tool_confirmation`、`user.custom_tool_result` 或仅仅是 `end_turn`，需要通过 `stop_reason.type` 区分。

---

## 5. 工具权限与交互

| 维度 | Clawed-Code | Managed Agents |
|------|-------------|----------------|
| **权限策略** | `PermissionChecker`（BypassAll / AutoAllow / AskAlways）+ `BusPermissionPrompter` | `permission_policy: { type: 'always_ask' }` |
| **权限确认** | 通过事件总线 → TUI 弹窗 / 终端 prompt | Session 进入 `idle`，client 发送 `user.tool_confirmation` |
| **Custom Tool** | 无独立概念，所有工具在本地执行 | `agent.custom_tool_use` → session 挂起 → client 返回 `user.custom_tool_result` |
| **MCP** | 本地 MCP 客户端发现 + 执行 | Agent 声明 `mcp_servers`，Session 通过 `vault_ids` 提供凭证 |
| **凭证管理** | 环境变量 / 本地配置文件 | Vaults + Credentials API（OAuth 自动刷新） |

### Custom Tool 对比

**Managed Agents** 的 custom tool 是一个关键的编排模式——凭证永远保留在 host 侧，sandbox 看不到密钥：

```
Agent emits: agent.custom_tool_use { name: "linear_graphql", input: {...} }
Session → idle (requires_action)
Client executes with host credentials → sends user.custom_tool_result
Session → running (agent continues)
```

**Clawed-Code** 目前没有等效机制。sub-agent 和主 agent 共享同一套工具注册表，无法实现"工具在 host 执行、sandbox 只发请求"的模式。

---

## 6. 事件流与通信

| 维度 | Clawed-Code | Managed Agents |
|------|-------------|----------------|
| **事件传输** | JSON-RPC `Notification`（stdio/TCP 换行 JSON）| SSE `GET /v1/sessions/{id}/events/stream` |
| **事件类型** | 33+ 种出站通知 + 双向权限请求 | 30+ 种事件类型 |
| **历史回放** | 不支持 | `GET /v1/sessions/{id}/events` 分页拉取历史 |
| **重连去重** | 不适用（本地） | `processed_at` 去重 + history consolidation |
| **消息队列** | `mpsc::unbounded_channel` 单条消息 | `user.message` 服务端排队，按序处理 |

### Managed Agents 事件类型（Clawed-Code 无等效项）

| MA Event | Clawed-Code Equivalent |
|----------|----------------------|
| `agent.message` | `TextDelta` (累积后) |
| `agent.thinking` | ❌ 无 |
| `agent.tool_use` / `agent.tool_result` | `ToolUseStart` |
| `agent.mcp_tool_use` / `agent.mcp_tool_result` | ❌ 无区分 |
| `agent.custom_tool_use` | ❌ 无 |
| `agent.thread_context_compacted` | ❌ 无 |
| `session.status_idle` | stream 结束 (`None`) |
| `session.status_running` | stream 开始 |
| `session.status_terminated` | `Error` / stream 结束 |
| `session.error` | `Error` |
| `span.model_request_start` / `span.model_request_end` | ❌ 无 |
| `user.message` (echo) | ❌ 无 |
| `user.interrupt` | `TaskStop` / `abort_signal` |

---

## 7. 文件与资源管理

| 维度 | Clawed-Code | Managed Agents |
|------|-------------|----------------|
| **文件访问** | 直接 `Read`/`Write`/`Glob`/`Bash` 操作宿主文件系统 | 通过 `agent_toolset_20260401` 在容器内操作 |
| **文件上传** | 不适用（本地） | `POST /v1/files` → 作为 session resource 挂载 |
| **代码仓库** | 直接在 cwd 工作 | `github_repository` resource，自动 clone + checkout |
| **输出文件** | 直接写入 cwd | 写入 `/mnt/session/outputs/` 自动捕获 |
| **挂载路径** | 不适用 | `mount_path` 必须绝对路径 |

---

## 8. Environments 概念

**Managed Agents 独有**：Environments 是可复用的容器配置模板，定义网络策略、包集合等。Session 创建时必须引用 `environment_id`。

Clawed-Code 没有等效概念——所有 subagent 共享宿主机的执行环境。

```
MA Environment config:
{
  "name": "dev-env",
  "config": {
    "type": "cloud",
    "networking": { "type": "unrestricted" | "package_managers_and_custom" },
    "packages": { }
  }
}
```

---

## 9. 多 Agent 编排

| 维度 | Clawed-Code | Managed Agents |
|------|-------------|----------------|
| **Coordinator 模式** | `AgentTracker` + `DispatchAgentTool` + `SendMessageTool` + `TaskStopTool` | Client 侧编排多个 Session |
| **Worker 限制** | `worker_tool_names()` 排除 `Agent/SendMessage/TaskStop/AskUserQuestion` | 每个 Session 有独立的 tools 配置 |
| **并发控制** | `Semaphore(DEFAULT_MAX_CONCURRENT_AGENTS = 8)` | 无内置限制，受 API RPM 约束 |
| **分布式** | `clawed-swarm` 模块（kameo actor 模型）支持跨进程 | Session 本身是远程的，天然可编排 |

### Swarm vs Managed Agents

Clawed-Code 的 `clawed-swarm` 使用了 **kameo actor 模型**：
- 每个 Agent 是一个 Kameo Actor，持有独立的 API session
- 通过 `SwarmNotifier` 广播状态变更
- 支持 team 创建、agent spawn/terminate、message routing
- 是进程内 actor 模型，非远程服务

Managed Agents 的"swarm"概念是 Client 创建多个 Session，各自独立执行，通过事件流接收输出。

---

## 10. 能力差距清单

### Managed Agents 有但 Clawed-Code 缺失

| 能力 | 重要度 | 说明 |
|------|--------|------|
| **沙箱隔离** | 高 | 每个 session 独立容器，工具执行不影响宿主机 |
| **Agent 版本控制** | 中 | 不可变版本 + Session pin，支持回滚和 A/B 测试 |
| **Context Compaction** | 高 | 自动压缩对话历史防止上下文溢出 |
| **Prompt Caching** | 中 | 官方自动利用缓存，降低成本 |
| **Extended Thinking** | 中 | 默认开启，返回思考链 |
| **Custom Tool Round-trip** | 高 | 凭证保留在 host 侧的安全模式 |
| **MCP 凭证 Vault** | 中 | 集中管理 + OAuth 自动刷新 |
| **SSE 事件流** | 中 | 30+ 种事件类型，支持历史回放和重连 |
| **Session 资源挂载** | 低 | 文件和 GitHub 仓库自动挂载 |
| **Environment 模板** | 低 | 可复用网络/包配置 |

### Clawed-Code 有但 Managed Agents 缺失

| 能力 | 重要度 | 说明 |
|------|--------|------|
| **Agent Definition 文件系统** | 中 | `.md` 文件定义 + 层级发现 + 缓存，灵活的本地配置 |
| **Memory Scope** | 中 | `user/project/local` 三级持久记忆 |
| **BusPermissionPrompter** | 低 | 通过事件总线路由权限请求，TUI 友好 |
| **JSON-RPC 协议层** | 中 | 17 个标准方法 + 33+ 通知类型，stdio/TCP 传输，IDE/桥接友好 |
| **多通道桥接** | 中 | `clawed-bridge` 平台适配器框架（飞书/Slack 等），带 SessionRouter |
| **Actor 模型 (Swarm)** | 低 | kameo actor 支持更细粒度的进程内并发编排 |

---

## 11. 对齐建议（按优先级）

### P0 — 高价值/低成本

1. **Context Compaction**：当前 sub-agent 无压缩机制，长对话会溢出。可借鉴 MA 的自动压缩策略。
2. **Custom Tool 模式**：实现 `custom_tool` 概念，让某些工具在 host 侧执行（特别是含敏感凭证的）。
3. **Extended Thinking 支持**：sub-agent 已支持 `thinking: None` 配置，应暴露为可选项。

### P1 — 架构改进

4. **Session 状态机**：当前 sub-agent 只有 running/completed/failed/killed 四种状态，缺少 `idle`（等待确认/等待输入）的细粒度表达。
5. **Prompt Caching**：sub-agent 每次新建对话，无法利用缓存。可考虑共享前缀消息的缓存策略。
6. **Agent 版本概念**：为 `AgentDefinition` 添加版本号，支持回滚。

### P2 — 长期规划

7. **Environment 抽象**：为 sub-agent 定义执行环境配置（工具白名单、网络策略等）。
8. **SSE-like 事件流**：对外暴露统一的事件流接口，与 MA API 事件类型对齐，便于桥接。
9. **远程 Session 支持**：在 `clawed-api` 中实现 MA API 客户端，使 clawed-code 能作为 Managed Agents 的 client orchestrator。
