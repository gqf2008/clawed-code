# clawed-rpc Crate 深度评审

> 评审日期：2026-04-13
> 评审范围：`crates/clawed-rpc/` 全部源码（9 个文件，2567 行）

## 架构概览

clawed-rpc 实现了一个 JSON-RPC 2.0 服务端，用于将 Agent Core 能力暴露给外部客户端（IDE/Web）。架构层次清晰：

```
Transport (trait) → StdioTransport / TcpTransport
                        ↓
                   RpcSession (per-connection state)
                        ↓
                   RpcServer (multi-transport orchestration)
                        ↓
                   EventBus ↔ Agent
```

协议层定义完整的 JSON-RPC 2.0 消息类型（Request/Response/Notification/RawMessage），方法层（methods.rs）负责 JSON-RPC 方法与内部 AgentRequest/AgentNotification 的双向路由映射。

## 模块结构

| 模块 | 文件 | 行数 | 职责 |
|------|------|-----:|------|
| `protocol` | `src/protocol.rs` | 468 | JSON-RPC 2.0 消息类型、序列化、错误码 |
| `session` | `src/session.rs` | 352 | 连接会话管理、pending request 跟踪、通知广播 |
| `server` | `src/server.rs` | 273 | 多传输层服务启停、客户端生命周期管理 |
| `transport/mod` | `src/transport/mod.rs` | 369 | Transport trait + TransportError + 工具类 + 测试 |
| `transport/tcp` | `src/transport/tcp.rs` | 188 | TCP 传输实现（TCPListener + TcpTransport） |
| `transport/stdio` | `src/transport/stdio.rs` | 492 | stdio 传输实现、JSON-RPC frame 编解码 |
| `methods` | `src/methods.rs` | 242 | 方法路由表、JSON-RPC ↔ AgentRequest 映射 |
| `error` | `src/error.rs` | 27 | RpcResult 类型别名 |
| `lib` | `src/lib.rs` | 41 | 公开 API 与重导出 |
| **测试** | `_test.rs` | 155 | `RpcServer::start_stdio` 集成测试 |

## 优点

1. **清晰的协议实现**：protocol.rs 完整实现了 JSON-RPC 2.0 规范，包含 Request/Response/Notification/Error 所有类型，序列化/反序列化正确
2. **良好的传输抽象**：`Transport` trait 设计合理，支持 stdio 和 TCP 两种传输方式，易于扩展 WebSocket
3. **Pending request 跟踪**：RpcSession 维护 `pending_requests` HashMap 用于响应匹配，防止响应丢失
4. **合理的错误处理**：RpcError/TransportError 分类清晰，支持带 data 字段和 JSON-RPC 错误码
5. **测试覆盖较好**：包含单元测试和集成测试，覆盖了协议序列化、方法路由、传输层、服务端启停等场景
6. **架构文档完善**：lib.rs 中有 ASCII 架构图，各模块文档注释清晰

## 问题与隐患

### P0 — 严重问题（必须修复）

#### 1. TcpTransport `read_line` 无上限内存分配风险

**文件**: `src/transport/tcp.rs:47`
```rust
let mut line = String::new();
let n = self.reader.read_line(&mut line).await?;
```

虽然第 51 行有 `MAX_LINE_SIZE` 检查，但 `read_line` 会一直读取直到遇到 `\n`。恶意客户端可以发送 100MB 无换行符的数据，导致 `String` 无限制增长，直到检查点才发现。应该在 `BufReader` 层面做限制，或使用 `read_until` + 大小限制的分块读取。

**修复建议**: 使用 `take(MAX_LINE_SIZE)` 包装 reader，或在 `read_line` 后立即检查长度并在超限前限制。

#### 2. RpcSession 内存泄漏 — pending_requests 永远不清理成功响应

**文件**: `src/session.rs:250-259`
```rust
pub async fn handle_response(&self, response: Response) {
    if let Some((id, tx)) = self.pending_requests.write().await.remove(&response.id) {
        let _ = tx.send(Ok(response));
    }
}
```

这里用 `response.id` 作为 key，但 `response.id` 类型是 `Option<RequestId>`，而 HashMap 的 key 是 `Option<RequestId>`。如果响应 ID 不匹配（None vs Some），请求将永远留在 map 中。更关键的是，`handle_response` 方法中 `response.id` 是 `Option<RequestId>` 类型，但 `self.pending_requests` 的 key 也是 `Option<RequestId>` — 这看起来正确，但需要确认 `RequestId` 实现了 `Eq + Hash`。

**实际验证**: `RequestId` 在 protocol.rs:215 定义了 `#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]`，所以 `Eq + Hash` 是有的。但需要确认 `remove` 的语义 — 如果 response.id 是 `Some(Number(1))` 而 pending key 也是 `Some(Number(1))`，这可以匹配。

