# Clawed-Code Managed Agents — JSON-RPC 协议规范

> 将 Anthropic Managed Agents REST API（51 个端点）映射到 clawed-code 的 JSON-RPC 2.0 协议上。
> 不新增 HTTP 层，不新增 REST 端点——全部通过扩展现有 RPC 方法和通知类型实现。

---

## 1. 设计原则

```
1. 一个长连接搞定所有事
   JSON-RPC over TCP/stdio，双向通信。不需要 SSE + REST 两个通道。

2. 方法名保持 MA 语义
   agent.create 对应 POST /v1/agents，session.create 对应 POST /v1/sessions。

3. 已有方法不动
   agent.submit / agent.abort / agent.permission 等已有方法保持不变，
   它们已经对应 MA 的 user.message / user.interrupt / user.tool_confirmation。

4. Notification = SSE Event
   MA 的 SSE 事件流用 JSON-RPC Notification 推送，不需要单独的 stream 端点。

5. 请求-响应 = REST CRUD
   MA 的 GET/POST/DELETE 操作用 JSON-RPC Request → Response 完成。
```

---

## 2. 完整方法目录

### 2.1 已有方法（不动，17 个）

| RPC 方法 | 对应 MA 操作 | 内部 AgentRequest |
|----------|-------------|-------------------|
| `agent.submit` | `POST /v1/sessions/{id}/events` (user.message) | `Submit` |
| `agent.abort` | `POST /v1/sessions/{id}/events` (user.interrupt) | `Abort` |
| `agent.permission` | `POST /v1/sessions/{id}/events` (user.tool_confirmation) | `PermissionResponse` |
| `agent.compact` | — (clawed 特有) | `Compact` |
| `agent.setModel` | 隐含在 agent config 中 | `SetModel` |
| `agent.clearHistory` | — (clawed 特有) | `ClearHistory` |
| `agent.sendMessage` | `POST /v1/sessions/{id}/events` (user.message) | `SendAgentMessage` |
| `agent.stopAgent` | `DELETE /v1/sessions/{id}` | `StopAgent` |
| `agent.listModels` | — (clawed 特有) | `ListModels` |
| `agent.listTools` | — (clawed 特有) | `ListTools` |
| `session.save` | — (clawed 特有) | `SaveSession` |
| `session.status` | `GET /v1/sessions/{id}` | `GetStatus` |
| `session.shutdown` | `POST /v1/sessions/{id}/archive` | `Shutdown` |
| `session.load` | — (clawed 特有) | `LoadSession` |
| `mcp.connect` | 隐含在 agent.mcp_servers + vault | `McpConnect` |
| `mcp.disconnect` | 隐含在 agent.mcp_servers 移除 | `McpDisconnect` |
| `mcp.listServers` | 隐含在 agent 配置查询 | `McpListServers` |

### 2.2 新增方法（按 MA 端点映射，24 个）

#### Agent 配置（6 个，对应 MA 的 `/v1/agents`）

| RPC 方法 | 对应 MA 端点 | 说明 |
|----------|-------------|------|
| `agent.create` | `POST /v1/agents` | 创建 Agent 配置，返回 agent_id + 版本号 |
| `agent.list` | `GET /v1/agents` | 列出所有 Agent 配置 |
| `agent.get` | `GET /v1/agents/{id}` | 获取单个 Agent 详情 |
| `agent.update` | `POST /v1/agents/{id}` | 更新 Agent，自动创建新版本 |
| `agent.archive` | `POST /v1/agents/{id}/archive` | 归档 Agent（不可逆） |
| `agent.listVersions` | `GET /v1/agents/{id}/versions` | 列出 Agent 版本历史 |

#### Session 管理（6 个，对应 MA 的 `/v1/sessions`）

| RPC 方法 | 对应 MA 端点 | 说明 |
|----------|-------------|------|
| `session.create` | `POST /v1/sessions` | 创建 Session，启动 agent loop |
| `session.list` | `GET /v1/sessions` | 列出所有 Session（分页） |
| `session.get` | `GET /v1/sessions/{id}` | 获取 Session 详情 |
| `session.update` | `POST /v1/sessions/{id}` | 更新 Session 元数据（仅 title） |
| `session.delete` | `DELETE /v1/sessions/{id}` | 删除 Session |
| `session.archive` | `POST /v1/sessions/{id}/archive` | 归档 Session（只读） |

#### 事件历史（1 个，对应 MA 的 `/v1/sessions/{id}/events`）

