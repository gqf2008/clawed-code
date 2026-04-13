# clawed-bridge Crate 深度评审

> 评审日期：2026-04-13
> 评审范围：`crates/clawed-bridge/` 全部源码

## 架构概览

clawed-bridge 是 Clawed Code 的外部消息通道网关层，负责将飞书、Telegram、企业微信、钉钉等平台的消息桥接到 Agent 的事件总线（Event Bus）。

核心数据流：
```
平台 → Webhook/Polling → ChannelAdapter → GatewayContext → ChannelGateway
  → SessionRouter → AgentRequest → Bus → Agent
  ← AgentNotification ← Bus ← Agent
  ← MessageFormatter ← OutboundMessage ← ChannelAdapter → 平台
```

设计亮点：
- `ChannelAdapter` trait 提供统一接口，各平台独立实现
- `SessionRouter` 按 channel 映射 Agent 会话，自动管理生命周期
- `MessageFormatter` 将细粒度通知聚合成完整消息
- `GatewayContext` 为适配器提供路由回调，避免循环依赖

## 模块结构

| 模块 | 行数 | 大小 | 职责 |
|------|------|------|------|
| `lib.rs` | 33 | 小 | 模块声明、公共 re-exports |
| `message.rs` | 267 | 中 | 平台无关的 InboundMessage/OutboundMessage 类型定义 |
| `config.rs` | 218 | 中 | BridgeConfig 及各平台配置结构、环境变量加载 |
| `adapter.rs` | 97 | 小 | ChannelAdapter trait 定义、AdapterError 枚举 |
| `gateway.rs` | 396 | 大 | 网关核心：适配器生命周期管理、消息路由、通知消费循环 |
| `session.rs` | 247 | 中 | SessionRouter：channel → Agent session 映射 |
| `formatter.rs` | 252 | 中 | MessageFormatter：AgentNotification → OutboundMessage 聚合 |
| `webhook.rs` | 248 | 中 | axum HTTP 服务器，通用 webhook 接收端点 |
| `adapters/mod.rs` | 7 | 极小 | 平台适配器子模块声明 |
| `adapters/feishu.rs` | 253 | 中 | 飞书适配器实现（token 管理、消息发送、事件解析） |
| `adapters/telegram.rs` | 0 | - | **空文件，未实现** |
| `Cargo.toml` | 27 | 极小 | 依赖声明 |

**总代码量：~1720 行（不含空行和注释）**

## 优点

1. **清晰的职责分离** — `ChannelAdapter` trait、`SessionRouter`、`MessageFormatter`、`WebhookServer` 各司其职，耦合度低。

2. **良好的异步设计** — 使用 `async_trait`、`tokio::sync` 原语，gateway 的 `run()` 方法采用 mpsc channel 解耦消息接收与路由。

3. **Double-check locking for token caching** — `FeishuAdapter::ensure_token()` (feishu.rs:55-89) 实现了标准的双检锁模式，read lock 快速路径 + write lock 慢路径，避免并发获取 token。

4. **测试覆盖较好** — 每个模块都有 `#[cfg(test)]` 测试，包括 serde roundtrip、错误显示、webhook 端点测试、session 生命周期测试。

5. **`AdapterMap` 使用 `Arc<Box<dyn ChannelAdapter>>`** — gateway.rs:46，允许多个任务并发读取适配器（通过 `RwLock`），同时保持 trait object 的灵活性。

6. **消息类型设计合理** — `InboundMessage` 和 `OutboundMessage` 覆盖 text、attachments、code blocks、tool results 等场景，并保留了 `raw` 字段供高级使用。

7. **`truncate()` 函数 UTF-8 安全** — formatter.rs:131-143，使用 `char_indices()` 找边界，正确处理 CJK/emoji 多字节字符。

8. **`/new` `/reset` 命令清理消费者任务** — gateway.rs:127-131，session 销毁时主动 abort 对应的 notification consumer，防止僵尸任务。

## 问题与隐患

### P0 — 安全与数据丢失风险

#### P0-1: 飞书 token 无过期刷新机制
**文件**: `adapters/feishu.rs:55-89`

`ensure_token()` 获取 token 后永久缓存，但飞书 tenant_access_token 有效期通常为 2 小时。token 过期后所有 API 调用会返回 401，而 `invalidate_token()` (feishu.rs:92-96) 是 `#[allow(dead_code)]` — **从未被调用**。

