# Yukinal 审计与重构进度

> 这是本轮“三轮审计、模块化拆分、整体重构”的唯一进度记录。每完成一个阶段就更新一次，避免在长任务或上下文切换后重复工作、遗漏约束，或把未验证的能力误写成已完成。

## 1. 目标与验证边界

- 目标：对当前项目做三轮审计，拆分过大的文件，收敛模块接口并完成可在本机验证的重构。
- 当前环境：Windows 校园网；不使用虚拟机；没有真实远程服务器、真实 Provider 或第三方 MCP 作为验收依赖。
- 可做的验证：本地编译、静态检查、单元/集成测试、回环 fixture、临时本地进程、浏览器预览或 Tauri GUI、本地生成数据和截图。
- 不可宣称已完成的验证：真实服务器网络故障、真实安装后升级、第三方 Provider/MCP 差异、跨平台打包、长时间耐久运行。
- 安全原则：不回滚或覆盖工作树中已有的用户改动；不提交密钥、个人数据或真实服务器信息；不以截图替代自动化测试。

## 2. 基线快照

- 日期：2026-09-21（Asia/Shanghai）。
- 分支：`main`，`HEAD` 为 `5c551d7`。
- 工作树：开始时已有大量未提交改动，主要集中在宿主命令、MCP、调查、远程文件、Provider 和文档；这些改动被视为用户已有工作，后续只在其上增量修复。
- 已有审查文档：`docs/project-review-and-roadmap.md`、`docs/Yukinal-security-performance-audit.md`。
- 已知高复杂度区域：Tauri 命令层、MCP OAuth、调查任务、远程文件、Agent loop、共享 IPC 契约和桌面全局样式。
- 初始大文件快照（源代码与脚本，约 1000 行以上）：
  - `apps/desktop/src/styles.css`
  - `apps/desktop/src-tauri/src/commands/mcp/oauth.rs`
  - `apps/desktop/src-tauri/src/commands/host/plan.rs`
  - `apps/desktop/src-tauri/src/commands/investigation.rs`
  - `apps/desktop/src-tauri/src/commands/mod.rs`
  - `apps/desktop/src-tauri/src/commands/mcp.rs`
  - `apps/desktop/src-tauri/src/commands/host/tools.rs`
  - `apps/agent/src/runtime/agent-loop.ts`
  - `apps/desktop/src-tauri/src/commands/agent_run.rs`
  - `crates/core/src/mcp/http.rs`

## 3. 三轮审计计划与状态

### 第一轮：结构、边界与安全审计

- [x] 绘制当前 Rust、TypeScript、Tauri、Agent、数据库和共享契约的依赖地图。
- [x] 检查 IPC 输入校验、权限/授权、secret、进程生命周期、网络出口、文件写入和错误映射。
- [x] 按模块接口、深度、局部性和代码异味记录问题；区分事实、风险和推断。
- [x] 输出：[`audit-round-1.md`](./audit-round-1.md)。
- [x] 已落地第一批 seam：`commands/mod.rs` 拆为 sidecar/audit/event projection；MCP 配置投影、调查策略、OAuth flow/callback 均已拆出。

### 第二轮：构建、测试、性能与可恢复性审计

- [x] 执行 `pnpm check` 的文档/密钥/契约、TypeScript、单元测试、bundle 与 sidecar 门禁；并单独复跑 Rust format、clippy 和 workspace test。
- [x] 对本轮失败做最小复现：fixture 的 Vite 假设和样式测试入口均已修复。
- [x] 检查取消、超时、审批、脱敏事件投影、上限、任务恢复和 OAuth owner。
- [x] 使用本地 fixture/回环进程覆盖无法接入真实服务器的 UI 路径。
- [x] 输出：[验证矩阵、失败证据和性能结论](./audit-round-2.md)。

### 第三轮：GUI 数据流、视觉与交互审计

