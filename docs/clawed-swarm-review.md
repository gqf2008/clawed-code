# clawed-swarm Crate 深度评审

> 评审日期：2026-04-13
> 评审范围：`crates/clawed-swarm/` 全部源码（14 个文件，~128KB）

## 架构概览

该 crate 基于 kameo actor 框架实现多代理集群网络。支持团队创建、代理生命周期管理、消息路由和广播，通过 MCP 协议集成到主 agent 引擎。

```
SwarmMcpServer（MCP 工具入口）
  └── SwarmNetwork（高级 API，RwLock<团队映射>）
        └── SwarmCoordinator（kameo actor，管理 agents HashMap）
              └── AgentActor（kameo actor，封装 SwarmSession → API 调用）
                    └── SwarmSession（轻量 agentic loop + ToolRegistry）
```

**依赖流向**：`server → network → {actors, session} → {api, tools, bus}`

### 模块结构

| 模块 | 行数 | 大小 | 职责 |
|------|------|------|------|
| `actors.rs` | 561 | 18.2KB | AgentActor + SwarmCoordinator 定义 + 消息处理 |
| `session.rs` | 325 | 12.1KB | 轻量 agentic loop（直接调用 clawed-api） |
| `network.rs` | 349 | 11.9KB | SwarmNetwork 高级 API（RwLock 团队管理） |
| `server.rs` | 391 | 15.1KB | MCP 服务器（8 个工具路由） |
| `conflict.rs` | ~270 | 8.9KB | 文件级冲突追踪 |
| `helpers.rs` | ~260 | 8.8KB | 团队文件 I/O、发现、颜色分配 |
| `types.rs` | 214 | 6.9KB | TeamFile/TeamMember/TeamContext 数据类型 |
| `team_create.rs` | ~160 | 5.2KB | TeamCreateTool 实现 |
| `bridge.rs` | ~150 | 5.0KB | Tool trait 适配器 |
| `bus_adapter.rs` | ~150 | 5.0KB | SwarmNotifier 事件通知 |
| `team_delete.rs` | ~180 | 6.7KB | TeamDeleteTool 实现 |
| `team_status.rs` | ~220 | 8.2KB | TeamStatusTool + 摘要格式化 |
| `messages.rs` | 163 | 4.7KB | 进程间通信消息类型 |
| `lib.rs` | 44 | 1.6KB | 模块导出 |

---

## 优点

### 1. 清晰的 Actor 模型应用（actors.rs）

- `AgentActor` 封装单个 AI agent 会话，持有 `SwarmSession` + `SwarmNotifier`
- `SwarmCoordinator` 管理 `HashMap<String, ActorRef<AgentActor>>`
- 消息类型定义清晰：`AgentQuery`/`AgentResponse`/`SpawnAgent`/`TerminateAgent`/`RouteMessage`/`BroadcastMessage`
- 使用 kameo 的 `#[derive(Actor)]` + `impl Message<T>` 模式，代码简洁

### 2. 循环依赖规避设计（session.rs:10-11）

```
clawed-swarm 不能依赖 clawed-agent（反向依赖会形成循环）
```

`SwarmSession` 直接调用 `clawed_api::client::ApiClient` + `clawed_tools::ToolRegistry`，重新实现了最小化的 agentic loop。这是一个有意识的架构决策，避免了循环依赖。

### 3. 完善的测试覆盖（actors.rs:368-561, network.rs:195-349）

- **actors.rs**：8 个测试，覆盖 agent 查询/状态、coordinator 创建/终止、广播、token 累计、模型覆盖、bus 事件
- **network.rs**：7 个测试，覆盖团队 CRUD、完整工作流、缺失团队错误、多团队隔离、并发创建
- **server.rs**：5 个测试，覆盖工具列表、团队创建、完整工作流、未知工具、缺失团队
- **types.rs**：6 个测试，覆盖名称净化、序列化/反序列化、生命周期

### 4. 并发安全设计（network.rs）

- `teams: Arc<RwLock<HashMap<...>>>` 保证多线程安全
- 读锁用于查询操作，写锁用于创建/删除
- 测试 `concurrent_spawns_in_same_team` 验证并发创建 5 个 agent 的正确性

### 5. 团队文件模型设计良好（types.rs）