**真正的泄漏风险**: 如果客户端发送一个请求后断开连接，对应的 pending entry 不会被清理。`RpcSession::run` 的 main loop 在 transport 关闭时退出，但如果还有其他并发 reader（比如 `forward_notifications`），这些 pending requests 不会被清理。

#### 3. StdioTransport 的 `frame_size` 字段溢出风险

**文件**: `src/transport/stdio.rs:234-235`
```rust
fn parse_header(&mut self, header: &[u8]) -> Result<Option<usize>, TransportError> {
    ...
    let size: usize = std::str::from_utf8(value)
        .map_err(|e| TransportError::Other(format!("Invalid Content-Length value: {}", e)))?
        .parse()
```

`usize` 在 64 位系统上是 64 位无符号整数。如果 Content-Length 值极大（比如 `99999999999999999999`），`parse()` 会返回错误，这没问题。但如果值刚好在 `usize::MAX` 附近，可能导致后续 `read_exact` 分配巨大内存。

**修复建议**: 添加最大帧大小检查（比如 100MB），与 TcpTransport 的 `MAX_LINE_SIZE` 保持一致。

### P1 — 高优先级问题

#### 4. `RpcServer::start_stdio` 中 joinset 的 join 错误被吞

**文件**: `src/server.rs:95-101`
```rust
while let Some(result) = self.joinset.join_next().await {
    match result {
        Ok(Ok(())) => { /* Task completed successfully */ }
        Ok(Err(e)) => {
            // Already logged in the task
        }
        Err(e) => {
            // Already logged in the task
        }
    }
}
```

所有错误分支都是空操作。如果 stdio 任务因为某些原因 panic 或失败，`RpcServer::run()` 会静默退出，不会传播错误。虽然注释说 "Already logged in the task"，但实际上在 `start_stdio` 的 spawn 闭包中只有 `tracing::info!` 记录 session 退出，没有 `tracing::error!` 记录异常。

**修复建议**: 至少对 `Err(e)` 分支（JoinError/panic）添加 `tracing::error!` 记录。

#### 5. `RpcServer::start_stdio` 中 session 在 joinset 外创建

**文件**: `src/server.rs:84-92`
```rust
let session_id = Uuid::new_v4();
let mut session = RpcSession::new(session_id, transport, self.event_bus.clone());
self.session_count += 1;
let initial_count = self.session_count;

self.joinset.spawn(async move {
    session.run().await;
    Ok::<_, RpcError>(())
});
```

`session_count` 在 spawn 前递增，但如果 spawn 失败（虽然 JoinSet::spawn 通常不会失败），计数会不一致。更重要的是，`self.session_count` 在 `spawn` 闭包外被修改，而 `run()` 方法没有维护这个计数 — 如果 session 异常退出，计数不会减少。

#### 6. StdioTransport 的 `parse_header` 不处理 `Content-Type` 以外的 header

**文件**: `src/transport/stdio.rs:206-244`
```rust
fn parse_header(&mut self, header: &[u8]) -> Result<Option<usize>, TransportError> {
    if header.is_empty() {
        return Ok(None); // Empty line signals end of headers
    }
    if let Some(content_type) = header.strip_prefix(b"Content-Type: ") {
        // ...
    }
    // 其他 header 被静默忽略
    Ok(Some(0)) // 返回 0 意味着继续读取
}
```

如果收到未知的 header（比如 `Content-Encoding: gzip`），它返回 `Ok(Some(0))`，这意味着 "继续读取更多 header"。但如果 `Content-Length` 在未知 header 之后，解析逻辑是正确的。然而，如果只收到未知 header 而没有 `Content-Length`，会导致 `Content-Length: missing` 错误。

**修复建议**: 对未知 header 添加 `tracing::warn!` 日志，方便调试。

#### 7. `RpcSession::handle_request` 中 `handle_notification` 的错误被吞

**文件**: `src/session.rs:210-213`
```rust
pub async fn handle_notification(&self, notification: Notification) {
    if let Err(e) = self.handle_notification_inner(notification).await {
        tracing::warn!("Error handling notification: {}", e);
    }
}
```

这里用 `warn` 级别是可以的，但对于某些关键通知（如 `initialize`、`shutdown`），可能需要 `error` 级别。

#### 8. `RpcSession::run` 中通知转发与请求处理并发竞态

**文件**: `src/session.rs:104-137`
```rust
// 两个 tokio::select! 分支：
msg_opt = transport.read_message() => { ... }
// vs
notification = bus_rx.recv() => { ... }
```