```rust
// feishu.rs:92-96
#[allow(dead_code)]
async fn invalidate_token(&self) {
    let mut guard = self.access_token.write().await;
    *guard = None;
}
```

**后果**: token 过期后服务持续失败，直到重启。

#### P0-2: 飞书 API 响应错误码处理不全面
**文件**: `adapters/feishu.rs:119-123`

```rust
if let Some(code) = body.get("code").and_then(|v| v.as_i64()) {
    if code != 0 {
        // ... error
    }
}
```

如果响应中缺少 `code` 字段（非标准响应），错误会被静默忽略，方法会返回空的 `message_id`。应该对缺少 `code` 字段的情况也做处理。

#### P0-3: Webhook 端点无任何认证/签名验证
**文件**: `webhook.rs:104-145`

`handle_webhook()` 仅检查平台名白名单，不验证请求签名（如飞书的 `X-Lark-Signature`、企业微信的 `msg_signature`）。任何人都可以向 `/webhook/feishu` 发送伪造消息。

`config.rs` 中定义了 `verification_token` 和 `encrypt_key` (feishu.rs:35-37)，但从未使用。

#### P0-4: `AdapterMap` 在 `start()` 中使用 `Arc::get_mut` 但后续只读访问不释放
**文件**: `gateway.rs:103-104`

```rust
let adapter_mut = Arc::get_mut(adapter)
    .ok_or_else(|| AdapterError::Internal(format!("adapter '{platform}' must not be shared yet")))?;
```

`Arc::get_mut` 要求 `Arc` 的引用计数为 1。如果任何其他位置 clone 了 `Arc`（目前代码中没有），`start()` 会失败。更关键的是，`send_message` (gateway.rs:187) 通过 `read().await` 获取 `&Box<dyn ChannelAdapter>`，这是共享引用，但 `ChannelAdapter::send_message` 签名是 `&self`，这没问题。然而 `stop()` (gateway.rs:252) 同样在 `read().await` 下调用 `&self` 方法，设计正确但 `start()` 的 `&mut self` 需求与 `Arc` 的不可变共享存在概念冲突。

### P1 — 功能缺陷与资源泄漏

#### P1-1: `run()` 只能调用一次
**文件**: `gateway.rs:110-111`

```rust
let mut inbound_rx = self.inbound_rx.take()
    .ok_or_else(|| AdapterError::Internal("Gateway can only be run once".into()))?;
```

`run()` 取走 `inbound_rx`，第二次调用必然失败。这不是一个 bug（设计上确实如此），但 `GatewayContext::route_inbound` 仍然可以发送消息到 `inbound_tx`，而这些消息会被永远丢弃（receiver 已被 take）。应该在 `run()` 返回后让 `route_inbound` 返回错误。

**建议**: 在 `run()` 完成后关闭 `inbound_tx`，让 `route_inbound` 的 `send()` 返回 `Err`。

#### P1-2: Notification consumer 的 idle timeout 硬编码
**文件**: `gateway.rs:168`

```rust
let idle_timeout = Duration::from_secs(600); // 10 min no-notification timeout
```

这个 600 秒硬编码在 spawn 的 async block 内部，与 `BridgeConfig.session_idle_timeout_secs`（用于 session cleanup）完全独立。用户无法配置通知超时。如果 Agent 响应很慢（例如处理大型代码库），600 秒可能不够。

#### P1-3: Consumer task 在 idle timeout 后不会重新创建
**文件**: `gateway.rs:154-156, 158`

```rust
// Clean up finished tasks
if tasks.get(&channel_id).is_some_and(|t| t.is_finished()) {
    tasks.remove(&channel_id);
}

if let std::collections::hash_map::Entry::Vacant(entry) = tasks.entry(channel_id.clone()) {
```

当 consumer task 因 idle timeout 退出后，如果同一 channel 的新消息到达，会创建新 task — 这部分是正确的。但存在一个竞态：如果消息在 task 退出但尚未被清理的窗口内到达，`tasks.get(&channel_id).is_some_and(|t| t.is_finished())` 检测为 finished 会 remove，然后 `Entry::Vacant` 会创建新的。这部分逻辑看起来正确。

但更深层的问题是：**如果 consumer task 因 broadcast channel lag 过大而丢失通知，它只是 warn 并 continue** (gateway.rs:195-197)，不会重置 formatter 状态。已累积的部分消息可能包含不完整内容。

