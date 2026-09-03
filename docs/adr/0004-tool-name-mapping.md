# 0004 — Tool 内部用点号命名，LLM 边界用双下划线

Status: accepted (2026-09)

## Context
的命名是 `docker.ps`。多数 OpenAI-compatible 网关的 function name 约束是
`^[a-zA-Z0-9_-]{1,64}$`：点号会被拒绝或静默改写；改写后的名字回到我们手里就对不上注册表。

## Decision
- **内部（唯一真相）**：`docker.ps` —— registry key、trace、audit、DB、IPC、Tool 实现全部用它。
- **模型边界**：`docker__ps` —— 只允许在 `packages/provider-sdk` 的 `createProviderNameIndex` 内产生。
- 反向映射：收到 `tool_call` 时先 `internalFor()`；解析不出即报错给模型，绝不猜测。
- 长度/字符合法性、注册期冲突检测在 `@yukinal/shared/naming/tool-name.ts`。

## Consequences
- (+) 审计/日志/UI 永远看到规范里的名字，文档与代码一致。
- (+) MCP 带来的外来名字（`server:tool`、含空格）在注册期被拒或强制命名空间化，而不是运行时炸。
- (−) 一次映射成本；(−) 模型可能在输出里拼 `docker__ps`，需要在 prompt 里明确"名字由系统提供"。
- 注：内部段禁止下划线，因此 `.`→`__` 映射在数学上是单射；冲突检测仍保留为对 MCP/未来扩展的防线。