| RPC 方法 | 对应 MA 端点 | 说明 |
|----------|-------------|------|
| `session.listEvents` | `GET /v1/sessions/{id}/events` | 分页拉取事件历史（stream 功能由 Notification 天然覆盖） |

> **为什么不需要 `session.streamEvents`？** MA 需要 SSE stream 是因为 HTTP 是单向的。
> JSON-RPC over TCP 连接上，服务端随时可以发 Notification——stream 就是默认行为。

#### Environment（5 个，对应 MA 的 `/v1/environments`）

| RPC 方法 | 对应 MA 端点 | 说明 |
|----------|-------------|------|
| `environment.create` | `POST /v1/environments` | 创建环境模板 |
| `environment.list` | `GET /v1/environments` | 列出所有环境 |
| `environment.get` | `GET /v1/environments/{id}` | 获取环境详情 |
| `environment.update` | `POST /v1/environments/{id}` | 更新环境（仅影响新 Session） |
| `environment.archive` | `POST /v1/environments/{id}/archive` | 归档环境 |

#### Vault（3 个，对应 MA 的 `/v1/vaults`）

| RPC 方法 | 对应 MA 端点 | 说明 |
|----------|-------------|------|
| `vault.create` | `POST /v1/vaults` | 创建凭证库 |
| `vault.list` | `GET /v1/vaults` | 列出所有 Vault |
| `vault.delete` | `DELETE /v1/vaults/{id}` | 删除 Vault |

#### Credential（3 个，对应 MA 的 `/v1/vaults/{id}/credentials`）

| RPC 方法 | 对应 MA 端点 | 说明 |
|----------|-------------|------|
| `credential.create` | `POST /v1/vaults/{id}/credentials` | 创建凭证（OAuth 或静态 token） |
| `credential.list` | `GET /v1/vaults/{id}/credentials` | 列出凭证 |
| `credential.delete` | `DELETE /v1/vaults/{id}/credentials/{id}` | 删除凭证 |

> Environment/Vault/Credential 的 CRUD 方法较多，初期可按需实现——
> 最核心的是 `agent.create` + `session.create` + `session.listEvents`。

---

## 3. 新增方法详情

### 3.1 `agent.create`

**请求参数：**
```json
{
  "name": "Coding Agent",
  "model": "claude-sonnet-4-6",
  "system": "You are a helpful coding agent.",
  "tools": [{ "type": "agent_toolset" }],
  "mcp_servers": [{ "type": "url", "name": "github", "url": "..." }],
  "skills": [{ "type": "anthropic", "skill_id": "xlsx" }],
  "description": "..."
}
```

**响应：**
```json
{
  "id": "agent_abc123",
  "version": 1,
  "created_at": "2026-04-12T10:00:00Z"
}
```

**内部实现：** 写入 `.claude/agents/` + 版本计数器 + 生成 `AgentDefinition`。

### 3.2 `agent.list`

**请求参数：** `{}` （可选 `limit` / `after_id`）

**响应：** `{ "agents": [...], "has_more": false }`

### 3.3 `agent.get`

**请求参数：** `{ "agent_id": "agent_abc123" }`

**响应：** 完整 Agent 配置，含版本号。

### 3.4 `agent.update`

**请求参数：** `{ "agent_id": "agent_abc123", ...变更字段... }`

**响应：** `{ "version": 2, "updated_at": "..." }`

**内部实现：** 追加新版本，旧版本保留。

### 3.5 `agent.archive` / `agent.listVersions`

标准 CRUD，无特殊逻辑。

### 3.6 `session.create`

**请求参数：**
```json
{
  "agent": "agent_abc123",
  "environment_id": "env_001",
  "title": "My Session",
  "resources": [
    { "type": "github_repository", "url": "https://github.com/owner/repo" },
    { "type": "file", "file_id": "file_abc", "mount_path": "/workspace/data.csv" }
  ],
  "vault_ids": ["vlt_001"]
}
```

**响应：**
```json
{
  "session_id": "sess_abc123",
  "status": "running"
}
```

**内部实现：**
1. 加载 `agent_id` 对应的 AgentDefinition
2. 创建 EventBus `ClientHandle`
3. 启动 agent loop（query_stream）
4. 推送 `session.statusRunning` Notification
5. 返回 `session_id`

### 3.7 `session.listEvents`

**请求参数：**
```json
{
  "session_id": "sess_abc123",
  "limit": 100,
  "after_id": "sevt_001"
}
```

**响应：**
```json
{
  "events": [
    { "id": "sevt_002", "type": "agent.message", "data": {...}, "processed_at": "..." },
    ...
  ],
  "has_more": true
}
```

