# Agent 工作规范

## Commit 规范

每次提交代码时，commit message 必须包含：

- **会话 ID**：当前交互会话的唯一标识
- **Agent 名称**：执行该改动的 agent 标识（如 `Kimi Code CLI`、`OMC-Planner` 等）

这样可以在代码审查时快速追溯到对应的会话上下文，方便定位设计决策和讨论记录。

### 示例格式

```
fix: 修复 UTF-8 截断 panic

Session: 20260418-xyz123
Agent: Kimi Code CLI
```

或行内简写：

```
fix(history): pop_last_turn 正确处理 tool result [Session: 20260418-xyz123, Agent: Kimi Code CLI]
```