当 transport 消息和 bus 通知同时到达时，`select!` 随机选择一个。这可能导致通知延迟，但对于 JSON-RPC 这是可接受的。真正的问题是：如果 transport 关闭（返回 `None`），`select!` 的 `msg_opt` 分支会立即匹配并退出循环，此时 pending 的 bus 通知不会被发送。

**修复建议**: 在 transport 关闭后，继续发送剩余的 bus 通知再退出。

### P2 — 中等优先级问题

#### 9. `TransportError` 的 `From<std::io::Error>` 和 `From<serde_json::Error>` 实现信息损失

**文件**: `src/transport/mod.rs:65-83`
```rust
impl From<std::io::Error> for TransportError {
    fn from(err: std::io::Error) -> Self {
        TransportError::Io(err)
    }
}
impl From<serde_json::Error> for TransportError {
    fn from(err: serde_json::Error) -> Self {
        TransportError::Serialization(err)
    }
}
```

这些实现是正确的，但 `TransportError::Other(String)`  variant 直接存储错误消息，不包含原始错误。对于调试和监控，保留原始错误类型会更好。

**修复建议**: 考虑为 `Other` variant 添加 `source: Option<Box<dyn std::error::Error + Send + Sync>>` 字段，或为常见错误类型添加专用 variant。

#### 10. `RpcSession` 中 `session_id` 字段只用于日志

**文件**: `src/session.rs:27`
```rust
session_id: Uuid,
```

`session_id` 只在 `run()` 方法中用于日志输出。如果需要追踪客户端连接（比如统计活跃连接、调试问题），应该暴露 session 查询接口。

#### 11. `TcpListener` 没有 backlog 配置

**文件**: `src/transport/tcp.rs:86-88`
```rust
pub async fn bind(addr: &str) -> Result<Self, std::io::Error> {
    let inner = tokio::net::TcpListener::bind(addr).await?;
    Ok(Self { inner })
}
```

`tokio::net::TcpListener::bind` 使用默认 backlog（通常是 1024）。对于 daemon 模式，可能需要自定义 backlog 大小。

**修复建议**: 添加 `bind_with_backlog(addr, backlog)` 方法，或在文档中说明使用默认值。

#### 12. `methods.rs` 中 `map_request_to_method` 和 `map_notification_to_method` 的不完整映射

**文件**: `src/methods.rs:133-170`（`map_request_to_method`）和 `src/methods.rs:173-204`（`map_notification_to_method`）

`map_request_to_method` 只映射了 `Initialize` 和 `Shutdown`，其他请求类型返回 `"unknown"`. `map_notification_to_method` 也只映射了 `AgentTextDelta` 等少数类型。

虽然这在当前版本中可能是故意的设计（只暴露部分能力），但这意味着 `AgentRequest::Abort`、`AgentRequest::UserSubmit` 等请求无法通过 JSON-RPC 调用。

**修复建议**: 在文档中明确说明哪些方法被暴露，哪些是内部使用。

#### 13. `RawMessage` 反序列化不支持位置参数

**文件**: `src/protocol.rs:310-318`
```rust
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RequestParams {
    Object(serde_json::Map<String, serde_json::Value>),
    Array(Vec<serde_json::Value>),
}
```

JSON-RPC 2.0 规范允许 `params` 为 Structured 值（object 或 array）。`RawMessage` 使用 `Option<serde_json::Value>` 作为 params 字段（第 278 行），这支持任何 JSON 值，所以这不是问题。但 `RequestParams` 枚举在 `Request` 类型中使用，而 `RawMessage` 使用 `Value`，两者不一致。

### P3 — 低优先级问题

#### 14. `error.rs` 几乎没有实际内容

**文件**: `src/error.rs:1-27`

这个模块只包含一个类型别名 `RpcResult<T>` 和两个 trivial 测试。`RpcError` 实际定义在 `protocol.rs` 中。这个模块的存在增加了认知负担。

**修复建议**: 将 `RpcResult` 移到 `protocol.rs` 中，或让 `error.rs` 包含所有错误相关定义（包括 `RpcError`）。

#### 15. `StdioTransport` 的 `write_message` 不检查写入完整性

**文件**: `src/transport/stdio.rs:274-284`
```rust
async fn write_message(&mut self, msg: &RawMessage) -> Result<(), TransportError> {
    let json = serde_json::to_string(msg)?;
    let header = format!("Content-Length: {}\r\n\r\n", json.len());
    self.writer.write_all(header.as_bytes()).await?;
    self.writer.write_all(json.as_bytes()).await?;
    self.writer.flush().await?;
    Ok(())
}
```

`write_all` 确保所有字节被写入，这是正确的。但如果 stdout 被管道关闭（比如客户端退出），`write_all` 会返回 `BrokenPipe` 错误。这个错误会被 `TransportError::Io` 包装，但不会区分 "正常退出" 和 "异常断开"。

