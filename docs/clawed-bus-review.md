# clawed-bus Crate 深度评审

> 评审日期：2026-04-13
> 评审范围：`crates/clawed-bus/` 全部源码

## 架构概览

`clawed-bus` 是 Agent Core 与 UI 层之间的解耦消息总线，提供两类通信通道：

- **广播通道 (broadcast)**：`AgentNotification`、`PermissionRequest` —— 1:N 发布订阅
- **点对点通道 (mpsc)**：`AgentRequest`、`PermissionResponse` —— N:1 / 1:1 带背压

总线工厂 `EventBus::new()` 产出一对 `BusHandle`（Agent Core 持有）和 `ClientHandle`（UI 持有），通过 tokio channels 连接。设计清晰，职责分离得当。

## 模块结构

| 模块 | 行数 | 大小 | 职责 |
|------|------|------|------|
| `lib.rs` | 49 | 1.8KB | 模块导出、顶层文档、架构图 |
| `bus.rs` | 707 | 25.2KB | `EventBus` 工厂、`BusHandle`、`ClientHandle`、`SendError`、15 个测试 |
| `events.rs` | 695 | 21.4KB | 所有事件类型定义（`AgentNotification` 35 变体、`AgentRequest` 20 变体、`PermissionRequest/Response`、辅助类型）、8 个测试 |
| **合计** | **1,451** | **48.4KB** | |

## 优点

1. **清晰的拓扑设计** — `bus.rs:9-14` 的 ASCII 文档精确描述了 4 种通道的方向性和语义（broadcast vs mpsc, 1:N vs N:1）。
2. **Core alive watch 机制** — `bus.rs:50` 的 `watch::channel(true)` 配合 `Drop`（`bus.rs:96-99`）使客户端能可靠检测 Core 断连。
3. **二次客户端防伪造** — `bus.rs:240-249` 中 `new_client()` 创建的二级客户端 `perm_resp_tx: None`，阻止了未授权客户端响应权限请求。
4. **Lagged 消息优雅处理** — `bus.rs:293-313` 和 `bus.rs:327-347` 中 `recv_notification` / `recv_permission_request` 使用 `tokio::select!` 同时监听消息和 core_alive，且在 Lagged 时自动跳过继续。
5. **权限请求超时保护** — `bus.rs:197` 使用 `tokio::time::timeout` 避免非交互式客户端永久阻塞。
6. **全面的事件类型覆盖** — `events.rs` 涵盖流式内容、工具生命周期、会话生命周期、子代理、MCP、Swarm 等完整场景，且都支持 serde 序列化。
7. **测试覆盖良好** — 23 个测试全部通过，涵盖基本流、权限超时、多订阅者、断连检测等高价值场景。

## 问题与隐患

### P0 — 严重

**P0-1: 权限响应竞态条件 — 非预期响应被吞没**
- 文件：`bus.rs:184-195`
- 问题：`request_permission_with_timeout` 中的等待循环遍历所有 `perm_resp_rx.recv()` 响应，对不匹配的 `request_id` 仅打印 warn（`bus.rs:189-192`）然后继续。**如果两个并发权限请求同时发出**，响应 A 可能在等待请求 B 的循环中被收到并被丢弃（因为 `request_id` 不匹配），导致请求 B 永远等不到自己的响应直到超时。
- 根因：单个共享的 `mpsc::Receiver<PermissionResponse>` 无法区分不同请求的响应。
- 影响：多工具并行调用权限检查时，响应可能错位丢失。

**P0-2: 广播通道不保证消息可达性**
- 文件：`bus.rs:46`（notifications），`bus.rs:48`（permission requests）
- 问题：`PermissionRequest` 使用 `broadcast::channel`（`bus.rs:48`），当接收者落后时消息会被跳过。虽然对 notification 来说是合理的（`bus.rs:11` 标注 "lossy"），但对 permission request 来说丢失消息是致命的 — UI 客户端可能错过权限弹窗，导致用户永远看不到请求。
- 建议：permission request 应该使用 mpsc 或 request/response 配对机制，而非 broadcast。

### P1 — 重要

**P1-1: `try_recv_notification` 返回 `Result<..., ()>` 反模式**
- 文件：`bus.rs:394-402`
- 问题：Clippy 已警告（`result_unit_err`），`Result<Option<T>, ()>` 的 `Err(())` 语义不清晰。调用者无法区分"落后跳过消息"和"其他错误"。
- 当前 Clippy 警告：
  ```
  warning: this returns a `Result<_, ()>`
  ```
- 修复：定义一个专用 `TryRecvError` 枚举，或返回 `Option<Result<AgentNotification, LaggedError>>`。

**P1-2: `EventBus` 是单元结构体，`new` 方法不符合惯例**
- 文件：`bus.rs:25,36-37`
- 问题：`EventBus` 是空结构体（unit struct），`new` 不返回 `Self`（`clippy::new_ret_no_self` 被显式 `#[allow]` 压制）。这表明 `EventBus` 实际上是一个命名空间，而非类型。
- 建议：直接暴露 `fn new_bus(capacity: usize) -> (BusHandle, ClientHandle)` 作为自由函数，或改名为 `BusFactory`。

