# 第一轮审计：结构、边界与安全

日期：2026-09-21。范围是当前工作树，包括已有未提交改动和本轮新增的模块拆分。该轮不连接真实服务器、不调用真实 Provider，也不使用虚拟机。

## 结论

项目的跨层边界总体清晰，且已有契约测试保护 React → Tauri → Rust → Agent 的主要接口。第一轮发现的主要问题不是缺少边界，而是几个边界被过多职责挤在同一个文件里，导致接口的深度和维护局部性下降。最严重的结构问题集中在 Tauri `commands` 层，而不是远端执行能力本身。

本轮已完成以下结构修复，行为由原有测试继续保护：

| 原区域 | 问题 | 新 seam |
| --- | --- | --- |
| `commands/mod.rs` | sidecar 生命周期、审计脱敏、调查事件栅栏和 IPC 基础类型混合，Divergent Change | `commands/sidecar.rs`、`commands/audit.rs`、`commands/event_projection.rs` |
| `commands/mcp.rs` | IPC 命令、凭据引用回收、传输校验和 UI 状态投影混合 | `commands/mcp/configuration.rs` |
| `commands/investigation.rs` | 命令编排、输入归一化、guardrail、状态转移和 Agent prompt 混合 | `commands/investigation/policy.rs` |
| `commands/mcp/oauth.rs` | token source、设备码/授权码流程、回调 HTTP 和 discovery 细节混合 | `commands/mcp/oauth/flow.rs`、`commands/mcp/oauth/callback.rs` |

## 依赖地图

```text
React features/stores
        │ callDesktop/listenDesktop（共享 schema 运行时解析）
        ▼
Tauri commands（薄适配器）
  ├─ sidecar：进程启动、事件转发、host request 分发
  ├─ audit：结果脱敏、活动/执行记录投影
  ├─ event_projection：任务/run 栅栏、状态转移、证据/步骤投影
  ├─ MCP configuration：编辑值 → credential refs → core transport
  ├─ investigation policy：输入边界、guardrail、任务策略、prompt
  └─ OAuth flow/callback：用户授权序列和 bounded loopback 回调
        │
        ├─ AppState：SQLite、credential store、supervisor、SSH/PTY/MCP
        ├─ crates/core：sidecar、MCP wire/HTTP、provider 和资源上限
        ├─ crates/database：迁移、models、repositories
        └─ apps/agent：ToolRegistry → PermissionEngine → host RPC
```

## 风险与代码异味

### P1：命令聚合文件的 Divergent Change（已修复）

证据：原 `commands/mod.rs` 同时包含 `start_sidecar`、`forward_sidecar_events`、`persist_agent_tool_result`、`sync_investigation_task_status`、`persist_investigation_event_state` 和大量状态/审计辅助函数。一次 sidecar 生命周期改动会与审计字段、任务状态机在同一个实现单元中变更，属于 Divergent Change，也造成 Shotgun Surgery 风险。

修复：将 transport、redaction、investigation projection 分到独立模块；root 只保留命令面、事件名、`EmptyResponse` 和受控 re-export。`cargo check -p yukinal-desktop` 与 `cargo clippy -p yukinal-desktop --all-targets -- -D warnings` 通过。

### P1：OAuth 回调和 token 逻辑共用过大的实现单元（已修复）

证据：原 `commands/mcp/oauth.rs` 同时处理 OAuth metadata、动态注册、PKCE、device-code polling、DPoP token exchange、刷新、loopback HTTP 解析和响应体上限。这里的风险不是“代码多”本身，而是网络输入上限和用户授权时序容易在另一条路径被漏掉。

修复：授权流程集中在 `oauth/flow.rs`，回环请求和统一 bounded body reader 在 `oauth/callback.rs`；token source 与 provider 安全策略留在 OAuth 根模块作为单一实现。

### P2：MCP 与调查命令的接口偏浅（已改善）

原命令文件直接暴露了大量内部字段和转换细节，调用者需要理解数据库模型、credential reference 和 core transport 的多组约束。现在 configuration/policy 模块承接这些不变量，命令层主要负责读取 IPC、调用深模块和返回响应。

### P2：前端全局样式是高耦合变化点（待第三轮 GUI 审计）

`apps/desktop/src/styles.css` 仍约 5,500 行，包含 tokens、rail、服务器列表、Agent、终端、设置、活动、调查和响应式规则。它是当前最大 UI 文件；第三轮将用固定数据和截图先确认真实视觉问题，再按页面 seam 拆分，避免只为降低行数机械移动 CSS。

### P2：Agent loop 仍承载多个运行时职责（待第二轮后处理）

`apps/agent/src/runtime/agent-loop.ts` 仍约 1,300 行，包含预算、取消、审批等待、provider streaming、tool execution、plan gate、observation replay 和结果投影。它已有 `PermissionEngine`、`ToolRegistry`、`TraceRecorder` 等深模块，但运行编排本身仍需要第二轮用压力/取消证据确认后再决定是否拆分，避免改变事件顺序。

## 已核对的安全边界

- React 只能经共享 IPC 白名单调用；事件和返回值有运行时 schema 解析。
- Agent 不持有 SSH、SQLite 或凭据资源；宿主会重新校验目标和 plan binding。
- MCP HTTP/ OAuth 使用无重定向客户端、出站代理和有界响应体；凭据通过 OS credential store 引用。
- sidecar、MCP、PTY、SSH 和 callback 都有关闭/取消路径，且本轮拆分未改变这些 owner。
- 审计对 secret、文件内容、输出长度和终态状态有单独规则；事件栅栏拒绝迟到 run。

## 本轮不做的事情

- 不把真实服务器、第三方 Provider 或第三方 MCP 的行为写成已验证事实。
- 不把所有 1,000 行以上的测试文件机械拆开；测试的场景连续性优先于文件行数。
- 不引入只有一个实现、没有变化需求支撑的 Adapter；当前拆分围绕已有职责和实际调用关系。
