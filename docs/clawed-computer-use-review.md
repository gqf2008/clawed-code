# clawed-computer-use Crate 深度评审

> 评审日期：2026-04-13
> 评审范围：`crates/clawed-computer-use/` 全部源码（5 个文件）

## 架构概览

该 crate 提供进程内 MCP 服务器，用于桌面自动化控制。核心能力包括截图、键鼠模拟、剪贴板访问、平台检测和会话互斥。

```
ComputerUseMcpServer（MCP 工具路由）
  ├── screenshot.rs ── 截图捕获（screenshots crate）
  ├── input.rs ─────── 键鼠模拟（enigo）
  ├── session_lock.rs ─ 文件锁互斥
  └── 内嵌剪贴板/平台检测（server.rs 尾部）
```

**依赖流向**：`server → {input, screenshot, session_lock} → enigo/screenshots`

### 模块结构

| 模块 | 行数 | 职责 |
|------|------|------|
| `server.rs` | 806 | MCP 服务器 + 11 个工具 + 剪贴板 + 平台检测 + 终端控制 |
| `input.rs` | 245 | 键鼠模拟（click, double_click, mouse_move, scroll, type_text, key_press） |
| `screenshot.rs` | 139 | 全屏/区域截图 + 显示器列表 |
| `session_lock.rs` | 146 | 文件锁防止并发桌面控制 |
| `lib.rs` | 30 | 模块导出 |

---

## 优点

### 1. 完善的跨平台剪贴板实现（server.rs:446-555）

- Windows：`powershell Get-Clipboard` / `Set-Clipboard`
- macOS：`pbpaste` / `pbcopy`
- Linux：三级降级链 `xclip → xsel → wl-paste`
- 正确处理 stdin 管道写入

### 2. 会话锁设计合理（session_lock.rs）

- 文件级锁，写入 PID 标识持有者
- 自动检测过期锁（通过检查进程存活 `is_process_alive`）
- `Drop` 实现自动释放，异常安全
- `acquire_at()` 支持测试路径注入

### 3. 跨平台平台检测（server.rs:559-654）

- OS 版本：Windows `cmd /C ver`、macOS `sw_vers`、Linux `/etc/os-release`
- 显示服务器：Windows `win32`、macOS `quartz`、Linux `wayland/x11` 环境变量检测
- 主机名：通过 `hostname` crate

### 4. 错误处理一致

- 所有操作返回 `anyhow::Result`
- MCP 工具错误返回 `McpToolResult { is_error: true }` 而非 panic
- 参数缺失返回有意义的错误信息

### 5. 输入模拟 API 简洁

- 键组合解析（`"ctrl+c"`, `"alt+tab"`）支持多种修饰符别名
- `parse_key` 覆盖 F1-F12、方向键、编辑键等常用键
- 滚动方向映射到 enigo 的 `Axis::Vertical/Horizontal`

---

## 问题与隐患

### P0 — 可能导致数据错误或安全风险

#### 1. 终端最小化使用固定延迟（server.rs:247-250）

```rust
minimize_terminal_window();
std::thread::sleep(std::time::Duration::from_millis(500));
```

500ms 在慢速系统上可能不够（窗口未最小化就截图），快速系统则白白等待。更严重的是 `minimize_terminal_window` 在 Linux 上获取"活动窗口"（server.rs:675-680），如果用户在截图前切换了焦点，会最小化错误的窗口。

**修复建议**：
- 使用进程 ID 查找窗口而非活动窗口
- 将固定延迟改为轮询检测窗口可见状态，设置最大超时

#### 2. 键鼠输入无坐标边界检查（input.rs:51-97）

所有坐标参数直接接受 `i32`，不检查是否在屏幕范围内。负坐标或超大坐标可能产生意外行为。

**修复建议**：使用 `list_displays()` 获取屏幕范围，添加边界验证。

#### 3. 会话锁 TOCTOU 竞态（session_lock.rs:42-58）

```rust
if path.exists() {
    // 检查旧锁 → 可能删除
}
// 写入我们的 PID
```

检查和写入之间存在时间窗口，两个进程可能同时通过检查。

**修复建议**：使用 `fs::File::create_new`（Rust 1.77+）或 `fdlock` crate 实现原子锁获取。

### P1 — 可能导致功能异常

#### 4. 每次操作都创建新的 Enigo 实例（input.rs）

每个函数（`click`、`mouse_move`、`type_text` 等）都调用 `create_enigo()` 创建新实例。Enigo 初始化涉及平台特定的系统调用（如 macOS 的 Accessibility API 请求），频繁操作有性能开销，且在 macOS 上可能触发重复的辅助功能权限弹窗。