- `TeamFile` 包含序列化/反序列化，支持 `skip_serializing_if` 减少冗余
- `TeamMember` 包含 `backend_type` 字段（"in-process"、"tmux"、"iterm2"），预留多后端扩展
- `TeamAllowedPath` 实现团队级共享路径权限
- `sanitize_name()` 正确处理文件名安全（字母数字保留、特殊字符转连字符、连续连字符折叠）

### 6. 事件通知系统（bus_adapter.rs）

- `SwarmNotifier` 通过 `tokio::sync::broadcast::channel` 发布事件
- 覆盖所有生命周期：`team_created`、`team_deleted`、`agent_spawned`、`agent_terminated`、`agent_query`、`agent_reply`

### 7. Token 累计追踪（actors.rs:128）

```rust
self.total_tokens += text.len() as u64 / 4;
```

虽然粗略（按字符数/4 估算），但对 swarm 场景的成本追踪足够。

---

## 问题与隐患

### P0 — 可能导致 panic 或数据丢失

#### 1. `session.rs` 消息转换将 `Message::System` 转为空 User 消息（session.rs:278-283）

```rust
Message::System(_) => ApiMessage {
    role: "user".to_string(),
    content: vec![],
},
```

System 消息被丢弃内容后转为空 User 消息，这可能违反 API 协议（API 不接受空的 user 消息）。

**修复建议**：过滤掉空消息，或将 System 内容合并到 system prompt 中。

#### 2. Token 估算不准确（actors.rs:128）

```rust
self.total_tokens += text.len() as u64 / 4;
```

这是按响应文本长度估算，忽略了输入 token、工具调用 token、API 返回的精确 token 计数。如果用于成本计算，可能导致显著偏差。

**修复建议**：使用 API 返回的 `usage` 字段，或至少标注为估算值。

#### 3. `SwarmSession::new` 返回 `Option<Self>` 隐藏错误（session.rs:55-83）

```rust
pub fn new(...) -> Option<Self> {
    let api_key = std::env::var("ANTHROPIC_API_KEY")
        .ok()
        .filter(|k| !k.is_empty())?;
    ...
    Some(Self { ... })
}
```

API key 缺失返回 `None`，但调用方（actors.rs:83）无法区分是 API key 问题还是其他初始化问题。错误信息延迟到运行时才暴露（actors.rs:119-122）。

**修复建议**：返回 `Result<Self, anyhow::Error>`，在创建时报告具体原因。

### P1 — 可能导致功能异常或 hang

#### 4. `SwarmSession::submit` 无超时机制（session.rs:88-258）

API 调用使用 `messages_stream`，但如果 API 响应缓慢或挂起，整个 agent actor 会被阻塞。kameo actor 模型中，一个阻塞的消息处理会阻止该 actor 处理后续消息。

**修复建议**：添加 `tokio::time::timeout` 包裹整个 submit 调用。

#### 5. 广播是顺序执行的（actors.rs:293-330）

```rust
for agent_id in &agent_ids {
    if let Some(agent_ref) = self.agents.get(agent_id) {
        match agent_ref.ask(query).await {
            ...
        }
    }
}
```

广播给 N 个 agent 需要等待 N 个 API 调用顺序完成。如果每个调用需要 10 秒，10 个 agent 需要 100 秒。

**修复建议**：使用 `futures::future::join_all` 并发执行。

#### 6. `delete_team` 仅 kill coordinator，不等待 agent 清理（network.rs:70-79）

```rust
coord_ref.kill();
```

`kill()` 是强制终止，agent actor 的 `Drop` 不会被调用。如果 agent 有未完成的工具操作或文件写入，可能丢失数据。

**修复建议**：发送终止消息等待清理完成，或使用 graceful shutdown。

#### 7. `team_create.rs`/`team_delete.rs` 等 Tool 实现未在审查范围内详细分析

这些模块依赖 `clawed-agent` 的 `QueryEngine`，如果 agent 引擎不可用，这些工具会静默失败。

#### 8. `history` 无上限，可能 OOM（session.rs:44）

```rust
history: Vec<Message>,
```

对话历史无限增长。`max_turns` 限制了循环次数，但每次 turn 的消息都累积。长对话可能导致内存耗尽。

**修复建议**：添加 `max_history_messages` 限制，实现滑动窗口裁剪。

### P2 — 性能优化