#### P1-4: `InboundMessage.text` 在 gateway 中被 clone 后直接使用原始文本
**文件**: `gateway.rs:141-142`

```rust
if let Err(e) = client.send_request(AgentRequest::Submit {
    text: msg.text.clone(),
    images: vec![],
}) {
```

attachments 和 images 永远传 `vec![]`。`InboundMessage` 有 `attachments` 字段 (message.rs:78)，但 gateway 从不传递。这意味着 **图片/文件附件功能已定义但从未生效**。

#### P1-5: Telegram 适配器是空文件
**文件**: `adapters/telegram.rs` (0 行)

`adapters/mod.rs` 声明了 `pub mod telegram;`，但 `telegram.rs` 是空文件，编译会通过但没有任何实现。`config.rs` 中有 `TelegramConfig`、`BridgeConfig::from_env()` 加载 Telegram 配置，`enabled_platforms()` 也会报告 telegram — **配置与实现不同步**。

#### P1-6: `BridgeConfig::from_env()` 不加载 wechat 和 dingtalk
**文件**: `config.rs:89-129`

`from_env()` 只处理 `BRIDGE_WEBHOOK_ADDR`、`BRIDGE_SESSION_IDLE_TIMEOUT`、Feishu 和 Telegram 的环境变量。WeChat 和 DingTalk 配置结构已定义 (config.rs:56-79) 但 `from_env()` 中完全没有加载逻辑。

### P2 — 代码质量与设计问题

#### P2-1: `WechatConfig` 和 `DingtalkConfig` 没有 `Default` 实现
**文件**: `config.rs:56-79`

`BridgeConfig` 和 `FeishuConfig`/`TelegramConfig` 都有 `#[derive(Default)]` 或部分字段可为空，但 `WechatConfig` 和 `DingtalkConfig` 只 derive 了 `Debug, Clone, Serialize, Deserialize`，没有 `Default`。而 `BridgeConfig::default()` 中这些字段是 `None`，所以实际上不需要 `Default` — 但保持一致性更好。

#### P2-2: `SessionRouter` 使用 `Mutex` 而非 `RwLock`
**文件**: `gateway.rs:53`

```rust
router: Arc<Mutex<SessionRouter>>,
```

`get_or_create` 需要 `&mut self`，但 `session_count`、`has_session`、`get_client_subscriber` 都是只读操作。在 gateway 的 `run()` 中，`router.lock().await` 会序列化所有操作，包括只读的 `get_client_subscriber` (gateway.rs:159)。高并发场景下这是不必要的瓶颈。

#### P2-3: `MessageFormatter` 的 `thinking` 字段从未在输出中使用
**文件**: `formatter.rs:18, 44-47, 86-103`

`thinking` 字段累积了 `ThinkingDelta` 通知，但 `snapshot()` 和 `finish()` 生成的 `OutboundMessage` 都不包含 thinking 内容。这要么是遗漏（thinking 应该被包含或过滤），要么是冗余代码。

#### P2-4: `OutboundMessage.code_blocks` 从未被填充
**文件**: `formatter.rs:87-88, 98-99`

`MessageFormatter` 的 `code_blocks` 字段在 `new()` 中初始化为空 `vec![]`，且在 `push()` 中没有任何逻辑向其中添加内容。`OutboundMessage` 的 `code_blocks` 永远为空。

#### P2-5: `MessageFormatter` 没有处理 `TurnComplete` 中的 `stop_reason`
**文件**: `formatter.rs:70-73`

```rust
AgentNotification::TurnComplete { .. } => {
    self.is_streaming = false;
    true
}
```

`stop_reason`（如 "end_turn"、"tool_use"、"max_tokens"）被完全忽略。不同的 stop reason 可能意味着不同的行为（例如 `tool_use` 后可能还有更多工具调用）。

#### P2-6: Gateway `_config` 字段仅用下划线前缀标记未使用
**文件**: `gateway.rs:60`

```rust
_config: BridgeConfig,
```

`_config` 只在 `new()` 中用于提取 `session_idle_timeout_secs`，之后不再使用。应该提取需要的值后丢弃，而不是持有整个配置结构。

#### P2-7: `ChannelId::platform` 和 `ChannelId::channel` 是 `String` 而非 `Arc<str>`
**文件**: `message.rs:12-17`

`ChannelId` 被频繁 clone（在 `HashMap` key、`GatewayContext` 路由等场景），使用 `String` 会导致频繁的堆分配。考虑使用 `Arc<str>` 或 `Box<str>` 减少克隆开销。

