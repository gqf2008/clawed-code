# TUI 1:1 复刻验证清单

对照官方 Claude Code 逐项验证 clawed TUI 的视觉输出。

## 验证环境

```bash
# 启动 clawed TUI
cargo run --

# 在另一个终端启动官方 CC（用于对比）
claude
```

---

## 1. PromptInput（输入框）

| 检查项 | 官方 CC | clawed | 验证方式 |
|--------|---------|--------|----------|
| 底部边框 | `╰──────────────╯` 底部圆角 | `render_input_separator` 生成 `╰...╯` | 观察输入框下方 |
| 输入前缀 | `❯` (U+276F) cyan bold | `❯` cyan bold | 观察输入行开头 |
| Model badge | `claude-3.5 ·` | 同格式 | 观察输入行左侧 |
| Placeholder | dim gray | dim gray | 清空输入观察 |
| Suggestion overlay | 输入框上方，▔ divider | 同 | 输入 `/` 触发补全 |
| Hint bar 左 | `Esc help  Tab complete ...` | 同 | 观察底部 hints |
| Hint bar 右 | `✓ bypass on` / `⚠ dontAsk on` | 同 | 观察右下角 mode |

---

## 2. Messages（消息列表）

| 检查项 | 官方 CC | clawed | 验证方式 |
|--------|---------|--------|----------|
| 用户消息前缀 | `❯` cyan bold | `❯` cyan bold | 发送任意消息 |
| Assistant 前缀 | `⏺` dim | `⏺` dim | 等待回复 |
| Thinking block | `∴ Thinking…` + Ctrl+O expand | `∴ Thinking…` + Ctrl+O | 使用 `--thinking` |
| Tool output | `├─`/`└─` tree + bullet | tree connectors | 触发 tool use |
| 消息间距 | 空一行分隔 | 空一行 | 观察多条消息 |
| Welcome screen | 标题 cyan bold + 版本 dim | 同 | 启动空会话 |

---

## 3. Spinner（动态状态）

| 检查项 | 官方 CC | clawed | 验证方式 |
|--------|---------|--------|----------|
| Spinner 字符 | `·✢✳✶✻✽` | `·✢✳✶✻✽` | 观察生成时的字符 |
| Interval | 120ms | 120ms | 肉眼观察流畅度 |
| Mode glyph | `↑` requesting, `↓` responding | 同 | 观察箭头方向 |
| Shimmer | 3-char highlight window 扫过 verb | 同 | 观察 verb 高亮 |
| Shimmer speed | 50ms (requesting) / 200ms (thinking) | 同 | 观察高亮移动速度 |
| Stall | 3s 后渐变为 ERROR_RED | 3s 后渐变为 rgb(171,43,63) | 断网或慢 API 测试 |
| Token counter | `~N` 平滑递增 | `~N` 平滑递增 | 观察长回复 |
| Thought for Ns | `thought for Ns` 3s delay | 同 | 使用 `--thinking` |
| Teammate count | `N teammate(s)` | 同 | 多 agent 场景 |

---

## 4. StatusLine（状态栏）

| 检查项 | 官方 CC | clawed | 验证方式 |
|--------|---------|--------|----------|
| 默认显示 | 隐藏（无配置时） | 显示内置 model/turn/tokens | 无 `statusLine` 配置 |
| External command | 执行 `settings.json` `statusLine.command` | 同 | 配置 `{"statusLine":{"command":"echo test"}}` |
| JSON context | 传入 stdin | 传入 stdin | 命令中 `cat > /tmp/debug.json` |
| 输出样式 | dimmed, truncate | dimmed, truncate | 观察过长输出 |

---

## 5. Layout（布局）

| 检查项 | 官方 CC | clawed | 验证方式 |
|--------|---------|--------|----------|
| 消息区域 | flexGrow=1 | `Constraint::Min(1)` | 调整终端高度 |
| 底部区域 | flexShrink=0 | `Constraint::Length(N)` | 同 |
| New messages pill | 底部居中 "N new messages" | 底部居中 pill | 滚动上去后接收新消息 |
| Sticky Header | 顶部固定用户 prompt | 顶部固定用户 prompt | 向上滚动查看历史 |

---

## 6. Scroll（滚动）

| 检查项 | 官方 CC | clawed | 验证方式 |
|--------|---------|--------|----------|
| 滚动键 | Shift+↑ / Shift+↓ / PageUp / PageDown | 同 | 按键测试 |
| Auto scroll | 新消息自动到底部 | 同 | 发送消息后观察 |
| Scroll offset | 记录向上滚动距离 | 同 | 滚动后观察 |

---

## 已知差异（非阻塞）

| 差异点 | 原因 | 是否修复 |
|--------|------|----------|
| Virtual scroll | 当前缓存机制已足够，极端场景未优化 | 否 |
| ANSI 颜色保留 | 官方 CC 本身用 `dimColor` 覆盖 ANSI | 否（当前行为已 1:1） |
| Mouse 支持 | 鼠标滚轮滚动已实现（ScrollUp/ScrollDown） | 是 |
| 虚拟滚动 | ratatui 限制，当前简单 offset 已足够 | 否 |