#### 9. `SwarmNetwork` 对每个操作都获取读锁（network.rs）

```rust
let teams = self.teams.read().await;
let coord = teams.get(team_name)...
```

每次操作都等待 RwLock 读锁。在高频操作场景下可能成为瓶颈。

**修复建议**：对于已知存在的 coordinator，缓存 ActorRef 引用。

#### 10. 消息转换中存在不必要的克隆（session.rs:194-206, 217-248）

每次 turn 中 `self.history.clone()` 传递给 `ToolContext`，而 history 可能很大。

**修复建议**：使用 `Arc<Vec<Message>>` 或引用传递。

#### 11. `agent_status` 查询遍历所有 agent（network.rs:161-173）

```rust
let team_status = coord.ask(GetTeamStatus).await...
team_status.agents.into_iter().find(|a| a.agent_id == agent_id)
```

查询单个 agent 状态需要获取所有 agent 的状态。

**修复建议**：在 `SwarmCoordinator` 中添加 `GetAgentStatus(agent_id)` 消息。

### P3 — 代码组织

#### 12. `SwarmMcpServer` 未实现 `BuiltinMcpServer` trait

当前 `SwarmMcpServer` 没有实现 `BuiltinMcpServer` trait（与 `ComputerUseMcpServer` 不同），这意味着它不能通过 MCP manager 自动注册。`call_tool` 是 `async fn`，而 `BuiltinMcpServer::call_tool` 要求同步。

**修复建议**：如果需要自动注册，要么适配为同步（`block_on`），要么扩展 trait 支持 async。

#### 13. `team_create.rs`/`team_delete.rs`/`team_status.rs` 与 `server.rs` 职责重叠

`server.rs` 有完整的工具定义和处理逻辑，而 `team_*.rs` 又实现了 `Tool` trait。两者功能重叠但不完全一致，容易造成维护负担。

**修复建议**：统一为一个工具注册路径，或明确两者职责边界。

#### 14. 缺少端到端集成测试

所有测试都是单元测试，缺少集成测试验证整个 swarm 链路的正确性（MCP → Network → Coordinator → Actor → Session → API）。

---

## 代码质量评估

| 维度 | 评分 | 说明 |
|------|------|------|
| 错误处理 | ⭐⭐⭐ | `Option` 返回隐藏错误原因，API 错误延迟到运行时 |
| 异步设计 | ⭐⭐⭐⭐ | RwLock 使用正确，但广播顺序执行 |
| 测试覆盖 | ⭐⭐⭐⭐ | 单元测试充分，缺少集成测试 |
| 命名 | ⭐⭐⭐⭐⭐ | 清晰、描述性的命名 |
| 文档 | ⭐⭐⭐ | 模块级文档好，公开 API 文档不完整 |
| 并发安全 | ⭐⭐⭐⭐ | RwLock 保护团队映射，但 kill 无 graceful shutdown |
| 架构设计 | ⭐⭐⭐⭐ | Actor 模型应用得当，循环依赖规避有意识 |

---

## 修复建议汇总

| 优先级 | 问题 | 位置 | 建议 |
|--------|------|------|------|
| P0 | System 消息转为空 User 消息 | session.rs:278-283 | 过滤或合并到 system prompt |
| P0 | Token 估算不准确 | actors.rs:128 | 使用 API 返回的 usage 字段 |
| P0 | new() 返回 Option 隐藏错误 | session.rs:55-83 | 改为 Result 返回具体原因 |
| P1 | submit 无超时机制 | session.rs:88-258 | 添加 tokio::time::timeout |
| P1 | 广播顺序执行 | actors.rs:293-330 | 使用 join_all 并发 |
| P1 | delete_team 强制 kill | network.rs:74 | 实现 graceful shutdown |
| P1 | history 无上限 | session.rs:44 | 添加 max_history_messages |
| P2 | 每个操作获取读锁 | network.rs | 缓存 ActorRef 引用 |
| P2 | history 克隆开销 | session.rs:217 | 使用 Arc 或引用 |
| P2 | agent_status 遍历所有 | network.rs:161-173 | 添加单 agent 查询消息 |
| P3 | team_*.rs 与 server.rs 重叠 | 多个文件 | 统一工具注册路径 |
| P3 | 缺少集成测试 | 测试 | 添加端到端测试 |