**修复建议**：使用 `thread_local!` 存储或提供共享实例 API。

#### 5. `clipboard_write` Linux 路径忽略 `write_all` 错误（server.rs:538-539）

```rust
if let Some(ref mut stdin) = child.stdin {
    let _ = stdin.write_all(text.as_bytes());
}
if let Ok(status) = child.wait() {
```

写入失败被静默忽略，`child.wait()` 可能返回成功但剪贴板内容为空。

**修复建议**：传播 `write_all` 错误。

#### 6. `capture_screen` 不保证主显示器（screenshot.rs:14-18）

```rust
let screen = screens.into_iter().next()
    .ok_or_else(|| anyhow::anyhow!("No displays found"))?;
```

`next()` 不一定是主显示器。虽然有 `capture_region` 可用，但 `capture_screen()` 的行为在多显示器场景下不明确。

**修复建议**：优先选择 `display_info.is_primary` 为 `true` 的屏幕。

#### 7. `screenshot` 工具未验证 region 参数（server.rs:252-260）

```rust
let x = region["x"].as_i64().unwrap_or(0) as i32;
let y = region["y"].as_i64().unwrap_or(0) as i32;
let w = region["width"].as_u64().unwrap_or(100) as u32;
let h = region["height"].as_u64().unwrap_or(100) as u32;
```

如果用户提供负宽高或极大值，`unwrap_or` 不会捕获，直接传给底层 API。

### P2 — 性能优化

#### 8. 截图 base64 字符串占用大量内存（screenshot.rs:99）

`ScreenshotResult::base64_png` 使用 `String` 存储。4K 截图的 PNG 约 2-5MB，base64 后膨胀到 2.7-6.7MB。对于 MCP 返回是必要的，但内部可延迟编码。

#### 9. `server.rs` 过于庞大（806 行）

11 个工具定义 + 处理逻辑 + 剪贴板 + 平台检测 + 终端控制全部在一个文件中。

**修复建议**：拆分为 `server.rs`（路由）、`clipboard.rs`、`platform.rs`、`terminal.rs`。

### P3 — 代码组织

#### 10. 缺少公开 API 文档注释

`ScreenshotResult`、`DisplayInfo`、`PlatformInfo`、`MouseButton`、`ScrollDirection` 等公开类型缺少 `///` 文档注释。

#### 11. 键解析只到 F12（input.rs:188-199）

缺少 F13-F24 键的支持。虽然这些键很少见，但完整覆盖更好。

---

## 代码质量评估

| 维度 | 评分 | 说明 |
|------|------|------|
| 错误处理 | ⭐⭐⭐⭐ | 一致的 `anyhow::Result` + `is_error` 返回模式 |
| 跨平台 | ⭐⭐⭐⭐⭐ | 三平台全覆盖，Linux 多工具降级 |
| 测试覆盖 | ⭐⭐⭐⭐ | 核心路径有测试，实际输入操作因需显示跳过 |
| 命名 | ⭐⭐⭐⭐⭐ | 清晰、描述性的命名 |
| 文档 | ⭐⭐⭐ | 模块级文档好，公开类型缺少文档注释 |
| 安全性 | ⭐⭐⭐ | 无坐标校验、TOCTOU 竞态、固定延迟、终端窗口识别错误 |
| 性能 | ⭐⭐⭐ | 每次操作创建 Enigo 实例 |

---

## 修复建议汇总

| 优先级 | 问题 | 位置 | 建议 |
|--------|------|------|------|
| P0 | 终端最小化固定延迟 + Linux 窗口识别错误 | server.rs:247-250, 675-680 | 使用 PID 查找窗口 + 轮询检测 |
| P0 | 输入无坐标边界检查 | input.rs:51-97 | 添加坐标范围验证 |
| P0 | 会话锁 TOCTOU 竞态 | session_lock.rs:42-58 | 使用原子文件锁 |
| P1 | 频繁创建 Enigo 实例 | input.rs | 使用 thread_local 或共享实例 |
| P1 | clipboard_write 忽略写入错误 | server.rs:539 | 传播错误 |
| P1 | 多显示器主屏不明确 | screenshot.rs:17 | 优先 is_primary 屏幕 |
| P1 | region 参数未验证 | server.rs:252-260 | 添加范围检查 |
| P2 | server.rs 过大 | server.rs | 拆分为多个模块 |
| P3 | 缺少文档注释 | 多处 | 补充公开 API 文档 |
| P3 | 键解析只到 F12 | input.rs:188-199 | 补充 F13-F24 |