**内部实现：** 从 EventBus 事件持久化存储（ring buffer 或 sqlite）分页读取。

---

## 4. 新增通知类型（对应 MA SSE Events）

> 现有 33+ 种 `AgentNotification` 已覆盖大部分 MA 事件。
> 以下 8 种是 MA 有但当前缺失的。

### 4.1 Session 状态机事件

| 通知类型 | JSON-RPC method | 对应 MA 事件 | 说明 |
|----------|----------------|-------------|------|
| `SessionStatusIdle` | `session.statusIdle` | `session.status_idle` | Agent 完成当前任务，等待输入 |
| `SessionStatusRunning` | `session.statusRunning` | `session.status_running` | Session 开始运行 |
| `SessionStatusTerminated` | `session.statusTerminated` | `session.status_terminated` | Session 终止（不可恢复） |
| `SessionStatusRescheduled` | `session.statusRescheduled` | `session.status_rescheduled` | 重试调度中 |

```rust
// 需要添加到 AgentNotification 枚举
SessionStatusIdle {
    session_id: String,
    stop_reason: Option<StopReason>,  // end_turn / requires_action / retries_exhausted
},
SessionStatusRunning { session_id: String },
SessionStatusTerminated { session_id: String, reason: String },
SessionStatusRescheduled { session_id: String },
```

### 4.2 Custom Tool 事件

| 通知类型 | JSON-RPC method | 对应 MA 事件 | 说明 |
|----------|----------------|-------------|------|
| `CustomToolUse` | `agent.customToolUse` | `agent.custom_tool_use` | 请求 host 侧执行工具 |
| `CustomToolResult`（入站请求） | `tool.customResult` | `user.custom_tool_result` | Client 回传执行结果 |

```rust
CustomToolUse {
    id: String,
    tool_name: String,
    input: Value,
},
```

对应的新入站方法 `tool.customResult`：
```json
{
  "jsonrpc": "2.0",
  "id": 5,
  "method": "tool.customResult",
  "params": {
    "tool_use_id": "sevt_abc123",
    "content": [{ "type": "text", "text": "result" }],
    "is_error": false
  }
}
```

### 4.3 上下文压缩事件

| 通知类型 | JSON-RPC method | 对应 MA 事件 | 说明 |
|----------|----------------|-------------|------|
| `ThreadContextCompacted` | `agent.contextCompacted` | `agent.thread_context_compacted` | 对话历史被压缩 |

已有 `CompactStart` / `CompactComplete`，但缺 MA 格式的确认通知：
```rust
ThreadContextCompacted {
    session_id: String,
    pre_compaction_tokens: u64,
},
```

### 4.4 Span 事件（可选）

| 通知类型 | JSON-RPC method | 对应 MA 事件 | 说明 |
|----------|----------------|-------------|------|
| `ModelRequestStart` | `span.modelRequestStart` | `span.model_request_start` | 模型推理开始 |
| `ModelRequestEnd` | `span.modelRequestEnd` | `span.model_request_end` | 模型推理结束，含 usage |

```rust
ModelRequestEnd {
    input_tokens: u64,
    output_tokens: u64,
    cache_read_tokens: u64,
    cache_creation_tokens: u64,
},
```

> 这与现有的 `TurnComplete` 中的 `UsageInfo` 高度重复，仅在与 MA 事件格式严格对齐时需要。

---

## 5. MA 事件 → Clawed Notification 完整映射

| MA Event Type | Clawed JSON-RPC Method | 已有？ |
|--------------|----------------------|--------|
| `agent.message` | `agent.textDelta` | 已有 |
| `agent.thinking` | `agent.thinkingDelta` | 已有 |
| `agent.tool_use` | `agent.toolStart` | 已有 |
| `agent.tool_result` | `agent.toolComplete` | 已有 |
| `agent.mcp_tool_use` | `agent.toolStart` (tool_name 区分) | 已有 |
| `agent.mcp_tool_result` | `agent.toolComplete` | 已有 |
| `agent.custom_tool_use` | `agent.customToolUse` | **新增** |
| `agent.thread_context_compacted` | `agent.contextCompacted` | **新增** |
| `session.status_idle` | `session.statusIdle` | **新增** |
| `session.status_running` | `session.statusRunning` | **新增** |
| `session.status_rescheduled` | `session.statusRescheduled` | **新增** |
| `session.status_terminated` | `session.statusTerminated` | **新增** |
| `session.error` | `agent.error` | 已有 |
| `span.model_request_start` | `span.modelRequestStart` | 可选 |
| `span.model_request_end` | `span.modelRequestEnd` | 可选 |
| `user.message` (echo) | — | 不需要（client 知道自己发了什么） |
| `user.interrupt` (echo) | — | 不需要 |
| `user.tool_confirmation` (echo) | — | 不需要 |
| `user.custom_tool_result` (echo) | — | 不需要 |

