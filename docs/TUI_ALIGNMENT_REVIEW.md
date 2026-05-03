# TUI 功能对齐审查报告

**审查日期**: 2026-05-04
**审查范围**: Clawed Code vs Claude Code Sourcemap（TUI 部分）
**审查方法**: 源码结构对比 + 功能点逐项检查

---

## 一、规模对比

| 维度 | Sourcemap (TypeScript/React/Ink) | Clawed Code (Rust/ratatui) |
|------|----------------------------------|---------------------------|
| TUI 核心代码 | ~37,500 行（components + ink + screens） | ~14,200 行（tui/ 目录） |
| 组件/模块数 | 168+ 个组件文件，81+ 子目录 | 13 个 Rust 模块 |
| 消息类型组件 | 34 个独立 .tsx 文件 | 1 个 `MessageContent` 枚举（5 变体） |
| 权限对话框 | 30 个独立 .tsx 文件 | 1 个 `permission.rs` 模块 |
| 输入系统 | 22 个 PromptInput 相关文件 | `input.rs` + `textarea.rs` |

---

## 二、已实现的功能（✓ 对齐）

| 功能 | Sourcemap 实现 | Clawed 实现 | 说明 |
|------|---------------|-------------|------|
| 全屏 TUI | Ink (React) | ratatui | 都使用双缓冲渲染 |
| 消息列表 | VirtualMessageList + ScrollBox | `messages.rs` + 手动滚动 | 支持折叠/展开 |
| 工具执行渲染 | 34 个消息组件 | `ToolExecution` 统一处理 | 含树形缩进、diff 着色 |
| 权限提示 | 30 个权限组件 | `permission.rs` | Allow/Deny/AllowAlways |
| 状态栏 | StatusLine.tsx (323 行) | `statusline.rs` (170 行) | 外部命令扩展 |
| 任务列表 | TaskListV2.tsx (377 行) | `tasklist.rs` (212 行) | 读取 `.claude_todos.json` |
| 输入框 | PromptInput 目录 (4658 行) | `input.rs` (904 行) | 多行、历史、补全 |
| 覆盖层/弹窗 | 多个 Dialog 组件 | `overlay.rs` | SelectionList + InfoPanel |
| Markdown 渲染 | 内置 | `markdown.rs` (pulldown-cmark) | 代码高亮 |
| 主题切换 | ThemePicker | `overlay.rs` build_theme_overlay | 8 种主题 |
| 图片粘贴 | 支持 | `mod.rs` Ctrl+V 读取剪贴板 | `ImageAttachment` |
| 鼠标支持 | 支持 | `mod.rs` 滚轮+点击展开 | crossterm MouseEvent |
| 队友视图 | TeammateViewHeader | `mod.rs` `view_teammate` | Swarm 集成 |
| 建议提示 | ContextSuggestions | `mod.rs` suggestions overlay | 文件/MCP/Agent 建议 |
| Vim 模式 | VimTextInput | `/vim` 命令（基础） | 标记为 work in progress |
| 底部栏 | 动态 hint | `bottombar.rs` | 生成中/空闲状态切换 |
| Doctor 检查 | Doctor.tsx | `build_doctor_overlay` | 14 项环境检查 |

---

## 三、缺失或简化的功能（✗ 未对齐）