- [x] 只使用本地 Vite preview，不连接真实服务器或真实账号。
- [x] 注入固定的服务器、活动、任务、工具调用、错误、长文本和空状态数据。
- [x] 对概览、项目、任务、活动、设置和终端的桌面/窄窗口状态进行截图与 DOM 检查。
- [x] 修复原生事件订阅、Agent 默认覆盖、窄屏按钮换行和终端 fixture schema 回归，并复核。
- [x] 输出：[截图索引、问题编号和环境限制](./audit-round-3.md)。

## 4. 重构工作流

1. 先记录证据，再改代码；每次只改变一个可验证的 seam。
2. 优先把命令层变成薄适配器，把状态转换、校验和生命周期逻辑放进有小接口的深模块。
3. 大文件按职责和不变量拆分，不按行数机械切片；测试随实现一起移动或补齐。
4. 每个阶段都运行最小相关测试，再运行完整门禁；失败时记录命令、环境和第一处错误。
5. 不为了降低行数引入没有第二个适配器支持的抽象，不把简单转发层误认为深模块。

## 5. 当前执行日志

| 时间 | 阶段 | 动作 | 结果 | 下一步 |
| --- | --- | --- | --- | --- |
| 2026-09-21 | 基线 | 读取 Git 状态、提交、项目结构和现有审查文档 | 工作树已有大规模拆分改动；尚未判断其完整性 | 建立第一轮审计证据并运行门禁 |
| 2026-09-21 | 第一轮 | 运行 `pnpm check` | 基线工作树门禁全绿：契约、类型、单测、bundle、sidecar、Rust 检查均通过 | 记录结构发现并拆分剩余大模块 |
| 2026-09-21 | 第一轮 | 拆分 Tauri 命令和 OAuth/MCP/调查策略 | `commands/mod.rs` 约 1,500 行降至 74 行；MCP、调查、OAuth 根文件均显著收敛；局部 `cargo check`/`clippy` 通过 | 运行完整第二轮验证，确认重构没有引入跨层回归 |
| 2026-09-21 | 第二轮 | 拆分 Agent helpers/工具事件、CSS 样式边界和低频页面加载 | Agent loop 降至约 1,097 行；全局 CSS 改为 7 个有序模块；初始 JS 从约 944 KB 降至约 400 KB | 全量门禁与最终工作树检查 |
| 2026-09-21 | 第三轮 | 用严格 schema 校验的虚构数据审阅本地 GUI | 修复 4 个可复现的预览/响应式问题；控制台无 warning/error | 记录最终验证结果并清理临时 viewport |
| 2026-09-21 | 收尾 | 全量本地门禁与 Rust workspace test | 文档链接、密钥扫描、契约、类型、workspace TypeScript 测试（其中 Agent 259 项）、sidecar 冒烟、Vite build、rustfmt、clippy 和 Rust workspace tests 通过；已恢复默认视口 | 交付，并把真实外部环境验证保留为未验收项 |

## 7. 最终验证表

| 项目 | 结果 | 说明 |
| --- | --- | --- |
| `pnpm check` | 通过 | 文档/发布与密钥扫描、shared 契约、类型、TypeScript 测试、Agent/桌面构建、打包契约及双 sidecar 冒烟均通过 |
| `cargo fmt --all -- --check` | 通过 | Rust 格式无漂移 |
| `cargo clippy -p yukinal-desktop --all-targets -- -D warnings` | 通过 | 桌面 crate 无 clippy warning |
| `cargo test --workspace -- --test-threads=1` | 通过 | workspace 测试和 doc-test 通过；Windows linker 的库创建提示不是测试失败 |
| Vite fixture GUI | 通过（本地范围） | 480 px、1,024 px、主要页面、动态加载终端与控制台检查完成；真实外部链路未验收 |

## 6. 证据与决策规则

- 每个审计发现至少绑定一个文件/符号、可复现命令或截图；无法复现的内容标为待确认。
- 风险等级：P0 为安全/数据损坏/构建阻断，P1 为核心工作流或恢复性，P2 为维护性/性能，P3 为体验或未来能力。
- 本文只记录已发生或明确决定的事情；路线设想放在审计报告，不把计划写成完成结果。
- 完成前必须保留一份最终验证表，逐项写明“通过、失败、未验证（原因）”。