**结论：19 种 MA 事件中，11 种已有对应，4 种需新增，4 种不需要（client echo）。**

---

## 6. MA 交互模式 → RPC 示例

### 6.1 标准 Agent → Session 流程

```
// MA: POST /v1/agents → 创建 Agent
{"jsonrpc":"2.0","id":1,"method":"agent.create","params":{"name":"Coder","model":"claude-sonnet-4-6","system":"..."}}
← {"jsonrpc":"2.0","id":1,"result":{"id":"agent_abc","version":1}}

// MA: POST /v1/sessions → 启动 Session
{"jsonrpc":"2.0","id":2,"method":"session.create","params":{"agent":"agent_abc"}}
← {"jsonrpc":"2.0","id":2,"result":{"session_id":"sess_123","status":"running"}}

// MA: SSE stream → JSON-RPC Notification（天然推送，不需要连 stream 端点）
← {"jsonrpc":"2.0","method":"session.statusRunning","params":{"session_id":"sess_123"}}
← {"jsonrpc":"2.0","method":"agent.textDelta","params":{"text":"Hello"}}
← {"jsonrpc":"2.0","method":"session.statusIdle","params":{"session_id":"sess_123","stop_reason":{"type":"end_turn"}}}

// MA: POST /v1/sessions/{id}/events → 已有 agent.submit
{"jsonrpc":"2.0","id":3,"method":"agent.submit","params":{"text":"Add tests"}}
← {"jsonrpc":"2.0","id":3,"result":{"ok":true}}
```

### 6.2 Tool Confirmation 流程

```
// Agent 触发权限请求
← {"jsonrpc":"2.0","method":"agent.permissionRequest","params":{"request_id":"pr_001","tool_name":"Bash","input":{"command":"rm -rf /"},"risk_level":"high","description":"Delete files"}}

// Client 批准（对应 MA 的 user.tool_confirmation）
{"jsonrpc":"2.0","id":4,"method":"agent.permission","params":{"request_id":"pr_001","granted":true,"remember":false}}
← {"jsonrpc":"2.0","id":4,"result":{"ok":true}}
```

### 6.3 Custom Tool 流程

```
// Agent 调用 custom tool
← {"jsonrpc":"2.0","method":"agent.customToolUse","params":{"id":"sevt_abc","tool_name":"linear_graphql","input":{"query":"{...}"}}}

// Client 在 host 侧执行（凭证不外泄），回传结果
{"jsonrpc":"2.0","id":5,"method":"tool.customResult","params":{"tool_use_id":"sevt_abc","content":[{"type":"text","text":"{...}"}],"is_error":false}}
← {"jsonrpc":"2.0","id":5,"result":{"ok":true}}
```

### 6.4 事件历史回放（对应 MA 的 stream reconnection）

```
// 断连后拉取历史
{"jsonrpc":"2.0","id":6,"method":"session.listEvents","params":{"session_id":"sess_123","limit":1000}}
← {"jsonrpc":"2.0","id":6,"result":{"events":[{"id":"sevt_001","type":"agent.message",...}],"has_more":false}}

// 同时 Notification 仍在推送新事件（同一个 TCP 连接）
← {"jsonrpc":"2.0","method":"agent.textDelta","params":{"text":"more output"}}
```

> MA 需要 stream + events.list 双通道去重。
> clawed-code 在同一个连接上：listEvents 拉历史，Notification 推实时事件。天然不丢。

### 6.5 Interrupt 流程

```
// 已有方法，直接对应 MA 的 user.interrupt
{"jsonrpc":"2.0","id":7,"method":"agent.abort","params":{}}
← {"jsonrpc":"2.0","id":7,"result":{"ok":true}}

// Session 进入 idle
← {"jsonrpc":"2.0","method":"session.statusIdle","params":{"session_id":"sess_123","stop_reason":{"type":"end_turn"}}}
```

---

## 7. 新增 AgentRequest 枚举项

需要在 `clawed-bus/src/events.rs` 的 `AgentRequest` 枚举中新增：