**P1-3: `PermissionResponse` 在 `AgentRequest` 中冗余定义**
- 文件：`events.rs:255-261`（`AgentRequest::PermissionResponse`） vs `bus.rs:185-187`（直接通过 mpsc 发送 `PermissionResponse`）
- 问题：存在两条权限响应路径：`AgentRequest::PermissionResponse`（走 request mpsc）和 `PermissionResponse` struct（走 perm_resp mpsc）。两条路径语义重叠但代码不互通。`AgentRequest::PermissionResponse` 变体在实际使用中是否与 `perm_resp_tx` 通道有关联？如果 UI 通过 `submit(AgentRequest::PermissionResponse {...})` 发送响应，`BusHandle::request_permission` 收不到，因为它监听的是 `perm_resp_rx` 而非 `request_rx`。
- 影响：两种机制并存容易误用，开发者可能选了错误的路径。

**P1-4: `send_request` 使用 `try_send` 而非 `send`**
- 文件：`bus.rs:316-320`
- 问题：`send_request` 使用 `try_send`，当 channel 满时直接返回 `DISCONNECTED` 错误。但这与 channel 实际是否断开无关 — 满队列也会返回同样的错误，误导调用者认为连接已断开。
- 建议：区分 `Full` 和 `Disconnected` 错误，或改用异步 `send().await`。

**P1-5: 无 `Send + Sync` trait bounds**
- 文件：`bus.rs:83-93`, `bus.rs:272-284`
- 问题：`BusHandle` 和 `ClientHandle` 未显式标注 `impl Send` / `impl Sync`。虽然 tokio 的 channel 类型默认是 `Send` 的，这些 struct 也应该自动实现 `Send`，但缺少显式 trait bound 意味着如果未来添加非 `Send` 字段会静默失去跨线程发送能力。建议添加 `static_assertions::assert_impl_all!` 或显式 `impl Send` 来保证。

### P2 — 建议改进

**P2-1: `ImageAttachment` 缺少 `Default` 实现**
- 文件：`events.rs:374-380`
- 问题：`submit` 方法中 images 硬编码为 `vec![]`（`bus.rs:363`），而 `ImageAttachment` 没有 `Default`，无法用 `..Default::default()` 语法。虽然当前影响不大，但 `UsageInfo` 有 `Default`（`events.rs:363`）而 `ImageAttachment` 没有，不一致。

**P2-2: `context_usage_pct` 精度未验证**
- 文件：`events.rs:89`
- 问题：`SessionStatus::context_usage_pct` 使用 `f64`，但没有任何文档说明取值范围（0.0-1.0？0-100？）。UI 侧消费时需要猜测。

**P2-3: `RiskLevel` 缺少 Critical 级别**
- 文件：`events.rs:406-412`
- 问题：当前只有 Low/Medium/High，对于 `rm -rf /` 这类操作没有更高警示级别。`bus.rs:669` 测试中用 `RiskLevel::High` 标记 `rm -rf /`，但 "High" 不足以表达灾难性风险。

**P2-4: 测试中 `drop(_bus)` 命名误导**
- 文件：`bus.rs:558-569`
- 问题：变量名为 `_bus`（下划线前缀在 Rust 中表示"有意忽略"），但测试中确实使用了它。应命名为 `bus`。

**P2-5: `ErrorCode::InternalError` 文档暗示 panic**
- 文件：`events.rs:428`
- 问题：`/// Internal error (bug, panic, etc.)` — panic 不应该作为"可恢复错误"通过事件总线传播。如果系统到了 panic 状态，event bus 通常已经不可用了。

**P2-6: `AgentNotification` 混合了事件和响应**
- 文件：`events.rs:17-233`
- 问题：`AgentNotification` 枚举同时包含纯事件（`TextDelta`、`ToolUseStart`）和请求响应（`SessionStatus`、`ModelList`、`ToolList`、`McpServerList`）。这两种语义不同的消息混在一个类型中，消费者需要 exhaustive match 处理所有变体，即使只关心其中几种。
- 建议：拆分为 `AgentEvent`（纯推送）和 `AgentQueryResponse`（请求响应）。

### P3 — 小问题

**P3-1: `submit` 和 `submit_with_images` 存在代码重复**
- 文件：`bus.rs:360-377`
- 问题：`submit` 应内部调用 `submit_with_images` 而非反过来。当前 `submit` 构建 `AgentRequest::Submit` 与 `submit_with_images` 重复。

**P3-2: `serde(default)` 不一致**
- 文件：`events.rs`
- `AgentRequest::Submit.images` 有 `#[serde(default)]`（line 247）
- `AgentRequest::McpConnect.env` 有 `#[serde(default)]`（line 291）
- 但 `PermissionResponse.remember` 有 `#[serde(default)]`（line 260）而 `UsageInfo.cache_read_tokens` 也有（line 368）
- `ImageAttachment` 和 `McpServerInfo` 等 struct 的字段没有 `#[serde(default)]`，如果 JSON 缺少字段则反序列化失败。作为"protocol layer"，应对缺失字段更宽容。

