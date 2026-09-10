# ADR 0004 工具名称在 Provider 边界映射

Status: Accepted
Date: 2026-09-09

## Context

Yukinal 以内部分层命名工具，例如 `docker.ps` 与 `filesystem.read`。部分 function-calling 网关对名称中允许的字符或长度有更严格限制，点号可能被拒绝或静默改写。如果被改写的名称进入内部日志或审计，调用链将无法可靠追踪。

## Decision

- ToolRegistry、Permission Engine、trace、SQLite 审计、IPC 和 UI 始终使用带点号的内部名称，例如 `docker.ps`。
- 发给 Provider 的工具名称改为双下划线形式，例如 `docker__ps`。
- `createProviderNameIndex` 统一生成双向映射。模型调用先反向映射为内部名称，再交给 ToolRegistry；无法映射的名称视为未知工具，不作猜测。
- 注册阶段检查字符、长度与映射冲突。内部名称的各段不使用下划线，因此点号与双下划线之间的映射可逆。

## Consequences

- 日志、审计与 UI 统一显示稳定的业务名称，不受 Provider 命名限制影响。
- 未来接入 MCP 或其他外部工具时，必须先命名空间化并通过冲突检查。
- 映射增加了少量集中复杂度，但位于 `packages/provider-sdk`，避免在 Agent loop 或每个工具实现中复制规则。
