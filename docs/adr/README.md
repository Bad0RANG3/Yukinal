# Architecture Decision Records

只记录"未来会反复被质疑"的决定。每条 ADR 应当独立成立——不需要任何额外背景就能读懂。

| ADR | 决定 | 状态 |
| --- | --- | --- |
| [0001](0001-agent-runtime-as-node-sidecar.md) | Agent Runtime = 独立 Node.js 进程（sidecar） | Accepted |
| [0002](0002-ssh-backend-russh.md) | SSH backend = russh | Accepted |
| [0003](0003-openai-compatible-only-for-mvp.md) | MVP 只实现 OpenAI-compatible provider | Accepted |
| [0004](0004-tool-name-mapping.md) | 内部点号命名，LLM 边界双下划线 | Accepted |
| [0005](0005-permission-engine-sole-decision-maker.md) | 三层风险事实，Permission Engine 唯一决策 | Accepted |
| [0006](0006-sidecar-transport-ndjson-jsonrpc.md) | sidecar 传输 = stdio 上的 NDJSON JSON-RPC | Accepted |
| [0007](0007-monorepo-and-day-one-abstractions.md) | 第一天建 monorepo + 钉死 7 个抽象 | Accepted |
| [0008](0008-project-name-yukinal.md) | 项目名 **Yukinal**（轭：意图与执行力同向） | Accepted |
| [0009](0009-sidecar-launch-and-lifecycle.md) | 只有 Rust 启动 sidecar；入口解析顺序与生命周期规则 | Accepted |

新增 ADR 的门槛：该决定会影响 ≥2 个模块，或者将来有人很可能想重新讨论。