#### P2-8: `FeishuAdapter::parse_event` 是公开静态方法但实际应该由 adapter 实例处理
**文件**: `feishu.rs:136-169`

`parse_event` 是 `pub` 的关联方法，不依赖 `self`。这意味着外部代码可以直接调用它绕过 adapter。作为 webhook 解析器，它应该由 webhook handler 调用，但当前 webhook handler (webhook.rs) 使用通用 JSON 格式，并未调用 `FeishuAdapter::parse_event`。

### P3 — 风格与小问题

#### P3-1: `emoji` 在 message 中使用
**文件**: `formatter.rs:75`

```rust
self.text.push_str(&format!("\n\n❌ Error: {}", message));
```

在系统消息中使用 emoji 可能在某些平台渲染异常。应使用纯文本格式或让平台适配器负责渲染。

#### P3-2: `from_file` 使用 `std::io::Error::other`
**文件**: `config.rs:134-135`

```rust
serde_json::from_str(&content)
    .map_err(|e| std::io::Error::other(e.to_string()))
```

`std::io::Error::other` 是 Rust 1.76+ 的 API。如果项目最低 Rust 版本低于 1.76，这会编译失败。

#### P3-3: 测试中使用了 `_client` 变量
**文件**: `gateway.rs:278, 302, 366` 等多处

```rust
let (bus, _client) = EventBus::new(64);
```

`_client` 前缀表示有意未使用，但这些测试可能应该验证 bus client 的正确行为。至少应该添加注释说明为什么不需要使用 client。

#### P3-4: `VALID_PLATFORMS` 常量与配置不同步
**文件**: `webhook.rs:86`

```rust
const VALID_PLATFORMS: &[&str] = &["feishu", "telegram", "wechat", "dingtalk"];
```

这个列表硬编码了平台名。如果未来新增平台，需要同步修改 `config.rs`、`enabled_platforms()`、`VALID_PLATFORMS` 三处。应该从配置或枚举中派生。

#### P3-5: `clawed-agent` 依赖未使用
**文件**: `Cargo.toml:8`

```toml
clawed-agent = { path = "../clawed-agent" }
```

grep 显示 `clawed-agent` 在 bridge crate 中没有被 `use`。这是一个未使用的依赖。

#### P3-6: `uuid` 依赖未使用
**文件**: `Cargo.toml:15`

`uuid` 在 bridge crate 源码中没有被导入或使用。`SessionRouter` 使用自增计数器 (`next_id`) 生成 session ID 而非 UUID。

#### P3-7: `tower` 的 timeout feature 已声明但未使用
**文件**: `Cargo.toml:21`

```toml
tower = { version = "0.5", features = ["timeout"] }
```

webhook server 的 router 没有配置任何 tower middleware（如 timeout layer），`timeout` feature 是多余的。

#### P3-8: `handle_command` 中 `/status` 命令只 log 不回复
**文件**: `gateway.rs:232-236`

```rust
"/status" => {
    let router = router.lock().await;
    let count = router.session_count();
    info!("Status request: {} active sessions", count);
    true
}
```

用户发送 `/status` 后只会在服务端日志看到结果，用户收不到任何回复。应该通过 adapter 发送一条消息给用户。

#### P3-9: `WebhookState.handlers` 和 `WebhookHandler` 全部 `#[allow(dead_code)]`
**文件**: `webhook.rs:24, 33-39`

`WebhookState.handlers` 初始化为空 vector 且从未修改。`WebhookHandler` 结构体定义后完全未使用。这是预留但未实现的功能。

#### P3-10: `InboundMessage::text` 构造函数中 `message_id` 和 `reply_to` 永远是 `None`
**文件**: `message.rs:89-103`

便捷构造函数 `InboundMessage::text()` 不设置 `message_id`，导致 gateway 无法追踪和回复特定消息（无法使用 `update_message` 编辑已发送消息）。

## 代码质量评估