| 功能 | Sourcemap 实现 | Clawed 状态 | 影响评估 |
|------|---------------|-------------|---------|
| **虚拟滚动** | `useVirtualScroll` + `ScrollBox` 虚拟化 | 无，全量渲染所有消息 | 大上下文时性能下降 |
| **消息搜索** | `/` 搜索 + `n/N` 跳转 + 高亮 | 无 | 长会话导航困难 |
| **Sticky Header** | 粘性提示头（ScrollChromeContext） | 无 | 滚动时丢失上下文 |
| **Cost 阈值对话框** | `CostThresholdDialog` | 无 | 预算控制缺失 |
| **Idle 返回对话框** | `IdleReturnDialog` | 无 | 空闲恢复提示缺失 |
| **MCP 工具 UI** | `MCPTool/UI.tsx` | 无专用 UI | MCP 工具结果渲染简化 |
| **Notebook 编辑 UI** | `NotebookEditTool/UI.tsx` | 无 | Notebook 操作无可视化 |
| **Web 工具 UI** | `WebFetchTool/UI.tsx`, `WebSearchTool/UI.tsx` | 无 | 网页结果无预览 |
| **文件编辑 Diff UI** | `FileEditToolDiff.tsx` + `StructuredDiff` | 仅文本 diff 着色 | 结构化 diff 缺失 |
| **Shell 进度显示** | `ShellProgress.tsx` + `ShellTimeDisplay` | 基础 duration 显示 | 实时进度条缺失 |
| **Companion/Buddy** | `CompanionSprite.tsx` | 无 | 吉祥物动画缺失 |
| **Bridge 集成** | Lark/Telegram/Slack UI | 无 TUI 集成 | `clawed-bridge` 存在但无 UI |
| **Voice 输入** | `VoiceIndicator.tsx` | 无 | 语音输入缺失 |
| **FPS 指标** | `useFpsMetrics` | 无 | 性能监控缺失 |
| **Telemetry/Analytics** | `logEvent`, Sentry | 无 | 遥测缺失 |
| **导出对话框** | `ExportDialog.tsx` | `/export` 命令行式 | 无交互式导出 |
| **Settings 对话框** | `Settings/` 目录 | `/config` InfoPanel | 配置界面简化 |
| **QuickOpen** | `QuickOpenDialog.tsx` | 无 | 快速打开文件缺失 |
| **GlobalSearch** | `GlobalSearchDialog.tsx` | 无 | 全局搜索对话框缺失 |
| **HistorySearch** | `HistorySearchDialog.tsx` | 无 | 历史搜索缺失 |
| **AutoUpdater** | `AutoUpdater.tsx` | 无 | 自动更新提示缺失 |
| **TrustDialog** | `TrustDialog/` | 无 | 信任确认缺失 |
| **Sandbox 权限** | `SandboxPermissionRequest.tsx` | 基础 `RiskLevel` | 沙箱权限细化缺失 |
| **技能改进调查** | `SkillImprovementSurvey` | 无 | 反馈收集缺失 |
| **Teleport** | `TeleportProgress.tsx` 等 | 无 | 远程会话切换缺失 |
| **多工作区** | `WorktreeExitDialog.tsx` | 无 | 工作区管理缺失 |
| **IDE 集成提示** | `IdeAutoConnectDialog.tsx` 等 | 无 | IDE 联动缺失 |
| **RateLimit 显示** | `RateLimitMessage.tsx` | 无 | 速率限制提示缺失 |
| **Token 警告** | `TokenWarning.tsx` | 无专用 UI | 上下文超限警告简化 |
| **Thinking 折叠** | `ThinkingToggle.tsx` | `Ctrl+O` 切换 | 功能对齐但 UI 简化 |
| **Message Actions** | `messageActions.tsx` | 无 | 消息操作菜单缺失 |
| **附件消息** | `AttachmentMessage.tsx` | 简化（图片计数） | 附件展示简化 |
| **UserPlanMessage** | `UserPlanMessage.tsx` | 无 | 计划消息无专用渲染 |
| **HookProgressMessage** | `HookProgressMessage.tsx` | 无 | Hook 进度无可视化 |
| **远程 Agent 任务** | `RemoteAgentTask.tsx` | 无 | 远程任务 UI 缺失 |
| **本地 Shell 任务** | `LocalShellTask.tsx` | 无 | Shell 任务无专用 UI |
| **InProcessTeammate** | `InProcessTeammateTask.tsx` | 基础队友视图 | 队友任务详情简化 |

---

## 四、架构差异

| 方面 | Sourcemap | Clawed |
|------|-----------|--------|
| 渲染框架 | React + Ink（自定义终端渲染器） | ratatui（纯 Rust TUI 库） |
| 布局引擎 | Yoga（Flexbox）+ 自定义几何 | ratatui Constraint 布局 |
| 状态管理 | React hooks + Context | 手动状态机（`App` struct） |
| 组件粒度 | 极细（168+ 组件） | 较粗（13 模块） |
| 事件循环 | React 渲染循环 | crossterm 事件轮询 |
| 屏幕模式 | Alternate Screen | **非 Alternate Screen**（兼容中文 IME） |

---

## 五、总体评估

### 已实现（约 60% 核心功能）

基础消息渲染、工具执行、权限提示、输入系统、状态栏、任务列表、覆盖层、主题、Markdown、图片粘贴、鼠标、Swarm 队友视图

### 缺失或简化（约 40%）

虚拟滚动、消息搜索、大量专用对话框（Cost/Idle/Trust/Export/Settings 等）、Bridge UI、Voice、Companion、结构化 Diff、Shell 进度、MCP/Notebook/Web 专用 UI、Telemetry、AutoUpdater、IDE 集成、Teleport、多工作区

### 关键差距

1. **性能**：无虚拟滚动，大上下文时全量重渲染
2. **可发现性**：无搜索、无 QuickOpen、无 GlobalSearch
3. **丰富度**：大量专用 UI 组件缺失，功能通过命令行/文本回退
4. **集成度**：Bridge、IDE、远程会话等外部集成无 TUI 入口

### 结论

这是一个**功能子集移植**，核心交互路径（聊天→工具→权限）已对齐，但周边功能（搜索、对话框、集成、可视化）大量简化或缺失。