**P3-3: `lib.rs` 的 doc test 被忽略**
- 文件：`lib.rs:31-43`
- 问题：`cargo test` 显示 `test crates/clawed-bus/src/lib.rs - (line 31) ... ignored`。被标记为 `ignore` 的 doc test 永远不会运行，容易过时。

**P3-4: 缺少 `clap` / CLI 集成**
- 文件：`Cargo.toml`
- 问题：`test-utils` feature 只暴露了 `subscribe_requests` 测试工具（`bus.rs:258-263`），但测试工具本身在 `any(test, feature = "test-utils")` 条件下可用，实际 `test-utils` feature 没有启用任何额外依赖。这个 feature 的存在价值不明确。

**P3-5: 硬编码魔法数字**
- 文件：`bus.rs:40-44`
- `REQUEST_QUEUE_CAP: 1024`、`PERM_RESP_QUEUE_CAP: 256`、`MIN_CRITICAL_CAP: 256` 作为局部常量定义在 `new()` 内部。应该提升到 struct 级别或文档中说明选择这些数字的依据。

**P3-6: `SendError` 实现 `thiserror::Error` 但未使用 `thiserror` 的优势**
- 文件：`bus.rs:426-433`
- 问题：`SendError` 手动实现了 `DISCONNECTED` 常量等价物，但 `thiserror` 的 derive 完全足够了。当前写法 `#[error("...")]` 配手动 `const DISCONNECTED` 略显冗余。

## 代码质量评估

| 维度 | 评分 | 说明 |
|------|------|------|
| 错误处理 | 6/10 | `SendError` 无法区分 Full vs Disconnected；`Result<_, ()>` 反模式（P1-1）；权限响应被静默吞没（P0-1） |
| 异步设计 | 7/10 | `tokio::select!` 正确使用，超时保护到位；但广播 channel 用于 permission request 不合理（P0-2） |
| 测试覆盖 | 8/10 | 23 个测试覆盖主要路径，但缺少并发权限请求测试、多客户端竞争测试、channel 满时行为测试 |
| 命名 | 8/10 | 整体命名清晰；`EventBus` 是单元结构体不妥（P1-2）；`_bus` 变量名误导（P2-4） |
| 文档 | 7/10 | 顶层架构图优秀；但 `context_usage_pct` 等字段缺少范围说明（P2-2）；doc test 被忽略（P3-3） |
| 模块组织 | 6/10 | `events.rs` 695 行承载 35+20 个枚举变体 + 6 个 struct + 3 个 enum + Display impls，过于膨胀；事件与响应混在同一枚举（P2-6） |
| 安全性 | 6/10 | 二次客户端防伪造设计优秀；但权限响应通道共享导致可能的请求/响应错位（P0-1）；权限请求走 broadcast 可能丢失（P0-2） |
| 序列化 | 8/10 | 内部标记枚举（`#[serde(tag = "type")]` 和 `#[serde(tag = "method", content = "params")]`）适合 JSON-RPC；但部分字段缺少 `#[serde(default)]`（P3-2） |

## 修复建议汇总

| 优先级 | 问题 | 修复方案 | 预估工作量 |
|--------|------|----------|------------|
| P0 | P0-1: 权限响应竞态 | 为每个权限请求创建独立的 oneshot channel，将 `Sender` 放入 `PermissionRequest` 随广播发出，UI 直接通过 oneshot 回应 | 中 |
| P0 | P0-2: permission request 走 broadcast | 将 `perm_req_tx/perm_req_rx` 改为 mpsc，或至少在 broadcast 之外增加 guaranteed delivery 机制 | 中 |
| P1 | P1-1: `Result<..., ()>` | 定义 `pub enum TryRecvError { Lagged(u64), Closed }` | 小 |
| P1 | P1-2: `EventBus` 单元结构体 | 改为自由函数 `fn new_bus(...)` 或重命名为 `BusFactory` | 小 |
| P1 | P1-3: 双重权限响应路径 | 明确 `AgentRequest::PermissionResponse` 的用途，或移除其中一条路径 | 中 |
| P1 | P1-4: `try_send` 误报断开 | 返回区分 `Full` / `Disconnected` 的错误类型 | 小 |
| P1 | P1-5: 缺少 `Send` 保障 | 添加 `const _: () = { fn _assert_send<T: Send>() {} fn _() { _assert_send::<BusHandle>(); _assert_send::<ClientHandle>(); } };` | 小 |
| P2 | P2-1: `ImageAttachment` 缺 Default | 添加 `#[derive(Default)]` 或手动实现 | 小 |
| P2 | P2-3: `RiskLevel` 缺 Critical | 添加 `Critical` 变体（向后兼容，需更新 Display） | 小 |
| P2 | P2-6: 事件与响应混杂 | 拆分为 `AgentEvent` 和 `AgentQueryResponse` 两个枚举 | 大 |
| P3 | P3-1: `submit` 重复代码 | 让 `submit` 调用 `self.submit_with_images(text, vec![])` | 小 |
| P3 | P3-5: 魔法数字 | 提升为 `const` 关联常量或 `static`，添加注释说明选择理由 | 小 |