#### 16. 缺少 WebSocket 传输实现

文档注释中提到 "TCP/WebSocket"（lib.rs:14），但实际只实现了 stdio 和 TCP。

**修复建议**: 更新文档注释，或添加 WebSocket 传输实现。

#### 17. `TcpTransport` 的 `close` 方法只关闭写端

**文件**: `src/transport/tcp.rs:73-76`
```rust
async fn close(&mut self) -> Result<(), TransportError> {
    self.writer.shutdown().await?;
    Ok(())
}
```

只关闭写端（half-close），不关闭读端。这意味着 reader 仍然可以接收数据。对于优雅关闭这是合理的，但应该添加文档说明。

#### 18. `RpcServer` 没有 graceful shutdown 机制

**文件**: `src/server.rs:118-126`
```rust
pub async fn shutdown(&mut self) {
    tracing::info!("Shutting down RPC server ({} active sessions)", self.joinset.len());
    self.joinset.shutdown().await;
    tracing::info!("All RPC sessions have been shut down");
}
```

`JoinSet::shutdown()` 会取消所有正在运行的任务，但不会等待它们完成清理。如果需要优雅关闭（比如发送 JSON-RPC shutdown 通知给所有客户端），需要额外实现。

## 代码质量评估

| 维度 | 评分 (1-5) | 说明 |
|------|:----------:|------|
| **错误处理** | 3 | 错误类型设计合理，但多处错误被吞（P1-4, P1-7），缺乏错误传播机制 |
| **异步设计** | 4 | 正确使用 `tokio::select!`、`JoinSet`、`Mutex`，并发模型合理 |
| **测试覆盖** | 4 | 单元测试覆盖协议序列化、方法路由、传输层；集成测试覆盖服务端启停 |
| **命名** | 5 | 命名清晰一致，符合 Rust 惯例 |
| **文档** | 4 | 模块文档良好，但缺少方法级别的详细文档 |
| **模块组织** | 4 | 层次清晰，但 `error.rs` 过于简单 |
| **安全性** | 3 | 缺少输入大小限制（TCP）、无认证机制、无 TLS 支持 |

## 修复建议汇总

| # | 优先级 | 文件 | 行号 | 问题 | 建议修复 |
|---|:------:|------|:----:|------|----------|
| 1 | P0 | `transport/tcp.rs` | 47 | `read_line` 无上限内存分配 | 使用 `take(MAX_LINE_SIZE)` 包装 reader |
| 2 | P0 | `session.rs` | 250-259 | 断开连接后 pending_requests 泄漏 | 在 transport 关闭时清理所有 pending entries |
| 3 | P0 | `transport/stdio.rs` | 234-235 | 大 Content-Length 导致内存分配 | 添加最大帧大小检查（100MB） |
| 4 | P1 | `server.rs` | 95-101 | JoinError 被静默吞没 | 添加 `tracing::error!` 记录 panic |
| 5 | P1 | `server.rs` | 84-92 | session_count 不一致风险 | 在 session 退出时减少计数 |
| 6 | P1 | `transport/stdio.rs` | 206-244 | 未知 header 静默忽略 | 添加 `tracing::warn!` |
| 7 | P1 | `session.rs` | 104-137 | transport 关闭后 bus 通知丢失 | 在退出前清空 bus_rx |
| 8 | P2 | `transport/mod.rs` | 65-83 | TransportError::Other 丢失原始错误 | 添加 source 字段 |
| 9 | P2 | `methods.rs` | 133-204 | 方法映射不完整 | 文档说明暴露范围 |
| 10 | P2 | `transport/tcp.rs` | 86 | 无 backlog 配置 | 添加 `bind_with_backlog` |
| 11 | P3 | `error.rs` | 1-27 | 模块内容太少 | 合并到 protocol.rs 或充实内容 |
| 12 | P3 | `lib.rs` | 14 | 文档提到 WebSocket 但未实现 | 更新文档或添加实现 |
| 13 | P3 | `transport/tcp.rs` | 73-76 | close 只关闭写端 | 添加文档说明 half-close 语义 |
| 14 | P3 | `server.rs` | 118-126 | 无 graceful shutdown | 实现优雅关闭机制 |

## 总结

clawed-rpc 是一个结构良好的 JSON-RPC 2.0 实现，协议层设计正确，传输抽象合理。主要风险集中在：

1. **内存安全**：TCP 和 stdio 传输都缺少严格的输入大小限制，可能导致 OOM
2. **错误处理**：多处关键错误被静默吞没，不利于故障诊断
3. **生命周期管理**：session 和 pending request 的生命周期管理存在泄漏风险

建议在投入生产使用前优先修复 P0 级别问题，特别是 TCP 传输的内存限制和 pending request 清理。