```rust
// ── Managed Agents 对齐 ──

/// Create a new Agent configuration.
CreateAgent {
    name: String,
    model: String,
    system: Option<String>,
    tools: Option<Vec<Value>>,
    mcp_servers: Option<Vec<Value>>,
    skills: Option<Vec<Value>>,
    description: Option<String>,
},

/// List Agent configurations.
ListAgents {
    limit: Option<u32>,
    after_id: Option<String>,
},

/// Get a single Agent by ID.
GetAgent { agent_id: String },

/// Update an Agent (creates new version).
UpdateAgent {
    agent_id: String,
    name: Option<String>,
    model: Option<String>,
    system: Option<String>,
    tools: Option<Vec<Value>>,
    mcp_servers: Option<Vec<Value>>,
    skills: Option<Vec<Value>>,
    description: Option<String>,
},

/// Archive an Agent.
ArchiveAgent { agent_id: String },

/// List Agent versions.
ListAgentVersions { agent_id: String },

/// Create a new Session.
CreateSession {
    agent: String,          // agent_id or {type, id, version}
    environment_id: Option<String>,
    title: Option<String>,
    resources: Option<Vec<Value>>,
    vault_ids: Option<Vec<String>>,
},

/// List Sessions.
ListSessions {
    limit: Option<u32>,
    after_id: Option<String>,
},

/// Get Session details.
GetSession { session_id: String },

/// Update Session metadata (title only).
UpdateSession { session_id: String, title: Option<String> },

/// Delete a Session.
DeleteSession { session_id: String },

/// Archive a Session.
ArchiveSession { session_id: String },

/// Paginated event history.
ListSessionEvents {
    session_id: String,
    limit: Option<u32>,
    after_id: Option<String>,
},

/// Custom tool result from client.
CustomToolResult {
    tool_use_id: String,
    content: Vec<Value>,
    is_error: bool,
},
```

---

## 8. 新增 JSON-RPC 方法路由

需要在 `clawed-rpc/src/methods.rs` 的 `parse_request` 中新增：

| method 字符串 | AgentRequest 变体 |
|--------------|-------------------|
| `agent.create` | `CreateAgent` |
| `agent.list` | `ListAgents` |
| `agent.get` | `GetAgent` |
| `agent.update` | `UpdateAgent` |
| `agent.archive` | `ArchiveAgent` |
| `agent.listVersions` | `ListAgentVersions` |
| `session.create` | `CreateSession` |
| `session.list` | `ListSessions` |
| `session.get` | `GetSession` |
| `session.update` | `UpdateSession` |
| `session.delete` | `DeleteSession` |
| `session.archive` | `ArchiveSession` |
| `session.listEvents` | `ListSessionEvents` |
| `tool.customResult` | `CustomToolResult` |

同时更新 `METHODS` 常量数组。

---

## 9. 新增 Notification → JSON-RPC 映射

在 `notification_to_jsonrpc` 中新增：

| AgentNotification 变体 | JSON-RPC method |
|----------------------|-----------------|
| `SessionStatusIdle` | `session.statusIdle` |
| `SessionStatusRunning` | `session.statusRunning` |
| `SessionStatusTerminated` | `session.statusTerminated` |
| `SessionStatusRescheduled` | `session.statusRescheduled` |
| `CustomToolUse` | `agent.customToolUse` |
| `ThreadContextCompacted` | `agent.contextCompacted` |
| `ModelRequestStart` | `span.modelRequestStart` |
| `ModelRequestEnd` | `span.modelRequestEnd` |

---

## 10. 对比总结

| 维度 | MA REST + SSE | Clawed JSON-RPC |
|------|--------------|-----------------|
| **总端点数** | 51 个 HTTP 端点 | 41 个 RPC 方法（17 已有 + 14 新增 + 10 可选 CRUD） |
| **连接数** | 2（REST + SSE） | 1（TCP/stdio 长连接） |
| **事件推送** | SSE stream（需重连去重） | Notification（天然推送，无需重连） |
| **事件历史** | `GET /events` 分页 | `session.listEvents` |
| **权限确认** | idle + user.tool_confirmation | agent.permissionRequest + agent.permission |
| **Custom Tool** | idle + user.custom_tool_result | agent.customToolUse + tool.customResult |
| **中断** | user.interrupt（跳队列） | agent.abort |
| **版本控制** | 自动不可变版本 | 追加式版本（实现层面决定） |

**新增工作量：**
- `AgentRequest` 枚举：+14 个变体
- `AgentNotification` 枚举：+8 个变体
- `parse_request` 路由：+14 个 match arm
- `notification_to_jsonrpc`：+8 个 match arm
- 业务逻辑层：AgentDefinition 版本化 + Session 状态机 + 事件持久化

**不涉及：**
- 新增 HTTP server
- 新增传输层
- 修改已有方法
- SSE handler
- 重连去重逻辑（单连接不需要）