| 维度 | 评分 | 说明 |
|------|------|------|
| 错误处理 | B- | `AdapterError` 分类合理，但 `FeishuAdapter` 不处理 token 过期、缺少 `code` 字段的 API 响应被静默忽略 |
| 异步设计 | B+ | 使用标准 tokio 原语，但 `Mutex<SessionRouter>` 序列化只读操作、consumer task 硬编码 timeout 可改进 |
| 测试覆盖 | B+ | 各模块均有测试覆盖核心路径，但缺少集成测试（完整消息流转）、缺少飞书 API mock 测试 |
| 命名 | A | 命名清晰一致：`ChannelAdapter`、`SessionRouter`、`MessageFormatter`、`ChannelGateway` 均达意 |
| 文档 | B | 模块级 doc 注释良好，但关键函数（如 `GatewayContext::route_inbound`、`MessageFormatter::push`）缺少 `# Errors` 和 `# Panics` 说明 |
| 模块组织 | B- | 职责分离清晰，但 `adapters/telegram.rs` 为空、`wechat`/`dingtalk` 配置已定义但无实现、死代码多 |
| 安全性 | C | 无 webhook 签名验证、无速率限制、配置中 `app_secret`/`bot_token` 直接存储（建议加密或从 secret manager 读取） |

## 修复建议汇总

| 优先级 | 问题 | 建议修复 | 涉及文件/行 |
|--------|------|----------|-------------|
| P0 | 飞书 token 无过期刷新 | 解析 API 响应中的 `expire` 字段，实现定时刷新或在 401 时调用 `invalidate_token()` | `adapters/feishu.rs:55-96` |
| P0 | Webhook 无签名验证 | 验证 `X-Lark-Signature` 等平台签名头，或使用 `verification_token` 验证 challenge 事件 | `webhook.rs:104-145` |
| P0 | 飞书 API 错误码缺失时被忽略 | 对响应中缺少 `code` 字段返回 `AdapterError::PlatformApi("unexpected response")` | `adapters/feishu.rs:119-123` |
| P1 | 图片/文件附件未传递 | 将 `InboundMessage.attachments` 转换为 `AgentRequest.images` 或新增 `files` 字段 | `gateway.rs:141-144` |
| P1 | Telegram 适配器空文件 | 实现或移除 `adapters/telegram.rs`，保持配置与实现一致 | `adapters/telegram.rs` |
| P1 | `from_env()` 缺少 wechat/dingtalk | 补充环境变量加载逻辑或明确标注为 TODO | `config.rs:89-129` |
| P1 | `run()` 后 `route_inbound` 消息被丢弃 | `run()` 完成后关闭 `inbound_tx`，让 `route_inbound` 返回有意义的错误 | `gateway.rs:110-111` |
| P1 | Consumer idle timeout 硬编码 | 将 600s 移入 `BridgeConfig` 作为可配置项 | `gateway.rs:168` |
| P2 | `thinking` 字段未输出 | 在 `OutboundMessage` 中添加 `thinking` 字段，或在 `push()` 中过滤掉 thinking 并移除该字段 | `formatter.rs:18, 44-47` |
| P2 | `code_blocks` 永远为空 | 在 `push()` 中解析 `TextDelta` 提取代码块，或移除该字段 | `formatter.rs:16, 87, 98` |
| P2 | `TurnComplete.stop_reason` 被忽略 | 根据 `stop_reason` 决定是否需要等待更多通知（如 `tool_use`） | `formatter.rs:70-73` |
| P2 | `_config` 持有未使用 | `ChannelGateway::new()` 只提取 `session_idle_timeout_secs`，不要存储整个 config | `gateway.rs:60` |
| P2 | `Mutex<SessionRouter>` 序列化只读操作 | 将 `SessionRouter` 内部改为 `RwLock`，只读方法用 `read()` | `gateway.rs:53` |
| P3 | 移除未使用依赖 | 删除 `Cargo.toml` 中的 `clawed-agent`、`uuid`，移除 `tower/timeout` feature | `Cargo.toml:8,15,21` |
| P3 | `/status` 命令应回复用户 | 通过 `GatewayContext` 或 adapter 发送一条包含 session 数量的消息 | `gateway.rs:232-236` |
| P3 | 清理 `webhook.rs` 死代码 | 移除 `WebhookState.handlers`、`WebhookHandler` 结构体及相关 `#[allow(dead_code)]` | `webhook.rs:24-39` |
| P3 | `VALID_PLATFORMS` 硬编码 | 从配置或枚举派生，或在 `config.rs` 中定义常量供 webhook 引用 | `webhook.rs:86` |
| P3 | `InboundMessage::text()` 不设置 `message_id` | 添加可选 `message_id` 参数或提供 `with_message_id` builder 方法 | `message.rs:89-103` |
