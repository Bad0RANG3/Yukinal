# 0008 — sidecar 的启动方式：Rust 拉起 `node <bundle>`，按固定顺序解析入口

Status: accepted (2026-09)

## Context

ADR 0001 定了 "Agent Runtime 是独立 Node 进程"，但没定 **谁** 起它、**起什么**。
要求 React 不碰进程；/ 要求无孤儿、体积小；开发期跑 `.ts`，
发布期不可能带 `tsx`。

## Decision

**只有 Rust 能启动 sidecar**，位于 `crates/core::sidecar`（传输/进程）+ `crates/core::supervisor`（所有权与状态）。
Tauri 命令层（`agent_spawn` / `agent_status` / `agent_kill` / `agent_logs`）只做参数编组。

启动目标是**编译后的 JS bundle**（`pnpm --filter @yukinal/agent build` → `apps/agent/dist/index.js`），
不依赖 `tsx`：

```text
YUKINAL_NODE   (或 PATH 上的 node)   +   <entry>
```

入口解析顺序（第一个命中即用，全落空则报可执行的错误）：

| 顺序 | 来源 | 用途 |
| --- | --- | --- |
| 1 | `YUKINAL_AGENT_COMMAND` + `YUKINAL_AGENT_ARGS`（`;` 分隔） | 测试/极端定制 |
| 2 | `YUKINAL_AGENT_ENTRY`（+ 可选 `YUKINAL_NODE`） | CI、指定 bundle |
| 3 | 从当前目录向上找 `apps/agent/dist/index.js` | `tauri dev`（CWD = `src-tauri`） |

失败信息必须自带修复步骤：`no agent bundle to launch (searched …); run \`pnpm --filter @yukinal/agent build\``。

### 生命周期规则

- `initialize` 必须是第一帧（ADR 0006），所以 **spawn → subscribe → handshake** 三步顺序固定：
  订阅晚于握手会永久丢掉 agent 启动日志（broadcast 不回放），代码里已用测试钉住。
- 握手失败（协议不匹配 / describe 报命名冲突）立即杀进程，不留半死状态。
- `kill_on_drop(true)` + 退出时 `RunEvent::ExitRequested` 主动 `stop()`：
  前者是 Windows job object 兜底（实测 `Stop-Process -Force` 硬杀父进程后无孤儿 node），
  后者保证正常退出路径上的礼貌关闭。
- 每次 `start()` 只保留一个 sidecar：已在跑就返回 `alreadyRunning`，不 fork 第二个 agent
  （否则会出现两个进程抢同一份凭据/同一条 trace 流的事故）。
- stderr 一律视为日志并保留最近 200 行进内存（`agent_logs`），崩溃原因必须能在 UI 里看到。

## Consequences

- (+) 开发/CI/生产同一条代码路：dev 用自动解析，CI 用 `YUKINAL_TEST_*`，发布版指向 bundle 资源。
- (+) 无 `tsx`、无第二套 runtime：包体积只增加 Node 本身（ 待量化）。
- (−) Node 运行时如何随安装包分发仍未定（下载便携版 / `node --sea` 单文件 / 逐步下沉 Rust）→ 打包方案定型前必须量化。
- (−) `PATH` 上的 `node` 可被劫持：发布版必须改为随包自带可执行文件的绝对路径，
  且该路径来自安装包资源目录而非搜索 `PATH`（记为 的已知风险，在打包阶段关闭）。
- (−) 一次请求最多等 `request_timeout`（默认 10s，`YUKINAL_AGENT_TIMEOUT_SECS`）；
  长任务应改成"通知流 + 取消"，不靠拉长超时。
