# ADR 0004：内部使用点号工具名，Provider 边界使用双下划线

Status: Accepted
Date: 2026-09-09

## Context

Yukinal 的工具按命名空间组织，例如 `docker.ps` 和 `filesystem.read`。不少 function-calling 网关对名称允许的字符和长度有更严格的限制，点号可能被拒绝或被静默改写；改写后的名称如果进入内部日志或审计，会造成无法可靠追踪。

## Decision

- 内部唯一名称使用点号：ToolRegistry、Permission Engine、trace、SQLite 审计、IPC 和 UI 都使用 `docker.ps`。
- 发给 Provider 的名称使用双下划线：`docker__ps`。
- `createProviderNameIndex` 负责生成映射，收到模型工具调用后先反向映射，再交给 ToolRegistry；无法映射时视为未知工具，不猜测。
- 注册阶段检查字符、长度和映射冲突。内部名称的段不使用下划线，因此点号与双下划线映射保持可逆。

## Consequences

- 日志、审计和界面显示稳定的业务名称，Provider 的命名限制不会改变内部契约。
- MCP 或其他外部工具未来加入时，必须先经过命名空间化和冲突检查。
- 映射层增加了少量复杂度，但集中在 `packages/provider-sdk`，避免在 loop 或每个工具中复制规则。
