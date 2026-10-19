# R2-D — 跨语言契约穷举对账（IPC 命令 × 宿主 / sidecar / UI 三方）

- **任务**：R2-D「跨语言契约穷举对账（全部 IPC 命令 × 宿主/sidecar/UI 三方 + 响应键集差集）」。
- **产出**：本文件；生成器 `target/r2/audit-contract-matrix.mjs`（1 347 行），原始输出 `target/r2/contract-matrix.json`（368 KB）与附录 B。
- **基线（本次读取的工作区版本，逐文件哈希）**：
  - `HEAD = 736f06a58d07e595fa79ac620d8ce116779bfe4c`（分支 `codex/ui-layout-direction`，提交时间 `2026-10-17T17:06:46+08:00`）；
  - `native/desktop_host/src/ipc_router.cpp` → `git hash-object` `d38eec87bef902034da39d958bc061834548a38b`（10 553 行）
  - `packages/shared/src/schemas/ipc.ts` → `16d3f3b36dce1236c2a5af53eacbb969c7dfd0c8`（743 行）
  - `agent/src/yukinal_agent/sidecar.py` → `0800606936c7d7081c89215a3dc35c5629dadc9b`（2 043 行）
  - `agent/src/yukinal_agent/ssh_tools.py` → `09caa48a80cd027e2fad1e50b0e134e4b39e6a62`（2 469 行）
  - `apps/desktop/src/lib/preview-fixtures.ts` → `2779db38c2765acf50efe1658ced61a76bbbdf6c`（585 行）
  - **工作区与 HEAD 有差异的文件**（他人在改，本报告不据此下门禁结论）：`.github/workflows/check.yml`、`native/CMakeLists.txt`、`native/README.md`、`packages/shared/fixtures/ipc/agent_spawn.json`、`packages/shared/fixtures/ipc/agent_status.json`、`scripts/check.mjs`、`scripts/lib/release-artifacts.mjs`、`scripts/lib/release-artifacts.test.mjs`、`scripts/package.mjs`。凡涉及"门禁覆盖"的结论都以上述哈希为准并标注读取版本。
- **只读约束**：本次**未修改任何被跟踪文件**。全部临时脚本与输出写在 `target/r2/`（`target/` 已在 `.gitignore:13` 忽略），本报告是本轮**唯一新增的被跟踪文件**。注意：工作区在我读取期间**有他人在改**（上一条列出的 9 个文件，以及 `services/control-plane/**` 6 个文件），因此 `git status` 非空；这些改动与本次对账无关，凡涉及它们的结论都以上述哈希为准。
- **方法（全机器生成，无人工枚举/抽样）**：表 1/2/3/4 与全部计数由 `target/r2/audit-contract-matrix.mjs` 等 6 个脚本产出，未手工抄写任何一行。命令行取自 `IPC_COMMANDS` 的字面量（86 条），**不是**手写列表；`IPC_SCHEMAS` 的成员（86 条）与之一一对应，脚本自检 `schemas without a command name = 0`、`commands without a schema = 0`、`commands missing from host = 0`。
- **时间盒**：本轮结束。凡不能从源码静态判定的行，本报告标「无法判定 + 缺什么证据」，不做推测。

---

## 一、总结论

| # | 结论 | 证据 |
| --- | --- | --- |
| 1 | **五个 `failure("NOT_IMPLEMENTED", …)` 站点对 86 条 IPC 命令全部不可达**；「宿主未实现」类为 **0** 行。 | 表 1；§四 |
| 2 | **「sidecar 无 handler」类为 0 行**：27 个 `METHODS` 声明全部有 `elif method ==` 分支；宿主用到的 15 个方法全部命中。 | §五.2 |
| 3 | **13 条命令的响应形状由 Python 单方面决定，宿主原样转发**（`agent_run_start/stop`、`agent_context_reset`、`agent_context_reset_all`、`agent_memory_*`3、`provider_*`6）：今天全部**能通过** `z.strictObject`，但 sidecar 侧**加一个字段就会**让 UI 的 `parse` 抛（其中 12 条键集恰好相等、1 条 `agent_run_start` 另有 3 个 optional 键恒缺席）。 | 表 2 |
| 4 | **3 条死命令**（`agent_kill`、`investigation_task_status_update`、`investigation_schedule_tick`）在宿主有完整实现、有 zod schema、有 fixture 覆盖，但 UI 全树零调用点。 | 表 1；§六.1 |
| 5 | **2 条命令只有 preview fixture 在用**（`remote_file_read`、`file_transfer_get`）。 | 表 1；§六.2 |
| 6 | **`agent_status.restart` 是唯一"schema 声明了、宿主永远不写"的键**：它是 `optional`，所以不会抛，但形状恒为缺席。 | 表 2；§五.1 |
| 7 | **事件词表 23 个名字里 9 个在 `native/`+`agent/` 零命中**（R3 记为 6 个 A 类 + 3 个 B 类，本轮独立复算为 9 个声明名，数字一致）。 | 表 3 |
| 8 | **谎言清单 215 行 / 73 文件**，其中 66 行引用了**不存在的路径**（`crates/`、`apps/desktop/src-tauri`、`apps/agent`、`agent-loop.ts`、`tools/registry.ts`、`crates/database/src/schema.rs` 等）。 | 表 4 |

**五类判定分布（86 行）**：

| 判定 | 行数 |
| --- | ---: |
| 端到端可达 | 81 |
| 宿主未实现(NOT_IMPLEMENTED) | 0 |
| sidecar 无 handler | 0 |
| UI 无调用点（死命令） | 3 |
| 仅 preview fixture 使用 | 2 |
| **合计** | **86** |

**响应形状差集分布（86 行，宿主均已回答）**：

| 判定 | 行数 |
| --- | ---: |
| 通过 | 72 |
| 通过（但未来加字段即抛） | 12 |
| 通过（缺 optional） | 2 |

---

## 二、表 1 — 命令矩阵（86 行 × 四方）

「宿主解析点」列的行号是 `native/desktop_host/src/ipc_router.cpp` 的分派分支行；「sidecar」列 🔎 是宿主在该分支内转发到的 Python 方法与其 `sidecar.py` 行号。

| # | 命令（`IPC_COMMANDS` key） | `IPC_SCHEMAS` | 宿主解析点 `ipc_router.cpp` | sidecar | UI 调用点 | fixture | 判定 |
| ---: | --- | ---: | --- | --- | --- | --- | --- |
| 1 | `core_ping` | 228 | `7138` · invoke 内联 | — 宿主本地 | `lib/runtime.ts:26` | `preview-fixtures.ts:429` | 端到端可达 |
| 2 | `server_list` | 229 | `7145` · invoke 内联 | — 宿主本地 | `lib/servers.ts:14` | `preview-fixtures.ts:431` | 端到端可达 |
| 3 | `workspace_list` | 230 | `7159` · invoke 内联 | — 宿主本地 | `features/projects/ProjectsPane.tsx:25` | `preview-fixtures.ts:444` | 端到端可达 |
| 4 | `server_add` | 231 | `6035` · server_command | — 宿主本地 | `features/servers/AddServerModal.tsx:96` | — | 端到端可达 |
| 5 | `server_update` | 232 | `6035` · server_command | — 宿主本地 | `features/servers/AddServerModal.tsx:95` | — | 端到端可达 |
| 6 | `server_delete` | 233 | `6035` · server_command | — 宿主本地 | `lib/servers.ts:28` | `preview-fixtures.ts:439` | 端到端可达 |
| 7 | `server_connect` | 237 | `6055` · connection_command | — 宿主本地 | `lib/servers.ts:28` | `preview-fixtures.ts:433` | 端到端可达 |
| 8 | `server_disconnect` | 241 | `6055` · connection_command | — 宿主本地 | `lib/servers.ts:28` | `preview-fixtures.ts:436` | 端到端可达 |
| 9 | `server_auth_respond` | 245 | `6056` · connection_command | — 宿主本地 | `features/servers/ServerAuthChallengeModal.tsx:72` | — | 端到端可达 |
| 10 | `server_auth_cancel` | 252 | `6056` · connection_command | — 宿主本地 | `features/servers/ServerAuthChallengeModal.tsx:91` | — | 端到端可达 |
| 11 | `server_host_key_status` | 256 | `6044` · host_key_command | — 宿主本地 | `lib/host-key.ts:41` | `preview-fixtures.ts:580` | 端到端可达 |
| 12 | `server_host_key_probe` | 260 | `6048` · server_read_command | — 宿主本地 | `lib/host-key.ts:72` | — | 端到端可达 |
| 13 | `server_host_key_trust` | 264 | `6048` · server_read_command | — 宿主本地 | `lib/host-key.ts:83` | — | 端到端可达 |
| 14 | `server_host_key_forget` | 272 | `6044` · host_key_command | — 宿主本地 | `lib/host-key.ts:88` | — | 端到端可达 |
| 15 | `server_snapshot` | 276 | `6050` · server_read_command | ✅ `server.snapshot.normalize` → `sidecar.py:192` | `features/overview/ServerOverview.tsx:172` | `preview-fixtures.ts:465` | 端到端可达 |
| 16 | `server_services` | 280 | `6049` · server_read_command | ✅ `server.snapshot.normalize` → `sidecar.py:192` | `features/services/ServicesPane.tsx:42` | `preview-fixtures.ts:467` | 端到端可达 |
| 17 | `server_logs` | 284 | `6049` · server_read_command | ✅ `server.snapshot.normalize` → `sidecar.py:192` | `features/logs/LogsPane.tsx:42` | `preview-fixtures.ts:476` | 端到端可达 |
| 18 | `remote_file_list` | 288 | `6050` · server_read_command | — 宿主本地 | `features/files/RemoteFilesPane.tsx:41` | `preview-fixtures.ts:486` | 端到端可达 |
| 19 | `remote_file_read` | 292 | `6051` · server_read_command | — 宿主本地 | **无** | `preview-fixtures.ts:500` | 仅 preview fixture 使用 |
| 20 | `remote_file_preview` | 296 | `6051` · server_read_command | ✅ `server.snapshot.normalize` → `sidecar.py:192` | `features/files/RemoteFilesPane.tsx:46` | `preview-fixtures.ts:506` | 端到端可达 |
| 21 | `file_prepare_remote_drag` | 300 | `7618` · remote_drag_prepare_command | — 宿主本地 | `features/files/RemoteFilesPane.tsx:145` | — | 端到端可达 |
| 22 | `file_drag_out_start` | 307 | `7622` · remote_drag_start_command | — 宿主本地 | `features/files/RemoteFilesPane.tsx:151` | — | 端到端可达 |
| 23 | `local_file_preview` | 311 | `7572` · invoke 内联 | — 宿主本地 | `features/files/RemoteFilesPane.tsx:51` | `preview-fixtures.ts:508` | 端到端可达 |
| 24 | `local_path_pick` | 315 | `7589` · invoke 内联 | — 宿主本地 | `features/files/RemoteFilesPane.tsx:104` | `preview-fixtures.ts:510` | 端到端可达 |
| 25 | `file_transfer_upload` | 319 | `7612` · transfer_command | — 宿主本地 | `features/files/RemoteFilesPane.tsx:122` | — | 端到端可达 |
| 26 | `file_transfer_download` | 327 | `7612` · transfer_command | — 宿主本地 | `features/files/RemoteFilesPane.tsx:134` | — | 端到端可达 |
| 27 | `file_transfer_list` | 335 | `7613` · transfer_command | — 宿主本地 | `features/files/RemoteFilesPane.tsx:56` | `preview-fixtures.ts:519` | 端到端可达 |
| 28 | `file_transfer_get` | 339 | `7613` · transfer_command | — 宿主本地 | **无** | `preview-fixtures.ts:514` | 仅 preview fixture 使用 |
| 29 | `file_transfer_cancel` | 343 | `7614` · transfer_command | — 宿主本地 | `features/files/RemoteFilesPane.tsx:228` | — | 端到端可达 |
| 30 | `file_transfer_resolve_conflict` | 347 | `7614` · transfer_command | — 宿主本地 | `features/files/RemoteFilesPane.tsx:161` | — | 端到端可达 |
| 31 | `activity_list` | 351 | `7173` · invoke 内联 | — 宿主本地 | `features/activity/ActivityFeed.tsx:63` | `preview-fixtures.ts:521` | 端到端可达 |
| 32 | `tool_execution_list` | 358 | `7219` · invoke 内联 | — 宿主本地 | `features/activity/ActivityFeed.tsx:114` | `preview-fixtures.ts:532` | 端到端可达 |
| 33 | `terminal_open` | 366 | `6060` · terminal_command | — 宿主本地 | `features/terminal/TerminalPane.tsx:176` | `preview-fixtures.ts:541` | 端到端可达 |
| 34 | `terminal_write` | 374 | `6060` · terminal_command | — 宿主本地 | `features/terminal/TerminalPane.tsx:197` | `preview-fixtures.ts:543` | 端到端可达 |
| 35 | `terminal_resize` | 378 | `6061` · terminal_command | — 宿主本地 | `features/terminal/TerminalPane.tsx:192` | `preview-fixtures.ts:544` | 端到端可达 |
| 36 | `terminal_close` | 386 | `6061` · terminal_command | — 宿主本地 | `features/terminal/TerminalPane.tsx:183` | `preview-fixtures.ts:545` | 端到端可达 |
| 37 | `agent_spawn` | 390 | `7547` · agent_command | — 宿主本地 | `lib/runtime.ts:60` | — | 端到端可达 |
| 38 | `agent_kill` | 391 | `7547` · agent_command | — 宿主本地 | **无** | — | UI 无调用点（死命令） |
| 39 | `agent_status` | 392 | `7547` · agent_command | — 宿主本地 | `lib/runtime.ts:40` | `preview-fixtures.ts:551` | 端到端可达 |
| 40 | `agent_logs` | 393 | `7548` · agent_command | — 宿主本地 | `lib/runtime.ts:53` | `preview-fixtures.ts:553` | 端到端可达 |
| 41 | `agent_run_start` | 394 | `7560` · agent_run_command | ✅ `agent.run.start` → `sidecar.py:286` | `features/agent/useAgentRun.ts:385` | — | 端到端可达 |
| 42 | `agent_run_stop` | 443 | `7560` · agent_run_command | ✅ `agent.run.stop` → `sidecar.py:288` | `features/agent/useAgentRun.ts:436` | — | 端到端可达 |
| 43 | `agent_context_reset` | 447 | `7558` · agent_run_command | ✅ `agent.context.reset` → `sidecar.py:292` | `features/agent/AgentPanel.tsx:391` | — | 端到端可达 |
| 44 | `agent_context_reset_all` | 451 | `7558` · agent_run_command | ✅ `agent.context.resetAll` → `sidecar.py:294` | `features/agent/MemoryLibrary.tsx:38` | — | 端到端可达 |
| 45 | `agent_memory_list` | 455 | `7559` · agent_run_command | ✅ `agent.memory.list` → `sidecar.py:298` | `features/agent/MemoryLibrary.tsx:30` | — | 端到端可达 |
| 46 | `agent_memory_save` | 456 | `7559` · agent_run_command | ✅ `agent.memory.save` → `sidecar.py:300` | `features/agent/MemoryLibrary.tsx:32` | — | 端到端可达 |
| 47 | `agent_memory_delete` | 457 | `7559` · agent_run_command | ✅ `agent.memory.delete` → `sidecar.py:302` | `features/agent/MemoryLibrary.tsx:35` | — | 端到端可达 |
| 48 | `agent_approval_respond` | 458 | `7561` · agent_run_command | ✅ `agent.approval.respond` → `sidecar.py:290` | `features/agent/useAgentRun.ts:460` | — | 端到端可达 |
| 49 | `chat_session_list` | 462 | `7274` · invoke 内联 | — 宿主本地 | `features/agent/AgentHistoryPane.tsx:137` | `preview-fixtures.ts:555` | 端到端可达 |
| 50 | `chat_session_get` | 474 | `7319` · invoke 内联 | — 宿主本地 | `features/agent/AgentHistoryPane.tsx:282` | `preview-fixtures.ts:564` | 端到端可达 |
| 51 | `chat_session_create` | 478 | `7338` · invoke 内联 | — 宿主本地 | `features/agent/useChatSessions.ts:127` | — | 端到端可达 |
| 52 | `chat_message_append` | 487 | `7385` · invoke 内联 | — 宿主本地 | `features/agent/useChatSessions.ts:92` | — | 端到端可达 |
| 53 | `chat_session_archive` | 506 | `7478` · invoke 内联 | — 宿主本地 | `features/agent/AgentHistoryPane.tsx:160` | — | 端到端可达 |
| 54 | `chat_session_rename` | 510 | `7501` · invoke 内联 | — 宿主本地 | `features/agent/AgentHistoryPane.tsx:170` | — | 端到端可达 |
| 55 | `chat_session_delete` | 517 | `7521` · invoke 内联 | — 宿主本地 | `features/agent/AgentHistoryPane.tsx:183` | — | 端到端可达 |
| 56 | `provider_list` | 521 | `7552` · provider_command | ✅ `provider.list` → `sidecar.py:278` | `lib/providers.ts:20` | `preview-fixtures.ts:547` | 端到端可达 |
| 57 | `provider_save` | 522 | `7552` · provider_command | ✅ `provider.save` → `sidecar.py:280` | `features/settings/RuntimeSettings.tsx:296` | — | 端到端可达 |
| 58 | `provider_activate` | 526 | `7553` · provider_command | ✅ `provider.activate` → `sidecar.py:284` | `features/onboarding/GettingStarted.tsx:46` | — | 端到端可达 |
| 59 | `provider_delete` | 530 | `7553` · provider_command | ✅ `provider.delete` → `sidecar.py:282` | `features/settings/RuntimeSettings.tsx:189` | — | 端到端可达 |
| 60 | `provider_models` | 534 | `7554` · provider_command | ✅ `provider.models` → `sidecar.py:274` | `features/settings/RuntimeSettings.tsx:273` | `preview-fixtures.ts:549` | 端到端可达 |
| 61 | `mcp_server_list` | 538 | `7565` · mcp_command | — 宿主本地 | `lib/mcp.ts:47` | `preview-fixtures.ts:576` | 端到端可达 |
| 62 | `mcp_server_save` | 539 | `7565` · mcp_command | — 宿主本地 | `lib/mcp.ts:73` | — | 端到端可达 |
| 63 | `mcp_server_delete` | 543 | `7566` · mcp_command | — 宿主本地 | `lib/mcp.ts:77` | — | 端到端可达 |
| 64 | `mcp_server_start` | 547 | `7567` · mcp_command | — 宿主本地 | `lib/mcp.ts:81` | — | 端到端可达 |
| 65 | `mcp_server_stop` | 551 | `7567` · mcp_command | — 宿主本地 | `lib/mcp.ts:85` | — | 端到端可达 |
| 66 | `mcp_server_review` | 555 | `7566` · mcp_command | — 宿主本地 | `lib/mcp.ts:89` | — | 端到端可达 |
| 67 | `mcp_oauth_connect` | 559 | `7568` · mcp_command | — 宿主本地 | `lib/mcp.ts:93` | — | 端到端可达 |
| 68 | `mcp_oauth_cancel` | 563 | `7568` · mcp_command | — 宿主本地 | `lib/mcp.ts:99` | — | 端到端可达 |
| 69 | `network_proxy_get` | 567 | `6040` · network_proxy_command | — 宿主本地 | `lib/network.ts:34` | `preview-fixtures.ts:578` | 端到端可达 |
| 70 | `network_proxy_save` | 571 | `6040` · network_proxy_command | — 宿主本地 | `lib/network.ts:81` | — | 端到端可达 |
| 71 | `investigation_task_list` | 575 | `6065` · invoke 内联 | — 宿主本地 | `features/investigations/InvestigationsPane.tsx:120`（字面量） | `preview-fixtures.ts:567` | 端到端可达 |
| 72 | `investigation_task_get` | 579 | `6103` · invoke 内联 | — 宿主本地 | `features/workpages/WorkpagesDock.tsx:128` | `preview-fixtures.ts:569` | 端到端可达 |
| 73 | `investigation_task_create` | 583 | `6124` · invoke 内联 | — 宿主本地 | `features/investigations/InvestigationsPane.tsx:158`（字面量） | — | 端到端可达 |
| 74 | `investigation_task_start` | 587 | `6153` · invoke 内联 | — 宿主本地 | `features/investigations/InvestigationsPane.tsx:144`（字面量） | — | 端到端可达 |
| 75 | `investigation_task_stop` | 591 | `6569` · invoke 内联 | — 宿主本地 | `features/investigations/InvestigationsPane.tsx:151`（字面量） | — | 端到端可达 |
| 76 | `investigation_task_status_update` | 595 | `6524` · invoke 内联 | — 宿主本地 | **无** | — | UI 无调用点（死命令） |
| 77 | `investigation_task_recover` | 599 | `6385` · invoke 内联 | — 宿主本地 | `features/investigations/InvestigationsPane.tsx:192`（字面量） | — | 端到端可达 |
| 78 | `investigation_brief_select` | 603 | `6614` · invoke 内联 | — 宿主本地 | `features/investigations/InvestigationsPane.tsx:179`（字面量） | — | 端到端可达 |
| 79 | `investigation_retention_preview` | 607 | `6716` · invoke 内联 | — 宿主本地 | `features/investigations/InvestigationsPane.tsx:220`（字面量） | — | 端到端可达 |
| 80 | `investigation_retention_prune` | 611 | `6767` · invoke 内联 | — 宿主本地 | `features/investigations/InvestigationsPane.tsx:223`（字面量） | — | 端到端可达 |
| 81 | `investigation_schedule_list` | 615 | `6826` · invoke 内联 | — 宿主本地 | `features/investigations/InvestigationsPane.tsx:132`（字面量） | `preview-fixtures.ts:572` | 端到端可达 |
| 82 | `investigation_schedule_runs` | 619 | `6841` · invoke 内联 | — 宿主本地 | `features/investigations/InvestigationsPane.tsx:141`（字面量） | `preview-fixtures.ts:574` | 端到端可达 |
| 83 | `investigation_schedule_create` | 623 | `6862` · invoke 内联 | — 宿主本地 | `features/investigations/InvestigationsPane.tsx:207`（字面量） | — | 端到端可达 |
| 84 | `investigation_schedule_update` | 627 | `6950` · invoke 内联 | — 宿主本地 | `features/investigations/InvestigationsPane.tsx:213`（字面量） | — | 端到端可达 |
| 85 | `investigation_schedule_tick` | 631 | `7099` · invoke 内联 | — 宿主本地 | **无** | — | UI 无调用点（死命令） |
| 86 | `provider_test` | 635 | `7554` · provider_command | ✅ `provider.test` → `sidecar.py:276` | `features/onboarding/GettingStarted.tsx:32` | — | 端到端可达 |

---

## 三、表 2 — 响应键集对账

### 3.1 判据

`apps/desktop/src/lib/ipc.ts:113` 是 UI 唯一入口：`IPC_SCHEMAS[command].response.parse(raw)` —— 用 `parse`，不是 cast。因此**根 schema 是不是 `z.strictObject` 决定了"多一个键"是致命还是无感**。脚本按下面的顺序解析"谁真正产出这些键"，只有解析失败才写「无法判定」：

1. C++ 路由自己写进 reply payload 的键（`return {true, {{"k", …}}}`，含 `response["k"] = …` 追加）；
2. `return {true, helper(...)}` 中 helper 在**另一个编译单元**时，下钻到 helper 的 `return` 字面量（`transfer_snapshot_reply`、`view_for`、`oauth_connect`、`get_investigation_task` …）；
3. `return {true, client.request(...)}`：宿主**不构造任何键**，键由 Python 决定 → 下钻到 `sidecar.py` 的 handler 的 `return` 字面量；
4. `return {true, normalized.at("output")}`：下钻到 `ssh_tools.py` 的归一化函数。

86 行里 **82 行的根 schema 是 `z.strictObject`**，4 行是 `EMPTY_PAYLOAD = z.record(z.string(), z.never())`（`file_drag_out_start`、`terminal_write`、`terminal_resize`、`terminal_close`——**任一键都会被 `z.never` 拒绝**，所以对它们而言"多键"和"少键"一样致命）。**没有一行使用非严格 `z.object`**，**没有一行无法解析**。

### 3.2 有差异的 14 行

> 另外 72 行**键集与 schema 完全一致**（零 extra、零 missing），不列出。完整逐行数据见附录 B 的 `rows[].hostKeys / extraInHost / missingFromHost`。

| 命令 | zod 根构造 | zod required / optional | 实际产出键的一方 | 该方可能返回的键集 | 判定 | 依据 |
| --- | --- | --- | --- | --- | --- | --- |
| `agent_status` | strictObject | 8 / 1 | C++ 路由内联字面量 | `agentVersion` `entry` `lastExit` `pid` `protocolVersion` `running` `startedAt` `toolCount` | **通过（缺 optional）** | 所有可达分支都缺 optional `restart`，合法 |
| `agent_run_start` | strictObject | 2 / 3 | sidecar `run_start` `sidecar.py:594`（宿主原样转发，不自己构造键） | `duplicate` `runId` `started` ／ `runId` `started` | **通过（缺 optional）** | 所有可达分支都缺 optional `resumed` `result` `duplicate`，合法 |
| `agent_run_stop` | strictObject | 1 / 0 | sidecar `run_stop` `sidecar.py:763`（宿主原样转发，不自己构造键） | `stopped` | **通过（但未来加字段即抛）** | 宿主原样转发 RPC，自身不构造键；sidecar 加一个字段就会被 `z.strictObject` 拒绝 |
| `agent_context_reset` | strictObject | 1 / 0 | sidecar `context_reset` `sidecar.py:805`（宿主原样转发，不自己构造键）；已剔除不可达分支 `deleted` | `reset` | **通过（但未来加字段即抛）** | 宿主原样转发 RPC，自身不构造键；sidecar 加一个字段就会被 `z.strictObject` 拒绝 |
| `agent_context_reset_all` | strictObject | 2 / 0 | sidecar `context_reset_all` `sidecar.py:835`（宿主原样转发，不自己构造键） | `count` `reset` | **通过（但未来加字段即抛）** | 宿主原样转发 RPC，自身不构造键；sidecar 加一个字段就会被 `z.strictObject` 拒绝 |
| `agent_memory_list` | strictObject | 1 / 0 | sidecar `memory_list` `sidecar.py:848`（宿主原样转发，不自己构造键） | `memories` | **通过（但未来加字段即抛）** | 宿主原样转发 RPC，自身不构造键；sidecar 加一个字段就会被 `z.strictObject` 拒绝 |
| `agent_memory_save` | strictObject | 1 / 0 | sidecar `memory_save` `sidecar.py:854`（宿主原样转发，不自己构造键） | `memory` | **通过（但未来加字段即抛）** | 宿主原样转发 RPC，自身不构造键；sidecar 加一个字段就会被 `z.strictObject` 拒绝 |
| `agent_memory_delete` | strictObject | 1 / 0 | sidecar `memory_delete` `sidecar.py:863`（宿主原样转发，不自己构造键） | `deleted` | **通过（但未来加字段即抛）** | 宿主原样转发 RPC，自身不构造键；sidecar 加一个字段就会被 `z.strictObject` 拒绝 |
| `provider_list` | strictObject | 1 / 0 | sidecar `provider_list` `sidecar.py:403`（宿主原样转发，不自己构造键） | `providers` | **通过（但未来加字段即抛）** | 宿主原样转发 RPC，自身不构造键；sidecar 加一个字段就会被 `z.strictObject` 拒绝 |
| `provider_save` | strictObject | 1 / 0 | sidecar `provider_save` `sidecar.py:421`（宿主原样转发，不自己构造键） | `provider` | **通过（但未来加字段即抛）** | 宿主原样转发 RPC，自身不构造键；sidecar 加一个字段就会被 `z.strictObject` 拒绝 |
| `provider_activate` | strictObject | 1 / 0 | sidecar `provider_activate` `sidecar.py:482`（宿主原样转发，不自己构造键） | `provider` | **通过（但未来加字段即抛）** | 宿主原样转发 RPC，自身不构造键；sidecar 加一个字段就会被 `z.strictObject` 拒绝 |
| `provider_delete` | strictObject | 2 / 0 | sidecar `provider_delete` `sidecar.py:466`（宿主原样转发，不自己构造键） | `credentialReclaimed` `deleted` | **通过（但未来加字段即抛）** | 宿主原样转发 RPC，自身不构造键；sidecar 加一个字段就会被 `z.strictObject` 拒绝 |
| `provider_models` | strictObject | 1 / 0 | sidecar `provider_models` `sidecar.py:493`（宿主原样转发，不自己构造键） | `models` | **通过（但未来加字段即抛）** | 宿主原样转发 RPC，自身不构造键；sidecar 加一个字段就会被 `z.strictObject` 拒绝 |
| `provider_test` | strictObject | 1 / 0 | sidecar `provider_test` `sidecar.py:525`（宿主原样转发，不自己构造键） | `ok` | **通过（但未来加字段即抛）** | 宿主原样转发 RPC，自身不构造键；sidecar 加一个字段就会被 `z.strictObject` 拒绝 |

### 3.3 关于 `strictObject` 的现状性判定（与 R3 对齐）

R3（`docs/review/r3-verification.md`）对 desktop H-2 的判定是：**「sidecar 多一个字段就把 UI 打崩」的现状性暗示不成立**——20 组生产端真实形状复测中，当前没有任何真实响应会被拒绝；风险是"未来加字段即崩"。本轮独立对账**与该判定一致**，并且把范围从 R3 的 20 组扩到全部 86 条命令：

- 当前**没有一条**命令因 host extra key 或 host missing required key 而被 `z.strictObject` 拒绝（`会抛 = 0`）；
- 12 条 verbatim 转发命令之所以落在"未来即崩"一格（另有 `agent_run_start` 因额外缺 3 个 optional 键而另列），是因为**宿主根本不是这些键的生产者**——键由 `sidecar.py` 决定，宿主的 `IPC_SCHEMAS` 却按 `strictObject` 校验。也就是说：**今天不崩，是因为两侧键集恰好相等；契约没有任何机制保证它继续相等。**
- 因此本报告的措辞一律是「未来加字段即抛」，不用「已经崩了」。

---

## 四、五个 `NOT_IMPLEMENTED` 站点的可达性

```
ipc_router.cpp:7643  return failure("NOT_IMPLEMENTED",
ipc_router.cpp:7914  return failure("NOT_IMPLEMENTED",
ipc_router.cpp:9072  return failure("NOT_IMPLEMENTED", "This Agent command has not migrated yet: " + method);
ipc_router.cpp:9089  return failure("NOT_IMPLEMENTED", "This Agent run command has not migrated yet: " + method);
ipc_router.cpp:9654  return failure("NOT_IMPLEMENTED", "This provider command has not migrated yet: " + method);
```

脚本 `target/r2/audit-ni-reach.mjs` 对每个站点做两级判定：

1. **谁的字面量在这个函数体里被测试**：把 86 条命令的字面量逐一在该站点所在函数的函数体（截到站点前一行）里查找；
2. **这个函数体**是不是"只有直接字面量分派"。

结果：

| NI 站点 | 所在函数 | 该函数归属命令数 | 其中在尾之前被字面量测试 | 靠该函数体内的 `kMethods` 表分派（也用 `found->second`，字面量不在本函数） | 会落到该尾的 IPC 命令 |
| --- | --- | ---: | ---: | ---: | ---: |
| 7643 | `IpcRouter::invoke` (6025-7646) | 29 | 29 | 0 | **0** |
| 7914 | `IpcRouter::transfer_command` (7647-7917) | 6 | 6 | 0 | **0** |
| 9072 | `IpcRouter::agent_command` (9006-9074) | 4 | 4 | 0 | **0** |
| 9089 | `IpcRouter::agent_run_command` (9075-9180) | 8 | 8 | **8**（重复计数：字面量与 kMethods 表都在本函数内，见下） | **0** |
| 9654 | `IpcRouter::provider_command` (9643-9665) | 6 | 6 | **6**（同上） | **0** |

**判定：这 5 个尾对 86 条 IPC 命令全部不可达。** 三点依据：

1. 86 条命令的**每一条**都在 `IpcRouter::invoke` 内有自己的字面量分支（`invoke` 内未被字面量测试的命令数 = **0**），所以没有任何一条会走到 `invoke` 的泛型尾（7643）；
2. 四个具名 handler 各自只服务一组命令，且这组命令**每一条**都在该 handler 的 NI 尾之前有自己的字面量分支（`transfer_command` 6/6、`agent_command` 4/4、`agent_run_command` 8/8、`provider_command` 6/6）；
3. 例外需要单独说明：`agent_run_command` 与 `provider_command` 用 `kMethods` 表把命令映射到 RPC 方法名，但**表前仍有逐条字面量前置校验**（`ipc_router.cpp:9094` 的 `agent_run_start`/`agent_context_reset` 前置条件、9119 的 `if (method != "agent_approval_respond")`、9077-9086 / 9643-9652 的 kMethods 定义），因此"不在表里"的值仍然只能在到达 NI 尾**之前**被字面量分支拦下。脚本对这两行做了重复计数（字面量与 kMethods 都在同一函数体内），故表格里标注了这一点。

这 5 个尾真正防的是"宿主收到一个不在 `IPC_COMMANDS` 里的方法名"。UI 侧 `callDesktop` 的 `command` 参数被 `IpcCommandName` 约束（`apps/desktop/src/lib/ipc.ts:5`「UI cannot invent native surface」），所以在**本仓库的调用面**上这条防御确实打不到。但它是**有意的纵深防御**，不算契约谎言——本报告把它记为"<?>"而不是"宿主未实现"。

### 4.1 「宿主未实现(NOT_IMPLEMENTED)」类为什么是 0 行

本任务要求"对每一个『宿主未实现』行引用 `failure("NOT_IMPLEMENTED", ...)` 的 文件:行"。**穷举的结论是该类为 0 行**，因此没有可引用的行——但为了让这条结论可复核，这里列出全部 5 个站点的 `文件:行` 与它们的归属函数（上表即逐一证伪）：

| 文件:行 | 原文 |
| --- | --- |
| `native/desktop_host/src/ipc_router.cpp:7643` | `return failure("NOT_IMPLEMENTED",` |
| `native/desktop_host/src/ipc_router.cpp:7914` | `return failure("NOT_IMPLEMENTED",` |
| `native/desktop_host/src/ipc_router.cpp:9072` | `return failure("NOT_IMPLEMENTED", "This Agent command has not migrated yet: " + method);` |
| `native/desktop_host/src/ipc_router.cpp:9089` | `return failure("NOT_IMPLEMENTED", "This Agent run command has not migrated yet: " + method);` |
| `native/desktop_host/src/ipc_router.cpp:9654` | `return failure("NOT_IMPLEMENTED", "This provider command has not migrated yet: " + method);` |

> 换句话说：86 条 `IPC_COMMANDS` 在宿主侧**全部**有实现分支（`commands missing from host = 0`，脚本自检项），5 个 NI 尾都只对"非法方法名"生效。

> **未判定项**：我无法从静态源码判定"是否有非 UI 的 IPC 客户端"（例如外部脚本直接调 `window.__YUKINAL_HOST__.invoke`）。证据缺口：需要 WebView 侧 `invoke` 的全部注册/暴露点，或一份明确"UI 是唯一客户端"的威胁模型。就本仓库而言，唯一调用点是 `apps/desktop/src/lib/ipc.ts`。

---

## 五、逐条核对 R1 的 desktop 发现

### 5.1 H-1 `agent_status.restart` 无生产者 —— **成立（本轮独立复现）**

- `packages/shared/src/schemas/ipc.ts:158` 声明 `restart: RestartRecordSchema.optional()`，并在 153-157 行给出理由「the field is omitted when there is nothing to report (`skip_serializing_if` on the Rust side)」。**"Rust side" 已不存在**，这条理由现在指的是 Python，而 Python 从不发它。
- 宿主侧：`ipc_router.cpp:9012-9035` 构造 8 个键（`running/pid/protocolVersion/agentVersion/toolCount/entry/startedAt/lastExit`），**从不写 `restart`**；`if (status.last_exit.has_value())` 只改 `lastExit` 的值，不新增键。
- 结论：该键在**任何**宿主状态下都缺席。因是 `optional`，不会抛——所以它不是 bug，是**契约里的一句无依据承诺**。

### 5.2 `sidecar 无 handler` 类为 0 —— 与 R1 的 6 个"无生产者通道"区分开

- `sidecar.py:58-86` 的 `METHODS` 声明 **27** 个方法；`METHODS` 与 `handle` / `handle_async` 的 `elif method ==` 链比对后，**27 个全部有分支**，`RPC_NOT_IMPLEMENTED`（`sidecar.py:228-229`）当前**没有任何 IPC 方法会命中**。
- 宿主实际转发到的方法共 **15** 个（`agent.approval.respond`、`agent.context.reset`、`agent.context.resetAll`、`agent.memory.list/save/delete`、`agent.run.start/stop`、`provider.activate/delete/list/models/save/test`、`server.snapshot.normalize`），全部有 handler。
- 其余 12 个 `METHODS`（`docker.*`、`systemd.inspect.normalize`、`package.inspect.normalize`、`system.ping`、`system.describe`、`tools.list`、`initialize`、`server.logs.normalize`、`server.services.normalize`）中，后三个由宿主在 `server_read_command` 里按字面量转发；剩下的由 sidecar 自身/测试驱动。**"声明了但没实现"这一类的行数 = 0。**

> **注意与 R1 C-2 的区别**：C-2 说的是**事件通道**没有生产者（表 3），不是 IPC 命令没有 handler。两者不要混。

### 5.3 C-2 事件词表 —— 独立复算

`packages/shared/src/events/index.ts:18-42` 声明 `EVENT_NAMES` **23** 个名字。脚本 `target/r2/audit-event-producers.mjs` 用 `git grep -c --fixed-strings` 对每个名字在 `native/` + `agent/` 计数（不使用 Word 边界，避免把 `server.updated_at` 之类算进来，也不使用正则，避免转义歧义）：

| 事件名（`EVENT_NAMES`） | `native/` + `agent/` 命中 | 判定 |
| --- | ---: | --- |
| `server.connected` | 0 | **无生产者（声明名）** |
| `server.disconnected` | 0 | **无生产者（声明名）** |
| `server.updated` | 0 | **无生产者（声明名）** |
| `server.auth_challenge` | 0 | **无生产者（声明名）** |
| `agent.thinking` | 0 | **无生产者（声明名）** |
| `agent.usage` | 0 | **无生产者（声明名）** |
| `terminal.output_lost` | 0 | **无生产者（声明名）** |
| `activity.created` | 0 | **无生产者（声明名）** |
| `investigation.schedule_notification` | 0 | **无生产者（声明名）** |
| `mcp.oauth_device_code` | 1 | 有生产者 |
| `agent.started` | 1 | 有生产者 |
| `agent.text` | 1 | 有生产者 |
| `agent.tool_call` | 5 | 有生产者 |
| `agent.tool_result` | 4 | 有生产者 |
| `agent.waiting_approval` | 6 | 有生产者 |
| `agent.approval_expired` | 1 | 有生产者 |
| `agent.completed` | 8 | 有生产者 |
| `agent.failed` | 2 | 有生产者 |
| `terminal.opened` | 1 | 有生产者 |
| `terminal.data` | 1 | 有生产者 |
| `terminal.closed` | 1 | 有生产者 |
| `file.local_dropped` | 2 | 有生产者 |
| `file.transfer_updated` | 2 | 有生产者 |

**9 个零命中**与 R3 的"6 个 A 类 + 3 个 B 类"在**个数与名字上都一致**（`server.connected`、`server.disconnected`、`server.updated`、`server.auth_challenge`、`agent.thinking`、`agent.usage`、`terminal.output_lost`、`activity.created`、`investigation.schedule_notification`）。R3 另指出 `terminal.opened` 是第三类（**有生产者、无订阅者**）——它在 `desktop_terminal.cpp:391` 被 emit，命中数 1，本轮复算一致。

> 与 R3 的口径差异：R3 把这 9 个分成"6 个 A 类（`SUBSCRIBABLE` 通道）+ 3 个 B 类（仅声明名）"；本报告只有一份 23 项词表可读，`SUBSCRIBABLE` 这个常量在当前工作区**不存在**（`git grep SUBSCRIBABLE` 只命中 `event-vocabulary.test.ts` 里的**字符串字面量**，不是导出常量）。因此本报告只报"零命中的 9 个声明名"，不复制 A/B 分类。

---

## 六、死命令与仅 fixture 命令

### 6.1 UI 无调用点（死命令）— 3 条

| # | 命令 | 宿主实现 | 宿主构造键 | zod 响应键 | fixture | UI 命中（camelCase 与字面量双查） |
| --- | --- | --- | --- | --- | --- | ---: |
| D1 | `agent_kill` | `ipc_router.cpp:7547` · `IpcRouter::agent_command` | `killed` | `killed` | 无 | 0 |
| D2 | `investigation_task_status_update` | `ipc_router.cpp:6524` · invoke 内联 | `task` | `task` | 无 | 0 |
| D3 | `investigation_schedule_tick` | `ipc_router.cpp:7099` · invoke 内联 | `claimed`, `skipped` | `claimed`, `skipped` | 无 | 0 |

三条的判定都经过**两种**查找：`IPC_COMMANDS.<camelCase>` 与字符串字面量 `"<command>"`，在 `apps/desktop/`（src + tests）下命中数均为 0。`agent_kill` 尤其值得注意：UI 有 `useAgentRun` 的停止路径，走的是 `agent_run_stop`（`ipc_router.cpp:9079` → `agent.run.stop`），**不是** `agent_kill`（整进程杀 sidecar）。

> 这三条不是"宿主没实现"，恰恰相反：**宿主实现完整、schema 完整、键集与 schema 一致**（零 extra、零 missing），缺的只是调用方。所以它们不是契约缺陷，是**契约里多出来的三条命令**。

> **未判定项**：`investigation_task_status_update` / `investigation_schedule_tick` 是否属于"由定时任务/后台驱动、本就不该由 UI 调用"，我无法从静态源码判定。证据缺口：一份说明这两条命令预期调用方的设计文档；`docs/` 内未搜到相关描述。

### 6.2 仅 preview fixture 使用 — 2 条

| 命令 | 宿主实现 | 唯一消费点 |
| --- | --- | --- |
| `remote_file_read` | `ipc_router.cpp:6051` | `preview-fixtures.ts:500` |
| `file_transfer_get` | `ipc_router.cpp:7613` | `preview-fixtures.ts:514` |

这两条在 UI 里既无 `IPC_COMMANDS.*` 调用也无字面量调用；`preview-fixtures.ts` 的 `default:` 分支（583 行）对其它命令抛「模拟数据模式仅供界面评估…」，说明 fixture 是界面评估通道，不是生产路径。

---

## 七、谁在撒谎（表 4）

**扫描口径（先说清楚我扫了什么、没扫什么）**：脚本 `target/r2/scan-liars.mjs` 枚举 `git ls-files` 的全部文件（跳过 `node_modules`/`target`/`dist`/`.venv`/`build`/`coverage`/`cargo-home`/`build-nmake`/`build-msvc`），对每行做四类匹配：

- `claim:tauri`：`Tauri` / `tauriEventName` / `tauri_event_name`；
- `claim:rust-emits`：`Rust emits` / `Rust side` / `Rust 侧` / `the Rust host` / `Rust mirrors`；
- `claim:mirrors` / `claim:asserted-both-sides`：`mirrors \`…\`` / `both sides` / `两边`；
- `claim:verified-at`：`verified at` + 路径；
- `deleted-path:X`：`crates/`、`apps/desktop/src-tauri`、`src-tauri`、`apps/agent`、`agent-loop.ts`、`policy-registry.ts`、`tools/registry.ts`。

**总数**：扫描 540 文件，73 个有命中，共 215 行，其中 66 行引用了不存在的路径，179 行是"已实现/由 X 发出/已核对"类断言。

**未扫**：中文「已实现」「已完成」「对齐」等表述没有单独模式，可能漏检；`docs/` 的历史文档里的删除线/引用块状态也算命中，见 §七.2。

**引用行里的链接已被"消毒"**（脚本 `target/r2/render-report.mjs` 的 `quoteSafe()`）：表 4 的"原文"列是**转述**，其中出现的 markdown 链接若照抄，会以 `docs/review/` 为基点解析，从而把一个**在被引文档里合法**的链接变成无效链接。处理规则有两条，都不改变引文语义：

- 目标是**同目录文件名**（`./rust-removal-plan.md`，被引文档在 `docs/` 下）→ 改写为本报告的**正确相对路径** `../rust-removal-plan.md`，例如 `docs/cpp-desktop-handoff.md:104`、`docs/README.md:12`；
- 目标是**仓库根相对路径**（`./docs/rust-removal-plan.md`，只对根目录下的文档成立）→ **降级为代码**并保留原始目标，例如 `CONTRIBUTING.md:3` 写作 `docs/rust-removal-plan.md → ./docs/rust-removal-plan.md`，因为那是 CONTRIBUTING.md 的链接，不是本报告要读者点的目标。

实测：`node scripts/check-docs-links.mjs` → `green (36 markdown files, 163 links)`，exit 0。

| 文件:行 | 类别 | 原文（截断） |
| --- | --- | --- |
| `docs/rewrite-cpp-python.md:3` | claim:tauri | > **2026-10-17 status:** the Rust/Tauri host has been deleted; the C++ WebView |
| `docs/rewrite-cpp-python.md:7` | claim:tauri | > hosts" or "Tauri development", read them as the state at that time. |
| `docs/rewrite-cpp-python.md:13` | claim:tauri | > existing UI. Tauri development was retired; new package builds select the Python |
| `docs/rewrite-cpp-python.md:104` | claim:tauri | The repository contains a working React/Tauri product baseline and shared |
| `docs/rewrite-cpp-python.md:108` | claim:tauri | being migrated in checked stages. React components and their Tauri calls are |
| `docs/rewrite-cpp-python.md:109` | claim:tauri | preserved; the Rust/Tauri host remains responsible for the native UI bridge, |
| `docs/rewrite-cpp-python.md:132` | claim:tauri | Tauri bridge, local trust-store operations, approval checks, and UI result |
| `docs/rewrite-cpp-python.md:167` | claim:tauri | `pnpm --filter @yukinal/desktop tauri dev` selects this runtime for Tauri |
| `docs/rewrite-cpp-python.md:171` | claim:tauri | validates the Python/C++ resource layout before Tauri builds. Node is no longer |
| `docs/rewrite-cpp-python.md:173` | claim:tauri | branch for older packaged layouts, but the new Tauri installer has not been built |
| `docs/rewrite-cpp-python.md:182` | claim:tauri | ├── host.* JSON-RPC bridge → Tauri/Rust host → remaining host tools and MCP |
| `docs/rewrite-cpp-python.md:191` | claim:tauri | React/Tauri desktop UI: unchanged; the host executes `host.*` tools, while the |
| `docs/rewrite-cpp-python.md:226` | claim:tauri | Tauri host passes only the config's named token and authentication environment |
| `docs/rewrite-cpp-python.md:670` | deleted-path:src-tauri, deleted-path:apps/desktop/src-tauri, claim:tauri | apps/desktop/src-tauri/Cargo.toml --lib --locked` passed. The desktop unit-test |
| `docs/rewrite-cpp-python.md:691` | claim:tauri | normalization layer. Rust remains the Tauri host bridge for input validation, |
| `docs/rewrite-cpp-python.md:714` | claim:tauri | The next milestones are new-layout Tauri installer parity checks, durable |
| `docs/rewrite-cpp-python.md:721` | claim:tauri | stdio lifecycle. The full Tauri installer smoke and Rust-to-Python host |
| `docs/rewrite-cpp-python.md:763` | claim:tauri | passes the selected proxy configuration through the Tauri bridge; other proxy |
| `docs/rewrite-cpp-python.md:764` | claim:tauri | protocols remain on the compatibility path. The C++ service and Tauri library |
| `docs/rewrite-cpp-python.md:772` | claim:tauri | deadline. The Tauri bridge forwards the selected route as configuration data |
| `docs/rewrite-cpp-python.md:793` | claim:tauri | already emitted by Tauri. It now accepts either a confirmed host-key pin or a |
| `docs/rewrite-cpp-python.md:818` | claim:tauri | The Tauri host remains responsible for UI IPC, local credentials, approvals, |
| `docs/rewrite-cpp-python.md:846` | deleted-path:crates/ | `native/desktop_host/src/host_trust.cpp` store that reproduces the `crates/host-trust` |
| `docs/rewrite-cpp-python.md:856` | claim:tauri | On 2026-10-15, the Rust/Tauri desktop host was removed at the user's explicit |
| `docs/rewrite-cpp-python.md:857` | deleted-path:crates/, deleted-path:src-tauri, deleted-path:apps/desktop/src-tauri, claim:tauri | request, before parity was reached. `crates/`, `apps/desktop/src-tauri/`, |
| `docs/rewrite-cpp-python.md:859` | deleted-path:apps/agent | agent (`apps/agent/`) and its smoke were deleted; the React UI talks only to the |
| `docs/rewrite-cpp-python.md:860` | claim:tauri | C++ host through `window.__YUKINAL_HOST__`, with no Tauri fallback. `pnpm check` |
| `docs/rewrite-cpp-python.md:1543` | claim:tauri | Tauri pipeline left `scripts/package.mjs`, the installer smoke scripts and |
| `docs/rewrite-cpp-python.md:1544` | claim:tauri | `.github/workflows/package.yml` behind when Rust/Tauri was deleted, so the repository |
| `docs/rust-removal-plan.md:3` | claim:tauri | 这份文档把 C++ 接管拆成可勾选的清单，并记录删除 Rust/Tauri 时的清理范围。 |
| `docs/rust-removal-plan.md:5` | deleted-path:apps/agent, deleted-path:crates/, deleted-path:src-tauri, deleted-path:apps/desktop/src-tauri, claim:tauri | > **状态：Rust/Tauri 已删除。** `crates/`、`apps/desktop/src-tauri/`、`Cargo.toml`/`Cargo.lock`/`rust-toolchain.toml`/`.cargo/`，以及已退休的 legacy Node agent（`apps/agent/`）都已 |
| `docs/rust-removal-plan.md:9` | deleted-path:crates/, deleted-path:src-tauri, deleted-path:apps/desktop/src-tauri, claim:tauri | - 桌面 UI 原有 **81 个 Tauri 命令**（`crates/core` + `apps/desktop/src-tauri`），其中 C++ 宿主已实现绝大部分（逐条清单见 第 8 节），其余仍返回 `NOT_IMPLEMENTED`。 |
| `docs/rust-removal-plan.md:10` | claim:tauri | - `pnpm package` 曾走 `tauri build`；现在由 `scripts/package.mjs` 暂存 C++ 宿主的**分发布局 + `SHA256SUMS.txt` + `release-manifest.json`**，并用 `pnpm package -- --verify` 复核完整性； |
| `docs/rust-removal-plan.md:12` | claim:rust-emits | - 原本由 Rust 承载的共享能力（36 段 SQLite 迁移权威副本、OS 凭据边界、出站代理、host-trust、MCP、Python Agent sidecar 启动与监管）需要按下面顺序在 C++/Python 中重新提供。 |
| `docs/rust-removal-plan.md:37` | claim:mirrors | - `agent_run_start`、`agent_run_stop`、`agent_approval_respond`（转发到 Python `agent.run.*`/`agent.approval.respond`；payload 原样透传，由 Python Agent Node 负责持久受理、停止与一次性审批 |
| `docs/rust-removal-plan.md:50` | deleted-path:crates/ | - ✅ `server_host_key_status`、`server_host_key_forget`（已知主机存储已在 `native/desktop_host/src/host_trust.cpp` 实现；`v1:host:port:SHA256:...` 格式与 `crates/host-trust` 一致） |
| `docs/rust-removal-plan.md:71` | deleted-path:src-tauri, deleted-path:apps/desktop/src-tauri, claim:tauri | `apps/desktop/src-tauri/src/commands/provider.rs`、`commands/server/mod.rs`、`commands/agent_run.rs`、`commands/investigation.rs` 里的其余命令，实际逻辑在 `agent/src/yukinal_a |
| `docs/rust-removal-plan.md:99` | deleted-path:crates/ | **已接通首个里程碑。** 参考实现是 `crates/core/src/sidecar/`（约 1100 行）： |
| `docs/rust-removal-plan.md:101` | deleted-path:crates/ | - `crates/core/src/sidecar/config.rs`：解析 Python/PyInstaller 入口、环境白名单、`YUKINAL_DATA_DIR`。 |
| `docs/rust-removal-plan.md:102` | deleted-path:crates/ | - `crates/core/src/sidecar/mod.rs`：`Command` 启动、`env_clear` + 最小环境、stdout/stderr 读取任务、NDJSON 帧（上限 24 MiB）、请求/响应按 `id` 关联、`initialize` 握手、退出与重启监管。 |
| `docs/rust-removal-plan.md:103` | deleted-path:src-tauri, deleted-path:apps/desktop/src-tauri, claim:tauri | - `apps/desktop/src-tauri/src/commands/host/`：host RPC 服务端（`host.credentials.set/get/reclaim`、`host.mcp.catalog`、`host.tool.execute`、`host.investigation.*`、审批 t |
| `docs/rust-removal-plan.md:118` | claim:tauri | 7. **删除 Rust/Tauri**（见 第 7 节），并同步清理门禁/打包/CI/文档。 |
| `docs/rust-removal-plan.md:128` | deleted-path:src-tauri, deleted-path:apps/desktop/src-tauri, claim:tauri | \| `apps/desktop/src-tauri` \| 命令迁完后整体删除；React UI 由 C++ WebView 宿主承载 \| |
| `docs/rust-removal-plan.md:129` | deleted-path:crates/, deleted-path:crates/database, claim:mirrors | \| `crates/database` \| C++ `desktop_host/src/database.cpp` + `schema_migrations.hpp`（已镜像 36 段迁移）；删前做一次迁移兼容核对 \| |
| `docs/rust-removal-plan.md:130` | deleted-path:crates/ | \| `crates/credentials` \| C++ `desktop_host/src/credential_store.cpp`（已实现接口 + OS 后端） \| |
| `docs/rust-removal-plan.md:131` | deleted-path:crates/ | \| `crates/net` \| C++ `desktop_host/src/network_proxy.cpp` + Python `provider_store.py`/`models.py`（代理解析已有） \| |
| `docs/rust-removal-plan.md:132` | deleted-path:crates/ | \| `crates/host-trust` \| 待移植进 C++ 宿主（known_hosts v1 格式） \| |
| `docs/rust-removal-plan.md:133` | deleted-path:crates/ | \| `crates/core`（sidecar/MCP/工具/supervisor） \| sidecar 监管与 host RPC 由 C++ 宿主接管；MCP 配置/生命周期由 C++/Python 分工 \| |
| `docs/rust-removal-plan.md:134` | deleted-path:crates/ | \| `crates/filesystem`、`crates/terminal` \| 由 C++ SFTP/PTY 服务承接 \| |
| `docs/rust-removal-plan.md:135` | deleted-path:crates/ | \| `crates/time` \| 进入 C++/Python 各自需要处 \| |
| `docs/rust-removal-plan.md:143` | deleted-path:crates/, deleted-path:src-tauri, deleted-path:apps/desktop/src-tauri, claim:tauri | 1. 删除：`crates/`、`apps/desktop/src-tauri/`、`Cargo.toml`、`Cargo.lock`、`rust-toolchain.toml`、`.cargo/`。 |
| `docs/rust-removal-plan.md:144` | claim:tauri | 2. ✅ `package.json`：`package`/`release:manifest` 已改为 `scripts/package.mjs`（C++ 宿主分发布局 + 完整性清单）；`test` 里的 Tauri 引用已随 Rust 删除一并移除。 |
| `docs/rust-removal-plan.md:147` | claim:tauri | 5. `scripts/lib/tauri-agent-command.mjs` 及其测试：删除或替换。 |
| `docs/rust-removal-plan.md:150` | deleted-path:src-tauri, claim:tauri | 8. 跑 `pnpm check` 与三平台安装验收，确认没有对 `cargo`/`src-tauri` 的悬挂引用。 |
| `docs/release.md:3` | claim:tauri | > **状态更新（2026-10-15）：Rust/Tauri 宿主与 legacy Node agent 已删除，安装包/打包尚未实现。** 本页下方关于 Tauri、cargo、NSIS/MSI/DMG/deb、release manifest 与随包 Node runtime 的记录都是**历史证据**，不再描述 |
| `docs/release.md:13` | claim:tauri | C++ 宿主已接替旧的 Tauri 打包路径：`scripts/package.mjs` 把 C++ 桌面宿主、已构建的桌面 UI（默认 `apps/desktop/dist`，可用 `--ui-dir` 或 `YUKINAL_DESKTOP_UI_DIR` 覆盖）、冻结 Python Agent（PyInstalle |
| `docs/release.md:15` | claim:tauri | **仍未实现**：NSIS/MSI/DMG/deb 安装器、升级与用户数据保留、代码签名与公证、三平台真实 UI 冒烟，以及 `package.yml` workflow（仓库目前只有 `check.yml`）。因此本页下方的 Tauri/cargo/Node-runtime 记录依旧只是**历史证据**，新的 C++ |
| `docs/release.md:21` | claim:tauri | 2026-10-15 起的当前源码配置已把安装包资源切换为冻结 Python Agent、C++ SSH 服务和 `NOTICE`，移除了 Node Agent 与 Node runtime 的发行资源及下载步骤。安装版 smoke 会从 Tauri 资源映射暂存文件，在受限 PATH 下启动 Python Agent |
| `docs/release.md:30` | claim:tauri | \| Rust 依赖审计 \| 2026-09-30 本机以 `cargo-audit 0.22.2` 扫描 628 项依赖，未发现已知漏洞；仍有 2 条允许提示：[`glib 0.18.5` 的 `VariantStrIter` unsound 告警](https://rustsec.org/advisories/RUS |
| `docs/release.md:33` | claim:tauri | \| 旧版 Node.js 随包运行时（历史记录） \| 2026-09-30 的 MSI、NSIS、Linux `.deb` 和 macOS DMG 验收使用固定版 Node 24.21.0，记录见下方远端 CI 快照与旧产物记录。当前源码配置已不再把该 runtime 或旧 Node Agent 放进安装包 \| 新 P |
| `docs/release.md:34` | deleted-path:crates/, claim:tauri | \| 真实环境与协议回归 \| 本机 acceptance fixture 通过真实 AgentLoop 与 Host RPC client 覆盖只读诊断、审批变更、验证失败，以及文件恢复被拒绝或批准后的 revision/原内容核验；新增 native Host RPC + 真实临时 SQLite 测试，记录 evide |
| `docs/release.md:41` | claim:tauri | 卸载风险记录（2026-09-29 起）：一次 Tauri 2.12 NSIS 本机卸载曾删除 `%LOCALAPPDATA%\dev.yukinal.workspace`，确认页和复选框状态没有留档，原触发状态仍无法追溯。Tauri 生成的 `Section Uninstall` 原生支持勾选“Delete the |
| `docs/release.md:45` | deleted-path:crates/, claim:tauri | Rust 依赖边界复核（2026-09-30）：`cargo tree -i glib@0.18.5 --target x86_64-unknown-linux-gnu` 将 `glib 0.18.5` 追溯到 Tauri 的 GTK 0.18 / WebKitGTK 栈；同一查询对 Windows 目标没有依赖输出， |
| `docs/release.md:47` | claim:tauri | 此前 5 条 `unic-*` 未维护告警均来自 `urlpattern 0.3.0 -> tauri-utils 2.9.3`；现已锁定 Tauri `2.12.0` / `tauri-utils 2.10.0`，该版本改用 `urlpattern 0.6`，2026-09-29 与 2026-09-30 两次本机 |
| `docs/release.md:57` | claim:tauri | 以下是 2026-09-30 上一轮脏工作区生成的本地验收产物，不是签名版或可公开发布的 release。构建操作系统为 Windows x64；当时的打包脚本先通过严格模式的 sidecar 冒烟，再由 Tauri 生成两个安装器。使用 7-Zip 检查 NSIS 内容，并读取 MSI 的 `File` 表，确认都带 |
| `docs/release.md:59` | claim:tauri | 此前版本的 NSIS 曾以 per-user 模式在本机真实安装，按测试专用 `YUKINAL_DATA_DIR`、不含 Node 的子进程 `PATH`，通过 `pwsh -NoProfile -File scripts/smoke-installed-windows.ps1` 验证 sidecar 的实际运行文件与 |
| `docs/release.md:61` | claim:tauri | 之前构建的 MSI 曾通过 `msiexec /a` 提取管理员映像至 `target/release/msi-smoke-post-wnaf-20260929/`，使用 `-InstallDir` 运行 PATH 隔离、sidecar SHA-256、Agent 握手及进程退出检查；正常关窗 1 次、握手后强制终止父 |
| `docs/release.md:63` | claim:mirrors | 为真实 MSI 生命周期新增了 `scripts/smoke-msi-install-windows.ps1`。它会从 MSI 读取 ProductCode、版本和安装路径；非管理员、检测到 Node.js、已有 Yukinal 安装、数据目录已存在或安装目录占用时会在改动系统前拒绝运行。仅在一次性 Windows x |
| `docs/release.md:65` | claim:tauri | 2026-09-30 NSIS 构建的真实生命周期脚本为 `scripts/smoke-nsis-install-windows.ps1`。它只在两个 Yukinal 数据目录、产品/卸载注册、测试安装目录和默认 `%LOCALAPPDATA%\Yukinal` 安装目录均未占用时运行；静默安装到 `target/re |
| `docs/release.md:67` | claim:tauri | 卸载生成脚本回归补强：构建后检查验证确认页回调在卸载区段之前捕获复选框状态、preservation hook 位于两条且仅两条应用数据递归删除语句之前、两条语句都位于 Tauri 同意与非更新 guard 内；同时检查 hook 会清零状态、只显示说明而不含递归删除，并拒绝缺失状态捕获和额外无条件数据删除。Windo |
| `docs/release.md:122` | deleted-path:src-tauri, deleted-path:apps/desktop/src-tauri, claim:tauri | 已有 Agent fixture `pnpm --filter @yukinal/agent exec tsx --test src/runtime/investigation-acceptance.test.ts` 使用真实 AgentLoop、权限审批和 Host RPC client，但服务端和远端文件是状态化内 |
| `docs/acceptance-handoff.md:5` | deleted-path:apps/agent, deleted-path:crates/, deleted-path:src-tauri, deleted-path:apps/desktop/src-tauri, claim:tauri | > **变更提示（Rust/Tauri 已删除）**：`crates/`、`apps/desktop/src-tauri/`、`Cargo.*`、`rust-toolchain.toml`、`.cargo/` 和 legacy Node agent（`apps/agent/`）已从仓库移除；`pnpm check` 不 |
| `docs/acceptance-handoff.md:48` | deleted-path:apps/agent | apps/agent/                旧 Node Agent（已删除） |
| `docs/acceptance-handoff.md:50` | deleted-path:src-tauri, deleted-path:apps/desktop/src-tauri, claim:tauri | apps/desktop/src-tauri/    Tauri/Rust 旧宿主（已删除） |
| `docs/acceptance-handoff.md:51` | deleted-path:crates/ | crates/                    Rust：core / database / net / credentials / host-trust 等（已删除） |
| `docs/acceptance-handoff.md:161` | deleted-path:crates/ | `crates/core/tests/sidecar_agent.rs::real_sidecar_runs_a_durable_read_through_provider_and_host` |
| `docs/acceptance-handoff.md:197` | claim:mirrors | 方式 A（推荐，隔离临时容器；测试结束后自动清理镜像/容器）： |
| `docs/acceptance-handoff.md:215` | claim:mirrors | 要覆盖的用例：只读操作成功、错误凭据、命令超时、输出上限、未知 host key、变更 host key。请把容器镜像、OpenSSH 版本、通过/失败的输入组合记进 `docs/rewrite-cpp-python.md`。 |
| `docs/acceptance-handoff.md:222` | deleted-path:apps/agent | `apps/agent/live/provider-live.test.ts`。它测的不是 Python LangChain 路径。 |
| `docs/acceptance-handoff.md:241` | deleted-path:crates/ | - 或照 `crates/core/tests/sidecar_agent.rs` 的回环测试，新增一个用真实 `baseUrl`/key 的 gated 测试。 |
| `docs/acceptance-handoff.md:247` | deleted-path:crates/ | - **调查任务的证据/产物持久化**（`host.investigation.evidence.record`、`plan.step_result`、`artifact.record`）在 Python 运行时没有实现；旧 Node Agent 有。`crates/core/tests/sidecar_agent.r |
| `docs/acceptance-handoff.md:324` | claim:tauri | 未迁入 C++ 的命令返回 `NOT_IMPLEMENTED`，不会回退 Tauri（Tauri 宿主已删除）。剩余迁移顺序见 `docs/cpp-desktop-handoff.md`。 |
| `docs/acceptance-handoff.md:326` | claim:tauri | 桌面宿主开发启动：`pnpm desktop:dev:cpp`（Tauri + Python Agent 的 `desktop:dev:python` 已随 Rust/Tauri 删除而移除）。 |
| `docs/handover.md:148` | deleted-path:crates/ | 参考：`45fef20^:crates/core/src/mcp/http.rs`、`.../handle.rs`、`.../catalog.rs`、 |
| `docs/handover.md:149` | deleted-path:src-tauri, deleted-path:apps/desktop/src-tauri, claim:tauri | `45fef20^:apps/desktop/src-tauri/src/commands/mcp/oauth.rs`、`.../oauth/flow.rs`、 |
| `docs/handover.md:178` | deleted-path:src-tauri, deleted-path:apps/desktop/src-tauri, claim:tauri | 参考：`45fef20^:apps/desktop/src-tauri/src/commands/investigation.rs`（1230 行）、 |
| `docs/handover.md:209` | deleted-path:src-tauri, deleted-path:apps/desktop/src-tauri, claim:tauri | 参考：`45fef20^:apps/desktop/src-tauri/src/commands/files/transfer.rs`（传输管理器）、 |
| `docs/handover.md:238` | deleted-path:src-tauri, deleted-path:apps/desktop/src-tauri, claim:tauri | 目标解析与完成记账。参考 `45fef20^:apps/desktop/src-tauri/src/commands/host.rs` |
| `docs/handover.md:251` | deleted-path:src-tauri, deleted-path:apps/desktop/src-tauri, claim:tauri | 参考：`45fef20^:apps/desktop/src-tauri/src/commands/host/plan.rs`（1766 行）、`evidence.rs` |
| `docs/handover.md:269` | claim:tauri | - **已完成（分发布局与完整性清单）**：`scripts/package.mjs` 取代旧的 Tauri 打包路径，把 C++ 桌面宿主、 |
| `docs/handover.md:311` | claim:mirrors | 是宿主对自己内建工具的自己声明、本租户的声明、闭集默认 `high`；宿主两组内建工具被逐一镜像（计划绑定 |
| `docs/handover.md:380` | claim:tauri | 6. **不要回退到 Rust/Tauri**：Rust 宿主已删除；未迁移命令返回 `NOT_IMPLEMENTED` 是正确的， |
| `packages/shared/src/schemas/event-vocabulary.test.ts:6` | claim:rust-emits | * The overlap is real and intentional — Rust emits `server.updated` and |
| `packages/shared/src/schemas/event-vocabulary.test.ts:23` | claim:tauri | import { tauriEventName } from "../events/index.js"; |
| `packages/shared/src/schemas/event-vocabulary.test.ts:82` | claim:tauri | test("every declared name maps to a distinct Tauri channel", () => { |
| `packages/shared/src/schemas/event-vocabulary.test.ts:83` | claim:tauri | // `tauriEventName` is a plain `.` -> `:` substitution, so two logical names that |
| `packages/shared/src/schemas/event-vocabulary.test.ts:85` | claim:tauri | const channels = EVENT_NAMES.map(tauriEventName); |
| `packages/shared/src/schemas/event-vocabulary.test.ts:91` | deleted-path:apps/agent, deleted-path:agent-loop.ts | // The producer side, transcribed from `apps/agent/src/agent-loop.ts`. If the loop |
| `packages/shared/src/schemas/event-vocabulary.test.ts:134` | claim:rust-emits | // The channel name *is* the payload's discriminator on the Rust side |
| `packages/shared/src/schemas/event-vocabulary.test.ts:135` | claim:tauri | // (`commands/mod.rs` emits on `tauri_event_name(params.type)`), so a mismatched pair |
| `docs/changelog.md:7` | claim:tauri | - **文档与仓库卫生整理（三轮审计）**：第一轮清理仓库根部误留的构建/验证 `.txt` 临时记录（`build*`/`ctest*`/`pnpm*`/`pub*`/`push*`/`commit*`/`test_out.txt`），并在 `.gitignore` 中按根目录锚定忽略，避免再次误提交。第二轮修正仍指 |
| `docs/changelog.md:9` | claim:mirrors | - **控制平面按租户策略**：审批要求与有效期不再是全服务单值，而是每个租户自己的策略，用的正是共享风险层的词汇（`packages/shared/src/types/risk.ts`）。控制平面把两个问题分开：kind **是什么**（风险）与**发生什么**（该权限档的模式）。风险来源按优先级是宿主对自己内建工具 |
| `docs/changelog.md:23` | deleted-path:apps/agent, deleted-path:crates/, deleted-path:src-tauri, deleted-path:apps/desktop/src-tauri, claim:tauri | - **删除整个 Rust/Tauri 桌面宿主（按用户要求）**：移除 `crates/`、`apps/desktop/src-tauri/`、`Cargo.toml`/`Cargo.lock`/`rust-toolchain.toml`/`.cargo/`，以及已退休的 legacy Node agent（`app |
| `docs/changelog.md:37` | claim:tauri | - 将固定版本且按 SHA-256 校验的 Node.js 运行时纳入桌面安装包；本机 Windows NSIS/MSI 构建成功，当前 MSI 管理员映像载荷启动/关闭冒烟通过。针对早先 NSIS 卸载曾删除数据目录的风险，明确维持产品规则：卸载永远保留数据库、设置和凭据；即使 Tauri 确认页的删数复选框被勾选， |
| `docs/changelog.md:38` | claim:tauri | - 补强 NSIS 卸载回归保护：静态检查锁定 hook 存在、用户勾选后的说明提示、复选框状态清零、Roaming/LocalAppData 两条且仅两条删除语句仍受保护 hook 与非更新条件约束，并拒绝缺失状态捕获和额外无条件删除。Windows UI smoke 从 Tauri 生成脚本提取真实回调，操作原生 |
| `docs/changelog.md:40` | claim:tauri | - 修正两个 macOS Node.js `.tar.gz` 校验值与归档名错配的问题；打包前新增六平台官方 SHASUMS 全量校验，当前六个 pin 均已核对通过，Windows 打包前置流程与 Tauri release 编译（`--no-bundle`）通过。 |
| `docs/changelog.md:52` | claim:tauri | - 建立 Tauri/Rust 宿主、React 工作区、Node.js Agent sidecar 与共享 IPC 契约的版本基线。 |
| `docs/cpp-desktop-handoff.md:13` | claim:tauri | Rust/Tauri 宿主与 legacy Node agent 已按 `rust-removal-plan.md` 从仓库删除；C++ WebView 宿主是唯一桌面宿主，`pnpm check` 不再依赖 cargo。因此 C++ 宿主目前是**唯一载体**：尚未迁移的命令返回 `NOT_IMPLEMENTED`， |
| `docs/cpp-desktop-handoff.md:40` | deleted-path:src-tauri, deleted-path:apps/desktop/src-tauri, claim:tauri | \| `apps/desktop/src-tauri/` \| **已删除**（Rust/Tauri 旧宿主） \| |
| `docs/cpp-desktop-handoff.md:44` | claim:tauri | 当前 IPC router 已处理 `core_ping`、`shell.open_url`、`server_list`、`workspace_list`、`server_add`、`server_update`、`server_delete`、`activity_list`、`tool_execution_list` |
| `docs/cpp-desktop-handoff.md:69` | claim:tauri | `pnpm package`（`scripts/package.mjs`）不再走 `tauri build`，而是把仓库已经产出的四件套暂存成可分发布局： |
| `docs/cpp-desktop-handoff.md:104` | claim:tauri | 命令级盘点、依赖顺序与删除清理清单见 [`rust-removal-plan.md`](../rust-removal-plan.md)；Rust/Tauri 已按该计划删除，本文件剩下的是从零实现 C++ 宿主能力的收尾项。 |
| `docs/cpp-desktop-handoff.md:106` | claim:tauri | - React 的现有外观和交互由 C++ WebView 宿主承载；所有 UI 命令都由 C++ 实现或明确标记为尚未迁移，不再依赖 Tauri/Rust 执行。 |
| `docs/cpp-desktop-handoff.md:110` | claim:tauri | - 三平台 C++ 宿主和安装器完成构建、升级、用户数据保留及实际 UI 冒烟验收；Rust/Tauri 旧实现已在 `45fef20` 删除，参考实现留在 git 历史里。 |
| `packages/shared/src/events/index.ts:5` | claim:tauri | * Rust -> UI events travel over Tauri's event system; agent -> UI events are |
| `packages/shared/src/events/index.ts:8` | claim:tauri | * These are logical names: `tauriEventName` below is the single place that turns |
| `packages/shared/src/events/index.ts:9` | claim:tauri | * them into the channel Tauri actually accepts. |
| `packages/shared/src/events/index.ts:47` | claim:tauri | * Convert a logical event name to the channel accepted by Tauri. |
| `packages/shared/src/events/index.ts:50` | claim:tauri | * `agent.started`), while Tauri event channels only allow `:`, `/`, `_`, `-` |
| `packages/shared/src/events/index.ts:53` | claim:tauri | export function tauriEventName(name: string): string { |
| `packages/shared/src/schemas/ipc.ts:2` | claim:tauri | * Per-command runtime gates for the Tauri IPC contract (see `ipc/index.ts`). |
| `packages/shared/src/schemas/ipc.ts:155` | claim:rust-emits | * report (`skip_serializing_if` on the Rust side), so the status of an idle supervisor |
| `packages/shared/src/schemas/ipc.ts:182` | claim:asserted-both-sides | * pin (`skip_serializing_if`), and a `null` here would mean both sides disagree about |
| `packages/shared/src/schemas/ipc.ts:662` | claim:tauri | * reads `params.type` and emits on `tauri_event_name(event_type)` |
| `packages/shared/src/schemas/ipc.ts:663` | deleted-path:src-tauri, deleted-path:apps/desktop/src-tauri, claim:tauri | * (`apps/desktop/src-tauri/src/commands/mod.rs:470,504`). Nothing the transport can |
| `docs/native-desktop-host.md:8` | claim:tauri | MCP, and tool orchestration. The final desktop package must not contain a Tauri or |
| `docs/native-desktop-host.md:20` | claim:tauri | The Rust/Tauri desktop host and the legacy Node agent have been removed. The C++ |
| `docs/native-desktop-host.md:44` | claim:asserted-both-sides | The fused outer and AI surfaces use neutral gray values on both sides of this |
| `docs/native-desktop-host.md:180` | claim:tauri | `NOT_IMPLEMENTED`; they never invoke the Tauri host. |
| `packages/shared/src/events/index.test.ts:4` | claim:tauri | import { EVENT_NAMES, tauriEventName } from "./index.js"; |
| `packages/shared/src/events/index.test.ts:6` | claim:tauri | test("Tauri event channels contain only supported characters", () => { |
| `packages/shared/src/events/index.test.ts:8` | claim:tauri | const channel = tauriEventName(name); |
| `packages/shared/src/events/index.test.ts:12` | claim:tauri | assert.equal(tauriEventName("agent.started"), "agent:started"); |
| `apps/desktop/src/lib/mcp.ts:8` | claim:rust-emits | * 可连接性由 Rust 的 `McpHttpConfig` 决定，界面只负责把后端返回的理由照原样显示出来 |
| `apps/desktop/src/lib/mcp.ts:315` | claim:asserted-both-sides | * 输入框」，后端用它决定「要不要读凭据库」。两边都从这一个判断出发，所以不会出现「界面 |
| `apps/desktop/src/lib/mcp.ts:363` | deleted-path:crates/, claim:rust-emits | * 这些行由 Rust 侧截断并脱敏过（`crates/core/src/mcp/mod.rs`），界面再脱敏一次会让两边 |
| `apps/desktop/tests/panel-states.test.tsx:150` | claim:tauri | ["", "folder", "浏览器预览模式", "远程文件需要 Tauri 原生连接。"], |
| `apps/desktop/tests/panel-states.test.tsx:176` | claim:tauri | const withIcon = ["logs", "原生 SSH 能力只在 Tauri 桌面壳中可用，预览不会伪造远端日志数据。"] as const; |
| `apps/desktop/tests/panel-states.test.tsx:182` | claim:tauri | const noIcon = "动态记录需要 Tauri 桌面壳中的本地数据库。"; |
| `docs/architecture-and-build-plan.md:36` | deleted-path:src-tauri, deleted-path:apps/desktop/src-tauri, claim:tauri | \| `apps/desktop/src-tauri/` \| **已删除**（Rust/Tauri 旧宿主）；UI IPC、凭据读取、目标解析、审批和持久任务衔接现由 C++ WebView 宿主提供，且不再建立 Rust SSH/SFTP 会话 \| 在 C++ 宿主继续验证授权生命周期、跨目标拒绝、预算、撤销、未知结果 |
| `docs/architecture-and-build-plan.md:88` | claim:rust-emits | - **字节路径：**文件内容由 Rust 宿主在本机文件与 SFTP 之间分块流动。WebView 和 sidecar 只传句柄、元数据及进度事件，不承载整文件字节或绝对路径授权。预览调用保持独立的大小上限；大文件和二进制文件仍能完整传输。 |
| `docs/architecture-and-build-plan.md:98` | claim:mirrors | 每一步完成后提交可独立复核的代码、测试和文档差异；不要先重做外观再补底层。跨层字段从 `packages/shared` 契约开始，Rust 镜像、IPC fixture、Agent 和 UI 同步修改。既有用户数据需要迁移时补迁移与旧库启动回归。 |
| `packages/shared/src/schemas/agent.ts:88` | claim:rust-emits, claim:verified-at | * This is safe to narrow because of how the Rust side emits, verified at |
| `packages/shared/src/schemas/agent.ts:89` | deleted-path:src-tauri, deleted-path:apps/desktop/src-tauri, claim:tauri | * `apps/desktop/src-tauri/src/commands/mod.rs:470,504`: the Tauri channel name is read |
| `packages/shared/src/schemas/agent.ts:90` | claim:tauri | * from `params.type` and emitted as `tauri_event_name(event_type)`. The channel and the |
| `packages/shared/src/types/mcp.ts:16` | deleted-path:src-tauri, deleted-path:apps/desktop/src-tauri, claim:mirrors, claim:tauri | * Every shape here mirrors `apps/desktop/src-tauri/src/commands/mcp.rs` field for field. The Rust |
| `packages/shared/src/types/mcp.ts:32` | claim:mirrors | * One MCP tool as the server declared it (mirrors `McpToolDescriptor`). |
| `packages/shared/src/types/mcp.ts:74` | claim:mirrors | * Live status of one server (mirrors `McpServerStatus`). |
| `apps/desktop/src/features/agent/AgentHistoryPane.tsx:13` | deleted-path:crates/, deleted-path:crates/database | *   `crates/database/src/repositories/chat.rs` 的 `rename`）。 |
| `apps/desktop/src/features/agent/AgentHistoryPane.tsx:105` | claim:mirrors | * 行号表的镜像。异步回调（改名成功后）要按 id 找到那一行，但它注册时 `rowIndex` |
| `apps/desktop/src/features/overview/ServerOverview.tsx:116` | claim:mirrors | * 「原始数字变成健康等级的换算全项目只有一处，就是这里，外加 Rust 镜像 |
| `apps/desktop/src/features/overview/ServerOverview.tsx:117` | deleted-path:crates/ | * （crates/core/src/health.rs）—— UI 和 Agent 不允许各算各的」。 |
| `apps/desktop/src/features/servers/HostKeyGate.tsx:30` | claim:tauri | * （不依赖 Tauri、不依赖 react-query，也就不依赖测试环境里有没有 `window`）。 |
| `apps/desktop/src/features/servers/HostKeyGate.tsx:49` | claim:tauri | * - 没有目标、或不在 Tauri 壳里 → 放行（让面板显示它自己的空状态/预览态）； |
| `apps/desktop/src/lib/format.ts:17` | claim:tauri | * 刻意**不**依赖 React 与 Tauri，这样可以直接单测（见 tests/format.test.ts）。 |
| `apps/desktop/src/lib/format.ts:95` | claim:mirrors | *   镜像文件会显示成「5120.0 MB」—— 数值没说错，但没人能一眼读出量级，而这正是 |
| `apps/desktop/src/lib/markdown/inline.ts:307` | claim:asserted-both-sides | * `~~删除~~`。GFM 的删除线不在 CommonMark 里，也不进下面的强调配对：两边都要求正好 |
| `apps/desktop/src/lib/markdown/inline.ts:364` | claim:asserted-both-sides | /** 还没被吃掉的标记个数：`***x***` 配对两次之后两边都会降到 0。 */ |
| `docs/desktop-window-direction.md:23` | claim:mirrors | 已检测到 Docker 的服务器可展开所属容器；容器继续展开镜像、运行状态和重启次数。读取复用现有快照缓存，只在展开且服务器已连接、主机密钥已信任时请求；断线、空数据和失败均保留实际状态。PVE 未检测时不显示能力。 |
| `docs/desktop-window-direction.md:73` | claim:mirrors | 本轮按批注调整后，使用用户已打开的隔离工作树桌面程序完成实际窗口检查，未再次发起进程关闭或启动。下图为 1442 × 921 的原始桌面窗口截图：纯文字字标、服务器与容器父子层级、简化输入栏，以及最新记录／定时任务摘要均已显示；容器可展开实际镜像、运行状态和重启次数。截图右下的蓝色光圈属于系统指针，输入框本身没有发光样 |
| `packages/shared/src/ipc/index.ts:2` | claim:tauri | * Tauri IPC contract shared by React and the Rust command layer. |
| `packages/shared/src/ipc/index.ts:4` | claim:rust-emits | * React may only call these commands and listen to these events. Rust mirrors the |
| `packages/shared/src/protocol/jsonrpc.ts:2` | deleted-path:apps/agent | * Sidecar protocol: React/UI  <->  apps/agent (standalone Node process, ADR 0001). |
| `packages/shared/src/protocol/jsonrpc.ts:4` | claim:tauri | * Transport: newline-delimited JSON-RPC 2.0 over the Tauri sidecar's stdio |
| `packages/shared/src/types/host.ts:226` | deleted-path:src-tauri, deleted-path:apps/desktop/src-tauri, claim:tauri | * `apps/desktop/src-tauri/src/commands/mcp.rs`; a Rust test asserts the two lists agree |
| `packages/shared/src/types/host.ts:303` | deleted-path:crates/ | * (`crates/core/src/mcp/descriptor.rs`, `#[serde(tag = "type")]`). |
| `packages/shared/src/types/provider.ts:206` | claim:mirrors | * How the desktop obtains the first token bundle (mirrors `McpOAuthFlow`). |
| `packages/shared/src/types/provider.ts:214` | claim:mirrors | * Client authentication at the token endpoint (mirrors `McpOAuthClientAuth`). |
| `scripts/package.mjs:3` | claim:tauri | * C++ desktop host packaging pipeline (the replacement for the retired Tauri |
| `scripts/package.mjs:4` | claim:tauri | * `tauri build` path). |
| `CONTRIBUTING.md:3` | claim:tauri | 感谢你愿意参与 Yukinal。桌面宿主已是 **C++ WebView 宿主**：Rust/Tauri 宿主与 legacy Node agent 已按 `docs/rust-removal-plan.md → ./docs/rust-removal-plan.md` 从仓库删除，`pnpm check` 不再依赖 |
| `README.md:28` | claim:tauri | > The Rust/Tauri host and the legacy Node agent have been removed; the C++ |
| `README.zh-CN.md:27` | claim:tauri | > Rust/Tauri 宿主与 legacy Node agent |
| `apps/desktop/src/components/PanelStates.tsx:124` | claim:tauri | * 原生能力只在 Tauri 壳里可用，而预览**不伪造**远端数据。每个页面的正文 |
| `apps/desktop/src/features/agent/history.ts:4` | claim:tauri | * 全部是纯函数 —— 不依赖 React、不依赖 Tauri、不读当前时间（`now` 由调用方传入）， |
| `apps/desktop/src/features/agent/run-lifecycle.ts:1` | claim:tauri | /** Tauri can deliver started/completed before the start command resolves. */ |
| `apps/desktop/src/features/agent/transcript.ts:4` | claim:tauri | * 这里刻意不依赖 React 与 Tauri。让动态可读的那些顺序规则 —— 历史条数上限、 |
| `apps/desktop/src/features/agent/useAgentRun.ts:2` | claim:tauri | * 一次 Agent 运行的全部机制：Tauri 事件流、run 生命周期握手、审批簿记。 |
| `apps/desktop/src/features/servers/HostKeySection.tsx:9` | claim:tauri | * Tauri 壳与一台 SSH 服务器。这个功能里最要紧的几件事（两个指纹都显示出来、没有「忽略」 |
| `apps/desktop/src/features/servers/ServerTreeItem.tsx:49` | claim:mirrors | <div className="server-container-detail"><strong>{container.name}</strong><span title="镜像">{container.image}</span><span>{container.status}{container.restartCou |
| `apps/desktop/src/features/servers/server-form.ts:38` | claim:rust-emits | * 口令的提交规则，与 Rust 侧「空 / 纯空白 = 没有口令」对齐。 |
| `apps/desktop/src/features/settings/McpOAuthDeviceCodeDialog.tsx:6` | claim:rust-emits | * 授权是在 Rust 侧进行的，而且**还没结束**：`mcp_oauth_connect` 正在等用户到浏览器里确认。 |
| `apps/desktop/src/features/settings/McpSettings.tsx:8` | claim:tauri | * `renderToStaticMarkup` 钉住，而不需要造一个 Tauri 壳和一台真的 MCP 服务器。 |
| `apps/desktop/src/features/settings/RuntimeSettings.tsx:289` | claim:rust-emits | // `HttpBaseUrlSchema` 两条都管，而且和 Rust 侧、和 IPC 层用的是同一份定义。 |
| `apps/desktop/src/lib/ipc.ts:219` | claim:tauri | * `listenDesktop` is async — Tauri hands back the unlisten function in a promise — |
| `apps/desktop/src/lib/providers.ts:41` | claim:rust-emits | * 没有歧义 —— 协议本身就是那家服务商的。同一份事实在 Rust 侧是 |
| `apps/desktop/tests/agent-history.test.tsx:104` | claim:tauri | // 这条按源码断言，理由和上面一样：行只在有数据时才渲染，而这个视图的数据来自 Tauri。 |
| `apps/desktop/tests/host-key-gate.test.ts:5` | claim:tauri | * 是这里唯一会出安全问题的地方，而把它留在组件里写成一串 `&&` 就只能靠一个带 Tauri 壳 |
| `apps/desktop/tests/host-mock.ts:5` | claim:tauri | * fake for it instead of mocking the retired Tauri IPC layer. |
| `apps/desktop/tests/markdown-spec.test.ts:22` | claim:asserted-both-sides | * whitespace collapsed on both sides (the spec's `<br />` and our paragraph breaks make exact |
| `apps/desktop/tests/markdown.test.ts:471` | claim:asserted-both-sides | // 「3 的倍数」规则：`*foo**bar*` 里的 `**` 两边都贴边，配不上，就是两个字面星号。 |
| `apps/desktop/tests/run-policy.test.ts:67` | claim:tauri | // Tauri serialises the args as JSON, so an `undefined` field really does leave the |
| `docs/README.md:12` | claim:tauri | \| 规划 C++ 接管与删除 Rust/Tauri \| [`C++ 接管与 Rust 移除计划`](../rust-removal-plan.md) \| |
| `native/desktop_host/src/schema_migrations.hpp:3` | deleted-path:crates/, deleted-path:crates/database | // Ordered SQL port of crates/database/src/schema.rs. Append new migrations only; |
| `packages/agent-sdk/src/transport.ts:4` | claim:rust-emits, claim:tauri | * The client never touches child_process or WebSocket directly: Tauri/Rust owns |
| `packages/provider-sdk/src/types.ts:23` | claim:asserted-both-sides | * 而两侧都是纯类型、编译器不会因为它们"不相等"报错 —— 只要每个使用点各自能解析， |
| `packages/shared/fixtures/ipc/mcp_server_list.json:9` | deleted-path:crates/ | "args": ["crates/core/tests/fixtures/mcp-server.js", "ok"], |
| `packages/shared/fixtures/ipc/mcp_server_review.json:7` | deleted-path:crates/ | "args": ["crates/core/tests/fixtures/mcp-server.js", "ok"], |
| `packages/shared/fixtures/ipc/mcp_server_start.json:7` | deleted-path:crates/ | "args": ["crates/core/tests/fixtures/mcp-server.js", "ok"], |
| `packages/shared/src/index.ts:6` | claim:rust-emits | * - Types are the contract; zod schemas are the runtime gate; Rust mirrors these |
| `packages/shared/src/schemas/consistency.ts:139` | claim:rust-emits | * wire and Rust emits them, the UI just has no subscriber yet — but a *gate* with no |
| `packages/shared/src/schemas/host.ts:264` | deleted-path:crates/ | * Mirrors `McpContentBlock` (`crates/core/src/mcp/descriptor.rs`). A `strictObject` per |
| `packages/shared/src/schemas/ipc.test.ts:3` | deleted-path:crates/, deleted-path:crates/core/src/ipc.rs | * in `crates/core/src/ipc.rs` and asserts the *same* fixtures against serde output). |
| `packages/shared/src/schemas/mcp.ts:28` | deleted-path:src-tauri, deleted-path:apps/desktop/src-tauri, claim:mirrors, claim:tauri | * These mirror `apps/desktop/src-tauri/src/commands/mcp.rs`. They are strict on the fields the UI |
| `packages/shared/src/schemas/permission.ts:397` | deleted-path:apps/agent, deleted-path:policy-registry.ts | * which ids exist (`apps/agent/src/permissions/policy-registry.ts`), and it answers an |
| `packages/shared/src/schemas/provider.ts:113` | claim:rust-emits | * Rust 侧对应地只对 `openai-compatible` 序列化这个字段（`commands/provider.rs` 的 |
| `packages/shared/src/schemas/server.test.ts:103` | claim:rust-emits | // 空串不是「空口令」：没填就该省略字段（`min(1)` 与 Rust 侧一致）。 |
| `packages/shared/src/schemas/server.ts:145` | claim:rust-emits | * 表单必须省略字段而不是送一个 `""`（Rust 侧 `min(1).max(4_096)` 与之对齐）。 |
| `packages/shared/src/types/chat.ts:4` | deleted-path:apps/agent | * These types are the vocabulary between: React <-> agent-sdk <-> apps/agent <-> LLM. |
| `packages/shared/src/types/errors.test.ts:7` | deleted-path:crates/, deleted-path:crates/database | * side (`crates/database/tests/error_taxonomy.rs`) asserts its own enum against |
| `packages/shared/src/types/errors.ts:18` | deleted-path:crates/, deleted-path:crates/database, claim:asserted-both-sides | * and is asserted by both this package and `crates/database` so the Rust host |
| `packages/shared/src/types/health.ts:9` | deleted-path:crates/ | * mirror (`crates/core/src/health.rs`) — UI and Agent never compute it differently. |
| `packages/shared/src/types/tool.ts:81` | deleted-path:apps/agent, deleted-path:tools/registry.ts | * `apps/agent` returns these as plain literals (`tools/registry.ts`), which the compiler |
| `scripts/check-publication.mjs:20` | deleted-path:crates/ | * `crates/filesystem/src/revision.rs` 里手写的 SHA-256 常量正是靠它才能被验证。 |
| `scripts/smoke-python-dev-agent.mjs:3` | claim:tauri | /** Exercise the source Python Agent through the same NDJSON sidecar contract as Tauri. */ |

### 七.1 最该先改的 5 处（都属于"让契约诚实"，见 §八）

1. `packages/shared/src/schemas/event-vocabulary.test.ts:5-13` —— 断言「Rust emits `server.updated` and `terminal.opened`」，而 `server.updated` 在 `native/`+`agent/` 零命中。
2. `packages/shared/src/schemas/event-vocabulary.test.ts:90-112` —— 注解声称"transcribed from `apps/agent/src/agent-loop.ts`"，该文件不存在。
3. `packages/shared/src/schemas/agent.ts:88-90` + `schemas/ipc.ts:662-663` —— 用「`apps/desktop/src-tauri/src/commands/mod.rs:470,504`」作为窄化类型的安全依据。
4. `packages/shared/src/types/mcp.ts:16`、`schemas/mcp.ts:28`、`types/host.ts:226,303`、`schemas/host.ts:264` —— 「mirrors `apps/desktop/src-tauri/src/commands/mcp.rs` field for field」「mirrors `crates/core/src/mcp/descriptor.rs`」。
5. `apps/desktop/src/lib/mcp.ts:363` + `features/overview/ServerOverview.tsx:116-117` + `features/servers/server-form.ts:38` —— 「由 Rust 侧截断并脱敏过（`crates/core/src/mcp/mod.rs`）」等，是**安全论证**建立在已删除的代码上。

### 七.2 关于 docs/ 的说明

`docs/` 下 9 个文件贡献了最多的命中（`rewrite-cpp-python.md` 29、`rust-removal-plan.md` 25、`release.md` 17、`acceptance-handoff.md` 12、`handover.md` 9、`changelog.md` 7、`cpp-desktop-handoff.md` 7、`native-desktop-host.md` 4、`architecture-and-build-plan.md` 3）。其中 `rewrite-cpp-python.md` 开头有「**2026-10-17 status:** the Rust/Tauri host has been deleted … read them as the state at that time」横幅，属于**有意的历史存档**，不应按"谎言"处理；其余多为无横幅的历史叙述。`README.md:28`、`README.zh-CN.md:27`、`CONTRIBUTING.md:3` 是**正确的现状描述**（"has been removed"），仅因出现 "Tauri" 一词被命中，属于**误报**。

---

## 八、修复建议（两类，各带成本量级）

> 成本量级定义：**S** = 纯注释/文案，≤20 行且不触碰运行时（半天内含验证）；**M** = 触及 schema 或单个处理器，需重跑相关单测（1-2 天）；**L** = 跨 C++/Python/TS 三方或需新增状态机（≥3 天，需单独 ADR）。

### 8.1 「让契约诚实」（删/改断言，不改行为）

| # | 动作 | 位置 | 成本 |
| --- | --- | --- | --- |
| A1 | 改掉"Rust emits/由 Rust 侧决定"的**因果依据**，改述为"两侧由 fixture 对齐" | `schemas/ipc.ts:155,182,662-663`、`schemas/agent.ts:88-90`、`schemas/consistency.ts:139`、`schemas/index.ts:6`、`ipc/index.ts:2,4`、`agent-sdk/src/transport.ts:4` | **S**（~12 行） |
| A2 | 删/改"mirrors `crates/`、`src-tauri`"的镜像断言（这些路径不存在，镜像无从谈起） | `types/mcp.ts:16,32,74`、`schemas/mcp.ts:28`、`types/host.ts:226,303`、`schemas/host.ts:264`、`types/health.ts:9`、`types/errors.ts:18`、`types/errors.test.ts:7`、`schemas/ipc.test.ts:3`、`types/chat.ts:4`、`types/tool.ts:81`、`schemas/permission.ts:397` | **S**（~14 行） |
| A3 | `tauriEventName` 改名或删除：它把逻辑名映射到**已不存在的 Tauri 通道**；若保留，改名 `eventChannelName` 并去掉 Tauri 措辞 | `events/index.ts:5-9,47-53`、`events/index.test.ts:6-12` | **S**（函数 + 2 处测试） |
| A4 | 事件词表测试里的"生产者已核对"断言改为**引用本报告的表 3**（或加零命中名单的显式白名单），删掉对 `apps/agent/src/agent-loop.ts` 的转写声明 | `schemas/event-vocabulary.test.ts:5-13,23,82-91,134-135,167-173` | **S**（~15 行） |
| A5 | 三份 fixture 里指向 `crates/core/tests/fixtures/mcp-server.js` 的 `args` 改成真实可运行的假服务器路径 | `fixtures/ipc/mcp_server_{list,review,start}.json` | **M**（改值需重跑 fixture 消费测试） |
| A6 | 迁移注释里的路径改为当前实现的真实文件名 | `schema_migrations.hpp:3`、`scripts/check-publication.mjs:20`、`scripts/package.mjs:3-4`、`scripts/smoke-python-dev-agent.mjs:3` | **S**（4 行） |
| A7 | UI 注释与**用户可见文案**里的"Tauri 桌面壳"改为"C++ 桌面宿主"；文案改动要同步快照测试 | `lib/ipc.ts:219`、`lib/mcp.ts:8,315,363`、`lib/format.ts:17`、`components/PanelStates.tsx:124`、`features/**` 9 处、`tests/panel-states.test.tsx:150,176,182`、`tests/host-key-gate.test.ts:5`、`tests/host-mock.ts:5`、`tests/run-policy.test.ts:67` | **M**（~20 行 + 3 处快照/文案断言） |
| A8 | `schema_migrations.hpp` 与 database 迁移数（37）在文档里与代码对齐 | `docs/native-desktop-host.md` | **S** |
| A9 | 给 `docs/` 里无横幅的历史文档加"历史存档，现状见 README"横幅（不改正文） | `rust-removal-plan.md`、`release.md`、`acceptance-handoff.md`、`handover.md`、`changelog.md`、`cpp-desktop-handoff.md`、`native-desktop-host.md`、`architecture-and-build-plan.md` | **S**（8 行） |
| A10 | `agent_status.restart` 二选一：**删掉 schema 字段**（连同 153-157 行理由），或保留并标注"当前无生产者，接线前不得依赖" | `schemas/ipc.ts:144-159`、`fixtures/ipc/agent_status.json` | **S**（删）／**M**（接线，见 B3） |

### 8.2 「让实现补齐」（改行为）

| # | 动作 | 位置 | 成本 |
| --- | --- | --- | --- |
| B1 | **给 verbatim 转发的 13 条命令加响应闸门**：把 `IPC_SCHEMAS[c].response` 由 `z.strictObject` 改为 `.catchall(z.unknown())`/`passthrough`，或在宿主转发前按 schema 裁剪键。**这是本报告的最高优先级项**：它把"未来加字段即崩"变回"加字段无感" | `schemas/ipc.ts:394-461`（agent 8 条）、`635-639` 附近（provider 6 条，实际 5 条 + `provider_test`）；宿主侧 `ipc_router.cpp:9122`、`9658` | **M** |
| B2 | 三条死命令二选一：删（schema + 宿主分支 + `consistency.ts` 映射），或接 UI 调用点 | `agent_kill` `ipc_router.cpp:7547`；`investigation_task_status_update` `6124?/6524`；`investigation_schedule_tick` `7099` | **S**（删，每条）／**M-L**（接线，取决于是谁该调） |
| B3 | `agent_status.restart` 接线：C++ supervisor 需真正记录重启次数/时间并写 `response["restart"]`；否则删字段 | `ipc_router.cpp:9012-9035`、sidecar 生命周期代码 | **L** |
| B4 | 审批 `decision` 加白名单（当前只校验 `size() <= 32`），并把 `begin_response` 的 `nullopt` 由"静默成功"改为显式错误 | `ipc_router.cpp:9141`、`9152-9153`；`desktop_approval.cpp:309` | **S-M** |
| B5 | `mcp_server_list` 的 `view_for` 不再硬编码 `running:false / toolCount:0`，改为向 `McpSupervisor` 查询只读状态；`status_for` 的 `pid` 同样 | `ipc_router.cpp:9186-9204`、`desktop_mcp.cpp:1138-1156` | **L**（`McpSupervisor` 需新增只读 status API） |
| B6 | 两个"仅 fixture"命令二选一：删（schema + 宿主分支 + fixture case），或接 UI | `remote_file_read`（宿主 `ipc_router.cpp:6051`，fixture `500`）、`file_transfer_get`（宿主 `7613`，fixture `514`） | **S** |
| B7 | 9 个零生产者事件名二选一：从 `EVENT_NAMES` 删除，或补生产者 + 订阅者 | `events/index.ts:18-42` | **S**（删）／**L**（补生产者） |

---

## 九、无法判定 / 证据缺口

| 项 | 缺什么证据 |
| --- | --- |
| 除 UI 外是否还有 IPC 客户端（决定 §四 的 5 个 NI 尾是否真的全不可达） | WebView 侧 `invoke` 的注册/暴露点清单，或一份明确"UI 是唯一客户端"的威胁模型 |
| `agent.context.delete` 之外的 sidecar 方法是否有非宿主调用者 | 运行时 RPC 日志；当前只能证明"宿主按字面量只调 15 个方法"，无法证明"没有别的进程调" |
| 12 条 verbatim 转发命令在**真实** sidecar 运行时的实际键集 | 我下钻的是 handler 的 `return` 字面量；若某 handler 的键来自更下层（如 `_provider_list` 内部再调 store），需要一次带 trace 的端到端回放（R3 做过 20 组，覆盖的是"生产端真实形状"，未覆盖全部 12 条） |
| `docs/` 历史文档是否应保留原叙述 | 仓库的文档政策（`docs/` 是否被视为不可变历史）——我按"有横幅=有意存档"处理，需 Lead 确认口径 |

---

## 附录 A — 生成器源码

`target/r2/audit-contract-matrix.mjs`（1 347 行，逐字复制；运行方式 `node target/r2/audit-contract-matrix.mjs .`）：

```javascript
#!/usr/bin/env node
/**
 * R2-D cross-language IPC contract auditor  (task-12)
 *
 * Reads (read-only):
 *   packages/shared/src/ipc/index.ts         IPC_COMMANDS  (camelCase -> wire literal)
 *   packages/shared/src/schemas/ipc.ts       IPC_SCHEMAS   (authoritative command set)
 *   native/desktop_host/src/ipc_router.cpp   IpcRouter::invoke + *_command handlers
 *   agent/src/yukinal_agent/sidecar.py       JSON-RPC method table
 *   apps/desktop/src/**                      UI call sites
 *   apps/desktop/src/lib/preview-fixtures.ts preview-mode fixtures
 *
 * Writes: target/r2/contract-matrix.json
 * Usage:  node target/r2/audit-contract-matrix.mjs [repoRoot]
 *
 * The extraction is deliberately literal-based (no TS parser): every judgement
 * is reproducible by grepping the quoted strings this script reports.
 */

import fs from "node:fs";
import path from "node:path";

const ROOT = path.resolve(process.argv[2] ?? ".");
const read = (rel) => fs.readFileSync(path.join(ROOT, rel), "utf8");
const readLines = (rel) => read(rel).split(/\r?\n/);

// ------------------------------------------------------------------ lexical ---

/**
 * Replace comment bodies with spaces, preserving every offset and newline.
 * String-aware: a `//` or `/*` inside a string literal is left alone (this
 * matters because `packages/shared` schemas contain URLs and glob patterns).
 */
function blankComments(text) {
  let out = "";
  let i = 0;
  while (i < text.length) {
    const c = text[i];
    if (c === '"' || c === "'" || c === "`") {
      const q = c;
      out += c;
      i += 1;
      while (i < text.length) {
        const d = text[i];
        if (d === "\\") {
          out += text.slice(i, i + 2);
          i += 2;
          continue;
        }
        out += d;
        i += 1;
        if (d === q) break;
      }
      continue;
    }
    if (c === "/" && text[i + 1] === "/") {
      while (i < text.length && text[i] !== "\n") {
        out += " ";
        i += 1;
      }
      continue;
    }
    if (c === "/" && text[i + 1] === "*") {
      while (i < text.length && !(text[i] === "*" && text[i + 1] === "/")) {
        out += text[i] === "\n" ? "\n" : " ";
        i += 1;
      }
      out += "  ";
      i += 2;
      continue;
    }
    out += c;
    i += 1;
  }
  return out;
}

/**
 * A C++ digit separator (`100'000`) reads as an unterminated char literal to a
 * quote-only scanner and desynchronises every following brace. C++ code is
 * scanned with this flag set, so `'` is only treated as a literal when it
 * genuinely opens one (end of line, delimiter, or escape).
 */
let cppQuotes = false;
const isQuoteStart = (text, i) => {
  const c = text[i];
  if (c === '"' || c === "`") return true;
  if (c !== "'") return false;
  if (!cppQuotes) return true;
  const prev = text[i - 1];
  if (prev === "\\") return false;
  if (/[0-9a-fA-F]/.test(prev ?? "")) return false; // 100'000
  return true;
};

/**
 * Strips // line comments only, preserving offsets.
 * A `//` inside a string literal is not a comment: `"https://api.example.com"`
 * must survive intact.
 */
function stripLineComment(line) {
  let i = 0;
  while (i < line.length) {
    const c = line[i];
    if (isQuoteStart(line, i)) {
      const q = c;
      i += 1;
      while (i < line.length && line[i] !== q) {
        if (line[i] === "\\") i += 1;
        i += 1;
      }
      i += 1;
      continue;
    }
    if (c === "/" && line[i + 1] === "/") return line.slice(0, i);
    i += 1;
  }
  return line;
}

/**
 * Depth at the first non-space character of every line, keyed by ABSOLUTE line
 * number (1-based). Pass `startAtLine` = the line the scan begins on so the
 * recorded line numbers stay meaningful when scanning from an offset.
 */
function lineStartDepths(text, startIndex = 0) {
  const depths = [];
  let depth = 0;
  let line = lineAt(text, startIndex);
  let atLineStart = true;
  for (let i = startIndex; i < text.length; i += 1) {
    const c = text[i];
    if (c === "\n") {
      line += 1;
      atLineStart = true;
      continue;
    }
    if (/\s/.test(c)) continue;
    if (atLineStart) {
      if (depths[line] === undefined) depths[line] = depth;
      atLineStart = false;
    }
    if (isQuoteStart(text, i)) {
      const q = c;
      i += 1;
      while (i < text.length && text[i] !== q) {
        if (text[i] === "\\") i += 1;
        if (text[i] === "\n") line += 1;
        i += 1;
      }
      continue;
    }
    if (c === "{" || c === "(" || c === "[") depth += 1;
    else if (c === "}" || c === ")" || c === "]") depth -= 1;
  }
  return depths;
}

/** 1-based line number containing a character offset. */
const lineAt = (text, index) => text.slice(0, index).split("\n").length;

/** Index of the brace/paren that closes the one at `openIndex`. */
function matchingClose(text, openIndex) {
  let depth = 0;
  for (let i = openIndex; i < text.length; i += 1) {
    const c = text[i];
    if (isQuoteStart(text, i)) {
      const q = c;
      i += 1;
      while (i < text.length && text[i] !== q) {
        if (text[i] === "\\") i += 1;
        i += 1;
      }
      continue;
    }
    if (c === "/" && text[i + 1] === "/") {
      while (i < text.length && text[i] !== "\n") i += 1;
      continue;
    }
    if (c === "{" || c === "(" || c === "[") depth += 1;
    else if (c === "}" || c === ")" || c === "]") {
      depth -= 1;
      if (depth === 0) return i;
    }
  }
  return -1;
}

/**
 * Entries of an object literal, found by line depth: a key line whose first
 * non-space character sits at exactly `baseDepth` + 1 is a direct member.
 */
function objectEntries(text, openIndex, keyPattern = /^ {2}([A-Za-z_$][\w$]*|"[^"]+")\s*:/) {
  const closeIndex = matchingClose(text, openIndex);
  if (closeIndex < 0) return { entries: [], closeIndex: -1 };
  const depths = lineStartDepths(text, openIndex);
  const openLine = lineAt(text, openIndex);
  const closeLine = lineAt(text, closeIndex);
  const lines = text.split("\n");
  const entries = [];
  for (let line = openLine + 1; line < closeLine; line += 1) {
    const depth = depths[line];
    if (depth !== 1) continue;
    const m = keyPattern.exec(lines[line - 1]);
    if (!m) continue;
    entries.push({ key: m[1].replace(/^["']|["']$/g, ""), line, text: lines[line - 1] });
  }
  return { entries, closeIndex, openLine, closeLine };
}

/**
 * Direct members of a `const X = { ... }` literal, found by indentation:
 * a line consisting of exactly `indent` spaces, a key, then `:`.
 */
function entriesByIndent(lines, startLine, endLine, indent = 2) {
  const re = new RegExp(`^ {${indent}}([A-Za-z_$][\\w$]*|"[^"]+")\\s*:`);
  const entries = [];
  for (let ln = startLine; ln <= endLine; ln += 1) {
    const m = re.exec(lines[ln - 1]);
    if (m) entries.push({ key: m[1].replace(/^["']|["']$/g, ""), line: ln, text: lines[ln - 1] });
  }
  return entries;
}

/** Line number of the first line at/after `from` whose text matches `re`. */
function firstLineMatching(lines, from, re) {
  for (let ln = from; ln <= lines.length; ln += 1) if (re.test(lines[ln - 1])) return ln;
  return -1;
}

/**
 * Key names of an inline `z.strictObject({ ... })` / `z.object({ ... })`,
 * including nested ones: a nested object describes a key of an *entry* (e.g.
 * `entries: z.array(z.strictObject({ name, path, type, size }))`), not of the
 * reply, so only the outermost key set is returned and the nested members are
 * reported separately.
 */
function inlineSchemaKeys(expr) {
  const m = /z\.(?:strictObject|object)\(\s*\{/.exec(expr);
  if (!m) return null;
  const open = expr.indexOf("{", m.index + m[0].length - 1);
  const close = matchingClose(expr, open);
  if (close < 0) return null;
  const inner = expr.slice(open + 1, close);
  // A key is a name at brace depth 0 of `inner` followed by `:`; nested
  // `z.strictObject({...})` members describe an entry, not the reply.
  const keys = [];
  let depth = 0;
  let i = 0;
  while (i < inner.length) {
    const c = inner[i];
    if (c === '"' || c === "'" || c === "`") {
      const q = c;
      i += 1;
      while (i < inner.length && inner[i] !== q) {
        if (inner[i] === "\\") i += 1;
        i += 1;
      }
      i += 1;
      continue;
    }
    if (c === "{" || c === "(" || c === "[") {
      depth += 1;
      i += 1;
      continue;
    }
    if (c === "}" || c === ")" || c === "]") {
      depth -= 1;
      i += 1;
      continue;
    }
    if (depth === 0) {
      const km = /^([A-Za-z_$][\w$]*)\s*:/.exec(inner.slice(i));
      if (km && (i === 0 || /[{,\s]/.test(inner[i - 1]))) {
        if (!keys.includes(km[1])) keys.push(km[1]);
        i += km[0].length;
        continue;
      }
    }
    i += 1;
  }
  return keys;
}

// ----------------------------------------------------------- 1. IPC_COMMANDS ---

const ipcIndexRaw = read("packages/shared/src/ipc/index.ts");
const ipcIndexText = blankComments(ipcIndexRaw);
{
  const m = /export const IPC_COMMANDS = \{/.exec(ipcIndexText);
  if (!m) throw new Error("IPC_COMMANDS not found");
  const open = ipcIndexText.indexOf("{", m.index);
  const { entries } = objectEntries(ipcIndexText, open, /^ {2}([A-Za-z_$][\w$]*)\s*:\s*"([^"]+)"/);
  const pairs = entries.map((e) => {
    const mm = /^\s*([A-Za-z_$][\w$]*)\s*:\s*"([^"]+)"/.exec(e.text);
    return { camel: mm[1], literal: mm[2], line: e.line };
  });
  globalThis.__commandPairs = pairs;
}
const commandPairs = globalThis.__commandPairs;
const camelToLiteral = new Map(commandPairs.map((p) => [p.camel, p.literal]));
const literalToCamel = new Map(commandPairs.map((p) => [p.literal, p.camel]));

// ------------------------------------------------------------ 2. IPC_SCHEMAS ---

const ipcSchemaRaw = read("packages/shared/src/schemas/ipc.ts");
const ipcSchemaText = blankComments(ipcSchemaRaw);
const ipcSchemaLines = ipcSchemaText.split("\n");
const schemaEntries = [];
{
  const startLine = firstLineMatching(ipcSchemaLines, 1, /^export const IPC_SCHEMAS = \{/);
  if (startLine < 0) throw new Error("IPC_SCHEMAS not found");
  const endLine = firstLineMatching(ipcSchemaLines, startLine + 1, /^\} (?:satisfies|as const)/);
  if (endLine < 0) throw new Error("IPC_SCHEMAS block not terminated");
  // Direct members are the command names, i.e. exactly the keys `IPC_COMMANDS`
  // declares. Anchoring on that set (instead of on indentation) also picks up
  // the four entries that are mis-indented by two spaces (`server_connect`
  // and friends, ipc.ts:237-255) without also picking up `params`/`response`.
  const commandKeys = new Set(commandPairs.map((p) => p.literal));
  for (let ln = startLine + 1; ln < endLine; ln += 1) {
    const m = /^(\s*)([a-z_][a-z0-9_]*|"[^"]+")\s*:/.exec(ipcSchemaLines[ln - 1]);
    if (!m) continue;
    const key = m[2].replace(/^"|"$/g, "");
    if (!commandKeys.has(key)) continue;
    schemaEntries.push({ command: key, line: ln, indent: m[1].length });
  }
  schemaEntries.forEach((entry, i) => {
    const stop = i + 1 < schemaEntries.length ? schemaEntries[i + 1].line - 1 : endLine - 1;
    const span = ipcSchemaLines.slice(entry.line - 1, stop).join("\n");
    entry.endLine = stop;
    entry.span = span;
    entry.responseExpr = responseExpression(span);
  });
}

const SCHEMA_FILES = fs
  .readdirSync(path.join(ROOT, "packages/shared/src/schemas"))
  .filter((f) => f.endsWith(".ts") && !f.endsWith(".test.ts"))
  .map((f) => `packages/shared/src/schemas/${f}`)
  .sort();
const schemaFileTexts = new Map(SCHEMA_FILES.map((f) => [f, blankComments(read(f))]));

/**
 * Resolve a named zod object schema to its key set. Handles:
 *   export const X = z.strictObject({ ... });        (single or multi line)
 *   export const X = Y;                              (alias)
 * Returns null when the schema is not a plain object schema (unions, arrays,
 * refinements) — those are reported as `named-unresolved` rather than guessed.
 */
const namedSchemaCache = new Map();
function namedSchema(name, depth = 0) {
  if (depth > 4) return null;
  if (namedSchemaCache.has(name)) return namedSchemaCache.get(name);
  namedSchemaCache.set(name, null); // cycle guard
  for (const [file, text] of schemaFileTexts) {
    const re = new RegExp(`^export const ${name}\\b[^=]*=`, "m");
    const m = re.exec(text);
    if (!m) continue;
    const rhsStart = m.index + m[0].length;
    const inline = inlineSchemaKeys(statementRhs(text, rhsStart));
    let result = null;
    if (inline) {
      result = { file, line: lineAt(text, m.index), keys: inline, shape: "object" };
    } else {
      const rhs = statementRhs(text, rhsStart).trim();
      const alias = /^([A-Za-z_$][\w$]*)\s*$/.exec(rhs);
      if (alias) {
        result = namedSchema(alias[1], depth + 1);
      } else {
        // `X.passthrough()` / `X.partial()` / `X.extend({...})`: start from the
        // base schema's key set and apply the visible additions.
        const derived = /^([A-Za-z_$][\w$]*)\s*\n?\s*\.(extend|merge|partial|passthrough|strict)\(/.exec(rhs);
        if (derived) {
          const base = namedSchema(derived[1], depth + 1);
          const extra = inlineSchemaKeys(rhs);
          if (base) {
            const keys = derived[2] === "partial" ? base.keys : [...new Set([...base.keys, ...(extra ?? [])])];
            result = { file, line: lineAt(text, m.index), keys, shape: `object(.${derived[2]})` };
          }
        }
      }
    }
    namedSchemaCache.set(name, result);
    return result;
  }
  namedSchemaCache.set(name, null);
  return null;
}

/**
 * RHS of a TS declaration, from the first expression character up to the `;`
 * that terminates the statement at brace depth 0. Needed because schema
 * declarations often span several lines and contain nested object literals.
 */
function statementRhs(text, rhsStart) {
  let depth = 0;
  let end = -1;
  for (let i = rhsStart; i < text.length; i += 1) {
    const c = text[i];
    if (isQuoteStart(text, i)) {
      const q = c;
      i += 1;
      while (i < text.length && text[i] !== q) {
        if (text[i] === "\\") i += 1;
        i += 1;
      }
      continue;
    }
    if (c === "{" || c === "(" || c === "[") depth += 1;
    else if (c === "}" || c === ")" || c === "]") depth -= 1;
    else if (c === ";" && depth === 0) {
      end = i;
      break;
    }
  }
  return text.slice(rhsStart, end < 0 ? text.length : end);
}

/** First expression after `response:` — cut at the depth-0 comma or newline. */
function responseExpression(span) {
  const idx = span.lastIndexOf("response:");
  if (idx < 0) return "";
  let depth = 0;
  let out = "";
  for (let i = idx + "response:".length; i < span.length; i += 1) {
    const c = span[i];
    if (c === '"' || c === "'" || c === "`") {
      const q = c;
      out += c;
      i += 1;
      while (i < span.length && span[i] !== q) {
        if (span[i] === "\\") {
          out += span[i];
          i += 1;
        }
        out += span[i];
        i += 1;
      }
      out += q;
      continue;
    }
    if (c === "{" || c === "(" || c === "[") depth += 1;
    else if (c === "}" || c === ")" || c === "]") {
      depth -= 1;
      if (depth < 0) break;
    } else if (c === "," && depth === 0) break;
    out += c;
  }
  return out.trim();
}

function resolveResponse(expr) {
  const trimmed = expr.trim().replace(/,$/, "");
  if (/^EMPTY_PAYLOAD$/.test(trimmed)) return { kind: "empty", keys: [], via: null };
  const inline = inlineSchemaKeys(trimmed);
  if (inline) return { kind: "inline-strict", keys: inline, via: null };
  const named = /^([A-Za-z_$][\w$]*)/.exec(trimmed);
  if (named) {
    const found = namedSchema(named[1]);
    if (found) return { kind: "named", keys: found.keys, via: `${found.file}:${found.line}`, name: named[1] };
    return { kind: "named-unresolved", keys: [], via: null, name: named[1] };
  }
  return { kind: "opaque", keys: [], via: null, expr: trimmed.slice(0, 100) };
}

const schemaRows = schemaEntries.map((e) => ({
  ...e,
  camel: literalToCamel.get(e.command) ?? null,
  response: resolveResponse(e.responseExpr),
}));

// Every parser below runs on C++ text from here on: `100'000` must not be read
// as an opening char literal.
cppQuotes = true;

// ------------------------------------------- 3. host dispatch in IpcRouter ---

const routerFile = "native/desktop_host/src/ipc_router.cpp";
const routerLines = readLines(routerFile);
const routerRaw = routerLines.join("\n");

/**
 * Every C++ translation unit under native/desktop_host/src, comments blanked.
 * Used to resolve reply keys of helpers that live outside ipc_router.cpp, e.g.
 * `make_file_preview` (local_paths.cpp) and `transfer_snapshot_json` (ipc_router).
 */
const nativeTexts = fs
  .readdirSync(path.join(ROOT, "native/desktop_host/src"))
  .filter((f) => /\.(cpp|hpp)$/.test(f))
  .sort()
  .map((f) => {
    const rel = `native/desktop_host/src/${f}`;
    return { file: rel, text: blankComments(read(rel)) };
  });
const invokeLine = routerLines.findIndex((l) => l.includes("IpcReply IpcRouter::invoke(")) + 1;
const transferLine = routerLines.findIndex(
  (l, i) => i + 1 > invokeLine && l.includes("IpcReply IpcRouter::transfer_command("),
) + 1;

const hostDispatch = new Map();
{
  const start = invokeLine - 1;
  const end = transferLine - 1;
  let pending = [];
  for (let i = start; i < end; i += 1) {
    const line = stripLineComment(routerLines[i]);
    const ln = i + 1;
    const found = [...line.matchAll(/method\s*==\s*"([^"]+)"/g)].map((m) => m[1]);
    if (found.length > 0) {
      for (const name of found) pending.push({ name, line: ln });
      continue;
    }
    const handoff = /return\s+([a-z][a-z0-9_]*)\s*\(/.exec(line);
    if (pending.length === 0) continue;
    if (/^\s*return\b/.test(line)) {
      // `return failure(...)` is an inline rejection branch, not a handoff.
      const handler = handoff && handoff[1] !== "failure" ? handoff[1] : null;
      for (const p of pending) hostDispatch.set(p.name, { kind: handler ? "handler" : "inline", handler, line: p.line });
      pending = [];
    }
  }
  for (const p of pending) hostDispatch.set(p.name, { kind: "inline", handler: null, line: p.line });
}

// handler function extents
const handlerRegions = new Map();
{
  const starts = [];
  routerLines.forEach((l, i) => {
    // Any out-of-line member definition, not only `IpcReply` ones: a helper
    // member between two command handlers (`IpcRouter::configure_sidecar`,
    // ipc_router.cpp:8925) would otherwise be folded into the previous handler.
    const m = /^[A-Za-z_][\w:<>,&*\s]*\bIpcRouter::([a-z][a-z0-9_]*)\(/.exec(l);
    if (m) starts.push({ name: m[1], line: i + 1 });
  });
  starts.forEach((s, i) => {
    let end = i + 1 < starts.length ? starts[i + 1].line - 1 : routerLines.length;
    // Free functions and anonymous namespaces live between member definitions
    // (e.g. `namespace {` after provider_command at ipc_router.cpp:9666). Their
    // bodies must not be attributed to the preceding handler.
    for (let ln = s.line + 1; ln <= end; ln += 1) {
      if (/^namespace \{/.test(routerLines[ln - 1]) || /^\}  \/\/ namespace/.test(routerLines[ln - 1])) {
        end = ln - 1;
        break;
      }
    }
    handlerRegions.set(s.name, { start: s.line, end });
  });
}

// generic NOT_IMPLEMENTED tail of invoke()
const genericNotImplemented = routerLines.reduce((acc, l, i) => {
  if (/^\s*return failure\("NOT_IMPLEMENTED"/.test(l)) acc.push(i + 1);
  return acc;
}, []);
const specificNotImplemented = routerLines.reduce((acc, l, i) => {
  if (/failure\(\s*"NOT_IMPLEMENTED"/.test(l) && !/^\s*return failure\("NOT_IMPLEMENTED"/.test(l)) acc.push(i + 1);
  return acc;
}, []);
const allNotImplemented = [...genericNotImplemented, ...specificNotImplemented].sort((a, b) => a - b);

// kMethods maps (C++ command -> sidecar method)
const kMethodsMaps = [];
routerLines.forEach((l, i) => {
  if (!/kMethods\s*=\s*\{/.test(l)) return;
  const pairs = new Map();
  for (let j = i; j < routerLines.length; j += 1) {
    if (j > i && /\};/.test(routerLines[j])) break;
    for (const m of routerLines[j].matchAll(/\{"([^"]+)",\s*"([^"]+)"\}/g)) pairs.set(m[1], m[2]);
  }
  kMethodsMaps.push({ line: i + 1, pairs });
});

// sidecar RPC method literals referenced inside each handler region
const rpcLiteralsByHandler = new Map();
for (const [name, region] of handlerRegions) {
  const lits = new Map();
  for (let i = region.start - 1; i < region.end && i < routerLines.length; i += 1) {
    const line = stripLineComment(routerLines[i]);
    for (const m of line.matchAll(/request\(\s*"([a-z][a-z0-9_.]*)"|request\(\s*found->second/g)) {
      if (m[1]) lits.set(m[1], i + 1);
      else lits.set("__kMethods__", i + 1);
    }
  }
  rpcLiteralsByHandler.set(name, lits);
}

// ------------------------------------------------------ 4. sidecar methods ---

const sidecarFile = "agent/src/yukinal_agent/sidecar.py";
const sidecarText = read(sidecarFile);
const sidecarLines = sidecarText.split(/\r?\n/);
const sidecarMethods = new Map();
{
  const m = /^METHODS = \{/m.exec(sidecarText);
  if (m) {
    const open = sidecarText.indexOf("{", m.index);
    const { entries } = objectEntries(sidecarText, open, /^ {4}"([^"]+)"\s*:/);
    for (const e of entries) sidecarMethods.set(e.key, { line: e.line, table: "METHODS" });
  }
  for (const mm of sidecarText.matchAll(/elif method == "([^"]+)":/g)) {
    const line = lineAt(sidecarText, mm.index);
    const cur = sidecarMethods.get(mm[1]);
    if (cur) cur.dispatch = line;
    else sidecarMethods.set(mm[1], { line, table: "dispatch-chain" });
  }
  const asyncAllow = /handle_async[\s\S]*?or request\.get\("method"\) not in \(([\s\S]*?)\)/.exec(sidecarText);
  if (asyncAllow) {
    const line = lineAt(sidecarText, asyncAllow.index);
    for (const mm of asyncAllow[1].matchAll(/"([^"]+)"/g)) {
      const cur = sidecarMethods.get(mm[1]);
      if (cur) cur.async = true;
      else sidecarMethods.set(mm[1], { line, table: "handle_async", async: true });
    }
  }
}
/** Methods listed in METHODS but explicitly refused with RPC_NOT_IMPLEMENTED. */
const sidecarDeclaredNotImplemented = new Set();
{
  const m = /elif method in METHODS:\s*\n\s*raise _RpcFault\(RPC_NOT_IMPLEMENTED/.exec(sidecarText);
  if (m) {
    for (const e of sidecarMethods.keys()) sidecarDeclaredNotImplemented.add(e);
    sidecarDeclaredNotImplemented.rpcLine = lineAt(sidecarText, m.index);
  }
}
/** RPC methods the sidecar implements (dispatched or in METHODS). */
const sidecarImplemented = (name) => {
  if (!sidecarMethods.has(name)) return { ok: false, reason: "no handler" };
  const entry = sidecarMethods.get(name);
  if (entry.table === "handle_async" && !entry.dispatch) return { ok: true, line: entry.line, note: "async allow-list only" };
  if (entry.table === "METHODS" && !entry.dispatch && !entry.async) {
    // present in METHODS but not dispatched anywhere -> refused with RPC_NOT_IMPLEMENTED (line 228-229)
    return { ok: false, reason: "declared in METHODS but refused (RPC_NOT_IMPLEMENTED)", line: entry.line };
  }
  return { ok: true, line: entry.dispatch ?? entry.line };
};

// ------------------------------------------------------------ 5. UI call sites ---

const srcFiles = [];
(function walk(dir) {
  for (const entry of fs.readdirSync(path.join(ROOT, dir), { withFileTypes: true })) {
    const p = `${dir}/${entry.name}`;
    if (entry.isDirectory()) walk(p);
    else if (/\.(ts|tsx)$/.test(entry.name)) srcFiles.push(p);
  }
})("apps/desktop/src");
const srcTexts = new Map(srcFiles.map((f) => [f, read(f).split(/\r?\n/)]));

function scan(predicate) {
  const hits = [];
  for (const [file, lines] of srcTexts) {
    lines.forEach((line, i) => {
      const clean = stripLineComment(line);
      if (predicate(clean, file)) hits.push({ file, line: i + 1, text: clean.trim().slice(0, 110) });
    });
  }
  return hits;
}

const FIXTURE_FILE = "apps/desktop/src/lib/preview-fixtures.ts";
const uiByCamel = new Map();
const uiByLiteral = new Map();
for (const { camel, literal } of commandPairs) {
  const camelHits = scan((line, file) => file !== FIXTURE_FILE && new RegExp(`IPC_COMMANDS\\.${camel}\\b`).test(line));
  uiByCamel.set(camel, camelHits);
  const literalHits = scan((line, file) => file !== FIXTURE_FILE && (line.includes(`"${literal}"`) || line.includes(`'${literal}'`)));
  uiByLiteral.set(literal, literalHits);
}

const fixtureCases = new Map();
{
  const lines = srcTexts.get(FIXTURE_FILE) ?? [];
  lines.forEach((line, i) => {
    const m = /case\s+IPC_COMMANDS\.([A-Za-z_$][\w$]*)/.exec(stripLineComment(line));
    if (m) fixtureCases.set(camelToLiteral.get(m[1]) ?? m[1], { file: FIXTURE_FILE, line: i + 1 });
  });
}

// ------------------------------------------------------------- 6. verdicts ---

/**
 * Line range of the branch inside a shared handler that serves `command`.
 * A sibling `if (method == "...")` at the same or lower indent belongs to a
 * different command; branches joined by `||` intentionally share a range.
 * Returns null when the handler has no branch listing this command (the
 * command is then served by the handler's fall-through path).
 */
function branchRange(command, dispatch) {
  const region = dispatch.handler ? handlerRegions.get(dispatch.handler) : null;
  if (!region) return null;
  const branches = [];
  for (let i = region.start - 1; i < region.end && i < routerLines.length; i += 1) {
    const line = stripLineComment(routerLines[i]);
    const head = /^(\s*)if \(/.exec(line);
    if (!head) continue;
    const names = [...line.matchAll(/method\s*==\s*"([^"]+)"/g)].map((m) => m[1]);
    if (names.length > 0) branches.push({ line: i + 1, indent: head[1].length, names });
  }
  const own = branches.find((b) => b.names.includes(command));
  if (!own) return { from: region.start, to: region.end, narrowed: false };
  const next = branches.find((b) => b.line > own.line && b.indent <= own.indent);
  return { from: own.line, to: next ? next.line - 1 : region.end, narrowed: true };
}

/**
 * Sidecar RPC literal(s) the host uses within [from, to].
 *
 * Only call positions count: a dotted string like `"agent.stream"` is a
 * notification topic (ipc_router.cpp:8963) or an event name, not an RPC
 * method. Dotted literals are therefore accepted only as the first argument of
 * `request(`/`call(`/`invoke(`.
 */
function rpcLiteralsIn(from, to) {
  const methods = new Set();
  for (let i = from - 1; i < to && i < routerLines.length; i += 1) {
    const line = stripLineComment(routerLines[i]);
    for (const m of line.matchAll(/(?:request|call|invoke)\(\s*(?:"([a-z][a-z0-9_.]*)"|\w)/g)) {
      if (m[1] && m[1].includes(".")) methods.add(m[1]);
    }
  }
  return methods;
}

const rows = schemaRows.map((row) => {
  const dispatch = hostDispatch.get(row.command) ?? null;
  const host = dispatch
    ? { status: dispatch.kind, handler: dispatch.handler, line: dispatch.line }
    : { status: "missing", handler: null, line: null, fallbackLine: genericNotImplemented[0] ?? null };

  const range = dispatch?.handler ? branchRange(row.command, dispatch) : null;

  let sidecar = { status: "n/a", method: null, line: null, reason: null };
  if (dispatch?.handler && range) {
    const methods = new Set();
    for (const km of kMethodsMaps) if (km.pairs.has(row.command)) methods.add(km.pairs.get(row.command));
    for (const lit of rpcLiteralsIn(range.from, range.to)) methods.add(lit);
    if (methods.size > 0) {
      const results = [...methods].map((m) => ({ method: m, ...sidecarImplemented(m) }));
      const ok = results.filter((r) => r.ok);
      sidecar = {
        status: ok.length === results.length ? "implemented" : ok.length === 0 ? "no-handler" : "partial",
        method: results.map((r) => r.method).join(" | "),
        line: ok.length > 0 ? ok[0].line : null,
        reason: results.filter((r) => !r.ok).map((r) => `${r.method}: ${r.reason}`).join("; ") || null,
        details: results,
        source: `${routerFile}:${range.from}-${range.to}`,
      };
    }
  } else if (dispatch) {
    sidecar = { status: "host-local", method: null, line: null };
  }

  const ui = uiByCamel.get(row.camel) ?? [];
  const uiLiteral = uiByLiteral.get(row.command) ?? [];
  const fixture = fixtureCases.get(row.command) ?? null;

  let verdict;
  if (host.status === "missing") verdict = "宿主未实现(NOT_IMPLEMENTED)";
  else if (sidecar.status === "no-handler") verdict = "sidecar 无 handler";
  else if (ui.length === 0 && uiLiteral.length === 0 && fixture) verdict = "仅 preview fixture 使用";
  else if (ui.length === 0 && uiLiteral.length === 0) verdict = "UI 无调用点（死命令）";
  else verdict = "端到端可达";

  return {
    command: row.command,
    camel: row.camel,
    line: row.line,
    host,
    sidecar,
    ui,
    uiLiteral,
    fixture,
    verdict,
    responseKeys: row.response.keys,
    responseKind: row.response.kind,
    responseVia: row.response.via ?? null,
  };
});

// ------------------------------------------- 7. host JSON key diff per row ---

/**
 * Key set of a host reply payload, i.e. the second element of
 * `IpcReply{true, <payload>}`. Recognised shapes:
 *   return {true, {{"k", ...}, {"j", ...}}};        -> k, j
 *   nlohmann::json r = {{"k", ...}}; return {true, std::move(r)};
 *   return {true, database_->some_call(...)};       -> opaque
 * Only the first two are visually decidable from this file; the third is
 * reported as `hostKeyOpaque` so the report can say "cannot judge here".
 */
function entriesOfPayload(payload) {
  // payload = `{"k", v}` or `{"k", v}, {"j", w}`
  const keys = [];
  let depth = 0;
  for (let i = 0; i < payload.length; i += 1) {
    const c = payload[i];
    if (c === '"' || c === "'" || c === "`") {
      const q = c;
      i += 1;
      while (i < payload.length && payload[i] !== q) {
        if (payload[i] === "\\") i += 1;
        i += 1;
      }
      continue;
    }
    if (c === "{") {
      depth += 1;
      if (depth === 1) {
        const m = /^\{\s*"([A-Za-z_][\w]*)"\s*,/.exec(payload.slice(i));
        if (m) keys.push(m[1]);
      }
      continue;
    }
    if (c === "}") depth -= 1;
  }
  return keys;
}

/**
 * Guard blocks inside a handler: `if (method == "a" || method == "b") { ... }`
 * and prefix guards `if (method.rfind("remote_file_", 0) == 0) { ... }`.
 * Each block records the commands it can serve and the line span of its body.
 */
function guardBlocks(region) {
  const blocks = [];
  for (let ln = region.start; ln <= region.end && ln <= routerLines.length; ln += 1) {
    const line = stripLineComment(routerLines[ln - 1]);
    if (!/^\s*if \(/.test(line)) continue;
    // Collect the whole statement, but stop at the first `{` or `;`: a
    // brace-less `if (cond) stmt;` is not a block and its extent must not be
    // guessed from the following code.
    let cond = line;
    let braceLine = /\{/.test(line) ? ln : -1;
    let semi = /;/.test(line);
    let scan = ln;
    while (braceLine < 0 && !semi && scan < region.end) {
      scan += 1;
      const next = stripLineComment(routerLines[scan - 1]);
      cond += ` ${next.trim()}`;
      if (/\{/.test(next)) braceLine = scan;
      if (/;/.test(next)) semi = true;
    }
    // A `;` before the `{` means the if-body was a single statement.
    if (braceLine < 0 || (semi && routerLines.slice(ln - 1, braceLine).join("\n").includes(";"))) continue;
    if (/;\s*$/.test(cond.split("{")[0])) continue;
    const braceCol = routerLines[braceLine - 1].indexOf("{");
    if (braceCol < 0) continue;
    const before = routerLines[braceLine - 1].slice(0, braceCol).trimEnd();
    if (!before.endsWith(")")) continue;
    const openIndex = routerLineOffsets[braceLine - 1] + braceCol;
    const closeIndex = matchingClose(routerRaw, openIndex);
    if (closeIndex < 0) continue;
    const head = routerLines[ln - 1];
    const names = [...cond.matchAll(/method\s*==\s*"([^"]+)"/g)].map((m) => m[1]);
    // `if (method != "agent_approval_respond")`: serves every command *except*
    // the named one, so the same data must be recorded with inverted polarity.
    const notNames = [...cond.matchAll(/method\s*!=\s*"([^"]+)"/g)].map((m) => m[1]);
    const prefix = /method\.rfind\(\s*"([^"]+)"/.exec(cond);
    blocks.push({
      line: ln,
      endLine: lineAt(routerRaw, closeIndex),
      indent: /^(\s*)/.exec(head)[1].length,
      names,
      notNames,
      prefix: prefix ? prefix[1] : null,
    });
  }
  return blocks;
}

/** Character offset at the start of each 1-based line of ipc_router.cpp. */
const routerLineOffsets = (() => {
  const offsets = [0];
  for (let i = 0; i < routerRaw.length; i += 1) if (routerRaw[i] === "\n") offsets.push(i + 1);
  return offsets;
})();

/**
 * Lines of a handler that can produce this command's success reply.
 *
 * The command's own guard (an explicit `if (method == "x")` branch, or the
 * prefix guard `if (method.rfind("remote_file_", 0) == 0)`) bounds the search;
 * a command with no guard is served by the handler's fall-through code, so the
 * whole handler is the base. Sibling guards naming other commands — and every
 * block nested inside them — are removed, because their `return {true, ...}`
 * belongs to that other command. Guards on the *shape* of a nested payload
 * (`if (method == "agent.stream" && params.value("type", ...) == "...")`) are
 * removed by the same rule: they only gate a side effect.
 */
/** Does this block contain a success reply (not just validation failures)? */
function blockHasSuccess(from, to) {
  const text = textOfLines(rangeLines(from, to));
  if (/return\s*\{\s*true\s*,/.test(text)) return true;
  // `return transfer_snapshot_reply(...)`, `return describe_network_proxy(...)`:
  // the payload is built by a helper. `return failure(...)` stays a rejection.
  for (const m of text.matchAll(/return\s+([A-Za-z_][\w:>.\-]*)\s*\(/g)) {
    if (m[1].replace(/.*[>:.\-]/, "") !== "failure") return true;
  }
  return false;
}

function commandLineIntervals(command, dispatch) {
  const region = handlerRegions.get(dispatch.handler);
  if (!region) return null;
  const blocks = guardBlocks(region).map((b) => ({
    ...b,
    // A guard `if (method == "a" || method == "b")` serves exactly a and b; a
    // guard `if (method != "c")` serves every command in the handler except c.
    serves:
      b.names.length > 0 ? b.names.includes(command) : b.notNames.length > 0 && !b.notNames.includes(command),
    hasSuccess: blockHasSuccess(b.line, b.endLine),
  }));
  // A command's reply lives in the innermost guard that serves it *and* actually
  // returns a success payload. A guard that only validates preconditions
  // (`agent_run_start` at ipc_router.cpp:9094) is skipped, so the tail of the
  // handler is used instead.
  const own = blocks.filter((b) => b.hasSuccess && b.serves).sort((a, b) => b.line - a.line)[0];
  const byPrefix = [...blocks].reverse().find((b) => b.prefix && command.startsWith(b.prefix) && b.hasSuccess);
  const base = own ?? byPrefix ?? { line: region.start, endLine: region.end };
  const covers = (outer, inner) => outer !== inner && inner.line >= outer.line && inner.endLine <= outer.endLine;
  const gates = blocks.filter(
    (b) => b !== base && b.line >= base.line && b.endLine <= base.endLine && !b.serves,
  );
  // A gate and everything nested inside it answers a different command.
  const excluded = blocks.filter((b) => gates.some((g) => g === b || covers(g, b)));
  const lines = [];
  for (let ln = base.line; ln <= base.endLine; ln += 1) {
    if (excluded.some((b) => ln >= b.line && ln <= b.endLine)) continue;
    lines.push(ln);
  }
  const intervals = [];
  for (const ln of lines) {
    const last = intervals[intervals.length - 1];
    if (last && last[1] === ln - 1) last[1] = ln;
    else intervals.push([ln, ln]);
  }
  return {
    intervals,
    base: { from: base.line, to: base.endLine, via: own ? "explicit-branch" : byPrefix ? "prefix-guard" : "fall-through" },
    excluded: excluded.map((b) => ({ from: b.line, to: b.endLine, names: b.names, notNames: b.notNames, prefix: b.prefix })),
    narrowed: Boolean(own) || excluded.length > 0,
  };
}

/** Text of the listed 1-based line numbers, joined in order. */
const textOfLines = (lns) => lns.map((ln) => routerLines[ln - 1] ?? "").join("\n");
const rangeLines = (from, to) => Array.from({ length: to - from + 1 }, (_, i) => from + i);

/**
 * Callee of a statement like `return run_remote_file_list(...)` or
 * `return database_->list_servers();`. `a->b(...)` / `a.b(...)` resolve to the
 * member name, which is what the native source scan needs to find the body.
 */
const calleeOfReturn = (head) => {
  const m = /(?:([A-Za-z_][\w]*)\s*(?:::|->|\.)\s*)?([A-Za-z_][\w]*)\s*\(/.exec(head);
  if (!m) return null;
  return m[2];
};

/**
 * Keys of the Python dict a normalizer helper returns. The host forwards
 * `normalized.at("output")` without building any key itself, so the reply shape
 * is decided in `agent/src/yukinal_agent/ssh_tools.py`. Only the three
 * normalizers the host actually forwards are mapped; anything else stays
 * "cannot judge" instead of being guessed.
 */
const PY_NORMALIZERS = {
  "server.logs.normalize": ["_parse_server_logs"],
  "server.services.normalize": ["_parse_service_inventory"],
  "server.snapshot.normalize": ["_parse_snapshot"],
};
/** Which normalizer a command's reply is forwarded from (1:1, no guessing). */
const PY_NORMALIZER_BY_COMMAND = {
  server_logs: "server.logs.normalize",
  server_services: "server.services.normalize",
  server_snapshot: "server.snapshot.normalize",
};
const pyText = read("agent/src/yukinal_agent/ssh_tools.py");
function pythonDictKeys(fnName) {
  const def = new RegExp(`^def ${fnName}\\(`, "m").exec(pyText);
  if (!def) return null;
  const keys = new Set();
  // Every `return` in the function body up to the next top-level `def`.
  const rest = pyText.slice(def.index);
  const nextDef = /\n(?=def |class )/.exec(rest.slice(1));
  const body = nextDef ? rest.slice(0, nextDef.index + 1) : rest;
  const addLiteral = (text, from) => {
    const open = text.indexOf("{", from);
    if (open < 0) return;
    const close = matchingClose(text, open);
    if (close < 0) return;
    for (const km of text.slice(open + 1, close).matchAll(/["']([A-Za-z_][\w]*)["']\s*:/g)) keys.add(km[1]);
  };
  for (const m of body.matchAll(/return\s+/g)) {
    const after = body.slice(m.index + m[0].length);
    if (after.startsWith("{")) {
      addLiteral(body, m.index + m[0].length);
      continue;
    }
    // `return result` where `result` was built as a literal and then extended
    // by `result["message"] = …` for the unavailable branch.
    const varName = /^([A-Za-z_][\w]*)/.exec(after);
    if (!varName) continue;
    const name = varName[1];
    const init = new RegExp(`\\b${name}\\s*(?::[^=\\n]+)?=\\s*\\{`).exec(body);
    if (init) addLiteral(body, init.index + init[0].length - 1);
    for (const am of body.matchAll(new RegExp(`\\b${name}\\s*\\[\\s*["']([A-Za-z_][\\w]*)["']\\s*\\]\\s*=`, "g"))) {
      keys.add(am[1]);
    }
  }
  return { file: "agent/src/yukinal_agent/ssh_tools.py", line: lineAt(pyText, def.index), keys: [...keys].sort() };
}

function replyKeysFromLines(lines, command = null) {
  const text = textOfLines(lines);
  const keys = new Set();
  const opaque = [];
  const addAssignments = (name) => {
    const re = new RegExp(`\\b${name}\\s*\\[\\s*"([A-Za-z_][\\w]*)"\\s*\\]\\s*=`, "g");
    // A conditional key (`if (pinned) response["pinnedFingerprint"] = …`) is a
    // key the reply can carry, so it is collected like any other.
    for (const m of text.matchAll(re)) keys.add(m[1]);
  };
  for (const m of text.matchAll(/return\s*\{\s*true\s*,\s*/g)) {
    const payloadStart = m.index + m[0].length;
    const head = text.slice(payloadStart).split("\n")[0].trim();
    if (head.startsWith("{")) {
      const close = matchingClose(text, payloadStart);
      if (close < 0) {
        opaque.push({ callee: null, text: head.slice(0, 70) });
        continue;
      }
      for (const k of entriesOfPayload(text.slice(payloadStart + 1, close))) keys.add(k);
      continue;
    }
    // `return {true, someCall(...)}` or `return {true, std::move(*local)}`
    const moved = /^std::move\(\s*\*?\s*([A-Za-z_][\w]*)\s*\)/.exec(head);
    if (moved) {
      const name = moved[1];
      const assign = new RegExp(`(?:nlohmann::json|json)\\s+${name}\\s*=\\s*\\{`).exec(text);
      let literalFound = false;
      if (assign) {
        const aOpen = text.indexOf("{", assign.index + assign[0].length - 1);
        const aClose = matchingClose(text, aOpen);
        if (aClose > 0 && aClose - aOpen < 20000) {
          for (const k of entriesOfPayload(text.slice(aOpen + 1, aClose))) keys.add(k);
          literalFound = true;
        }
      }
      // Keys added to that same object after its literal.
      addAssignments(name);
      if (literalFound) continue;
      // `auto detail = database_->get_investigation_task(id); return std::move(*detail);`
      // — the payload is built by that member, so follow it.
      const memberInit = new RegExp(
        `(?:const\\s+)?auto\\s+${name}\\s*=\\s*[A-Za-z_][\\w]*\\s*(?:->|\\.)\\s*([A-Za-z_][\\w]*)\\s*\\(`,
      ).exec(text);
      if (memberInit) {
        opaque.push({ callee: memberInit[1], text: `${name} = …->${memberInit[1]}(…)` });
        continue;
      }
      opaque.push({ callee: name, text: `${name} (object literal not found in the branch)` });
      continue;
    }
    // `return {true, result}` where `result` came from a call whose own helper
    // builds the object (`oauth_connect`, `McpSupervisor::start`): resolve the
    // variable back to its initializer's callee and hand it to helperKeys.
    const bareVar = /^([A-Za-z_][\w]*)\s*\}?\s*;?$/.exec(head);
    if (bareVar) {
      const name = bareVar[1];
      const init = new RegExp(`(?:const\\s+)?auto\\s+${name}\\s*=\\s*([A-Za-z_][\\w]*)\\s*\\(`).exec(text);
      if (init) {
        opaque.push({ callee: init[1], text: `${name} = ${init[1]}(…)` });
        continue;
      }
      // `auto detail = database_->get_investigation_task(id);` → member helper.
      const memberInit = new RegExp(`(?:const\\s+)?auto\\s+${name}\\s*=\\s*[A-Za-z_][\\w]*\\s*(?:->|\\.)\\s*([A-Za-z_][\\w]*)\\s*\\(`).exec(text);
      if (memberInit) {
        opaque.push({ callee: memberInit[1], text: `${name} = …->${memberInit[1]}(…)` });
        continue;
      }
    }
    // `return {true, std::move(*local)}` where the local is an optional filled
    // from a member call: `auto detail = database_->get_…(id)`.
    const movedVar = /^std::move\(\s*\*?\s*([A-Za-z_][\w]*)\s*\)/.exec(head);
    if (movedVar) {
      const name = movedVar[1];
      const memberInit = new RegExp(`(?:const\\s+)?auto\\s+${name}\\s*=\\s*[A-Za-z_][\\w]*\\s*(?:->|\\.)\\s*([A-Za-z_][\\w]*)\\s*\\(`).exec(text);
      if (memberInit) {
        opaque.push({ callee: memberInit[1], text: `${name} = …->${memberInit[1]}(…)` });
        continue;
      }
    }
    // `return {true, nlohmann::json::object()}` is an explicitly empty payload.
    if (/^nlohmann::json::object\(\s*\)/.test(head)) continue;
    // `return {true, client.request(...)}`: the sidecar's response object is
    // handed back verbatim, so the reply keys are decided in Python and this
    // file constructs none of them.
    if (/(?:^|[^\w])request\s*\(/.test(head)) {
      opaque.push({ callee: null, rpcForward: true, text: head.slice(0, 70) });
      continue;
    }
    // `return {true, normalized.at("output")}` re-emits a sidecar object whose
    // keys are decided in Python; the reply keys are not in this file.
    const passthrough = /\.at\(\s*"([A-Za-z_][\w]*)"\s*\)/.exec(head);
    if (passthrough) {
      opaque.push({ callee: null, passthrough: passthrough[1], text: head.slice(0, 70) });
      continue;
    }
    opaque.push({ callee: calleeOfReturn(head), text: head.slice(0, 70) });
  }
  // `return transfer_snapshot_reply(*manager, id);` — a success payload built
  // by a helper, without a `{true, …}` literal at all. `return handle_host_request(…)`
  // is the same idea: the payload is built by that function, not here.
  for (const m of text.matchAll(/return\s+([A-Za-z_][\w]*)\s*\(/g)) {
    if (m[1] === "failure") continue;
    if (!opaque.some((o) => o.callee === m[1])) opaque.push({ callee: m[1], text: `${m[1]}(…)` });
  }
  // `client.request("server.logs.normalize", …)` then `return {true, …}`: the
  // host forwards the Python dict, so its keys come from ssh_tools.py. Only the
  // one normalizer this command actually forwards is followed.
  const normalizer = command ? PY_NORMALIZER_BY_COMMAND[command] : null;
  if (normalizer && text.includes(`"${normalizer}"`)) {
    for (const fnName of PY_NORMALIZERS[normalizer] ?? []) {
      const py = pythonDictKeys(fnName);
      if (!py) continue;
      for (const k of py.keys) keys.add(k);
      opaque.push({ callee: null, pyForward: normalizer, ...py, text: `${fnName} → ${py.file}:${py.line}` });
    }
  }
  return { keys: [...keys].sort(), opaque, pyForwarded: opaque.filter((o) => o.pyForward) };
}

/**
 * Keys returned by a named C++ helper, e.g. `make_file_preview` or
 * `transfer_snapshot_json`. The helper lives in a different translation unit,
 * so its `return {...}` literals are scanned there instead of being reported
 * as undecidable. Returns null when the helper cannot be located.
 */
const helperKeyCache = new Map();
function helperKeys(name, depth = 0) {
  if (depth > 3) return null;
  if (helperKeyCache.has(name)) return helperKeyCache.get(name);
  helperKeyCache.set(name, null);
  const bare = name.includes("::") ? name.slice(name.lastIndexOf("::") + 2) : name;
  // A lambda assigned to a local (`const auto view_for = [](...) { ... };`) is
  // as much a reply-payload builder as a free function.
  const patterns = [
    new RegExp(`^[\\w:<>,&*\\s]*\\b${bare}\\s*\\([^;{]*\\)[^;{]*\\{`, "m"),
    new RegExp(`(?:const\\s+)?auto\\s+${bare}\\s*=\\s*\\[[^\\]]*\\]\\([^)]*\\)[^;{]*\\{`, "m"),
  ];
  for (const { file, text } of nativeTexts) {
    let def = null;
    for (const re of patterns) {
      def = re.exec(text);
      if (def) break;
    }
    if (!def) continue;
    let i = text.indexOf("{", def.index + def[0].length - 1);
    let depthB = 0;
    let end = -1;
    for (let j = i; j < text.length; j += 1) {
      if (text[j] === "{") depthB += 1;
      else if (text[j] === "}") {
        depthB -= 1;
        if (depthB === 0) {
          end = j;
          break;
        }
      }
    }
    if (end < 0) continue;
    const body = text.slice(i, end);
    const keys = new Set();
    const nested = new Set();
    for (const m of body.matchAll(/return\s*\{/g)) {
      // Only a payload literal: `{{"k", v}, …}` or `{true, {{"k", v}}}`. A
      // `return {true, call(…)}` inside a helper is someone else's payload.
      const open = body.indexOf("{", m.index + "return".length);
      if (open < 0) continue;
      const close = matchingClose(body, open);
      if (close < 0) continue;
      let inner = body.slice(open + 1, close);
      // `return {true, {{"k", v}}}`: skip the success flag, keep the payload.
      const flag = /^\s*true\s*,\s*/.exec(inner);
      if (flag) {
        inner = inner.slice(flag[0].length);
        // `{{"k", v}, {"j", w}}` — the outer brace wraps the pair list.
        if (/^\s*\{/.test(inner)) {
          const o2 = inner.indexOf("{");
          const c2 = matchingClose(inner, o2);
          if (c2 > 0) inner = inner.slice(o2 + 1, c2);
        }
      }
      if (!/^\s*\{/.test(inner)) continue;
      for (const k of entriesOfPayload(inner)) keys.add(k);
      continue;
    }
    // Keys of an object this function returns, built as a local first:
    //   nlohmann::json view = {{"config", …}, {"status", …}};  return view;
    //   nlohmann::json response = {…}; response["extra"] = …;  return response;
    // Nested assignments (`connection["identityId"] = …`) belong to a field of
    // an entry, so only the returned variable's own name is honoured.
    const returnedVars = new Set(
      [...body.matchAll(/return\s+(?:std::)?(?:move\()?\s*\*?\s*([A-Za-z_][\w]*)/g)].map((m) => m[1]),
    );
    for (const name of returnedVars) {
      const init = new RegExp(`\\b${name}\\s*=\\s*\\{`).exec(body);
      if (init) {
        const open = body.indexOf("{", init.index + init[0].length - 1);
        const close = matchingClose(body, open);
        if (close > 0) for (const k of entriesOfPayload(body.slice(open + 1, close))) keys.add(k);
      }
      const reAssign = new RegExp(`\\b${name}\\s*\\[\\s*"([A-Za-z_][\\w]*)"\\s*\\]\\s*=`, "g");
      for (const m of body.matchAll(reAssign)) keys.add(m[1]);
    }
    // Delegation to another helper.
    for (const m of body.matchAll(/return\s+([A-Za-z_][\w]*(?:::[A-Za-z_][\w]*)*)\s*\(/g)) nested.add(m[1]);
    if (keys.size === 0 && nested.size > 0) {
      for (const fn of nested) {
        const sub = helperKeys(fn, depth + 1);
        if (sub) for (const k of sub.keys) keys.add(k);
      }
    }
    const result = { file, line: lineAt(text, def.index), keys: [...keys].sort() };
    helperKeyCache.set(name, result);
    return result;
  }
  helperKeyCache.set(name, null);
  return null;
}

/**
 * Full key union of a handler, ignoring branch guards. Used only to tell
 * "this branch really adds the key" from "a sibling branch adds it".
 */
function handlerUnionKeys(dispatch) {
  const region = handlerRegions.get(dispatch.handler);
  if (!region) return { keys: [], opaque: [] };
  const lines = [];
  for (let ln = region.start; ln <= region.end; ln += 1) lines.push(ln);
  return replyKeysFromLines(lines);
}

for (const row of rows) {
  if (row.host.status === "missing") continue;
  const dispatch = hostDispatch.get(row.command);
  // Inline commands are answered inside `IpcRouter::invoke` itself; delegated
  // ones inside the named handler it returns into.
  const container = dispatch.handler ?? "invoke";
  const scope = handlerRegions.has(container)
    ? commandLineIntervals(row.command, { handler: container })
    : null;
  const fallback = { from: dispatch.line, to: dispatch.line };
  const { keys, opaque } = scope
    ? replyKeysFromLines(
        scope.intervals.flatMap(([a, b]) => Array.from({ length: b - a + 1 }, (_, i) => a + i)),
        row.command,
      )
    : { keys: [], opaque: [{ callee: null, text: "no handler region found" }] };
  row.hostKeys = keys;
  row.hostKeyOpaque = opaque;
  row.hostKeyScope = scope ? { ...scope.base, handler: container } : { ...fallback, handler: null };
  row.hostKeyExcluded = scope?.excluded ?? [];
  row.hostKeyShapeGuards = scope?.shapeGuards ?? [];
  row.hostKeySource = scope
    ? `${routerFile}:${scope.intervals.map(([a, b]) => (a === b ? `${a}` : `${a}-${b}`)).join(",")}`
    : `${routerFile}:${fallback.from}`;
  row.hostKeyNarrowed = scope?.narrowed ?? false;
  row.extraInHost = keys.filter((k) => !row.responseKeys.includes(k));
  row.missingFromHost = row.responseKeys.filter((k) => !keys.includes(k));
  // Union of every literal in the whole handler, so the report can distinguish
  // "this branch adds a key" from "a sibling branch adds one".
  const whole = handlerUnionKeys({ handler: container });
  row.handlerUnionKeys = whole.keys;
  row.handlerOpaque = whole.opaque;
  row.siblingOnlyKeys = whole.keys.filter((k) => !keys.includes(k));

  // Resolve unnoted callees whose return payload is built inside a helper in
  // another translation unit, so those rows are not left as "cannot judge".
  row.hostKeyResolved = [];
  for (const item of [...row.hostKeyOpaque]) {
    if (!item.callee) continue;
    const helper = helperKeys(item.callee);
    if (!helper) continue;
    for (const k of helper.keys) row.hostKeys.push(k);
    row.hostKeyResolved.push({ callee: item.callee, ...helper });
    item.resolvedVia = `${helper.file}:${helper.line}`;
  }
  row.hostKeys = [...new Set(row.hostKeys)].sort();
  row.hostKeyOpaque = row.hostKeyOpaque.filter((o) => !o.resolvedVia);
  row.extraInHost = row.hostKeys.filter((k) => !row.responseKeys.includes(k));
  row.missingFromHost = row.responseKeys.filter((k) => !row.hostKeys.includes(k));
}

// ---------------------------------------------------------------- 8. output ---

const verdictCounts = rows.reduce((acc, r) => {
  acc[r.verdict] = (acc[r.verdict] ?? 0) + 1;
  return acc;
}, {});

const report = {
  root: ROOT,
  counts: {
    ipcSchemasKeys: rows.length,
    ipcCommandsEntries: commandPairs.length,
    hostDispatchEntries: hostDispatch.size,
    sidecarMethods: sidecarMethods.size,
    notImplementedSites: allNotImplemented.length,
    ...verdictCounts,
  },
  rows,
  notImplementedSites: allNotImplemented.map((line) => ({ line, text: routerLines[line - 1].trim() })),
  genericNotImplemented: genericNotImplemented[0] ?? null,
  sidecarMethods: [...sidecarMethods].map(([method, v]) => ({ method, ...v })).sort((a, b) => a.method.localeCompare(b.method)),
  kMethodsMaps: kMethodsMaps.map((k) => ({ line: k.line, entries: [...k.pairs].map(([a, b]) => ({ command: a, method: b })) })),
  commandsMissingFromHost: rows.filter((r) => r.host.status === "missing").map((r) => r.command),
  schemasMissingFromCommands: rows.filter((r) => r.camel === null).map((r) => r.command),
  commandsMissingFromSchemas: commandPairs.filter((p) => !rows.some((r) => r.command === p.literal)).map((p) => p.literal),
};

const outPath = path.join(ROOT, "target/r2/contract-matrix.json");
fs.mkdirSync(path.dirname(outPath), { recursive: true });
fs.writeFileSync(outPath, `${JSON.stringify(report, null, 2)}\n`);

if (process.argv.includes("--json")) process.stdout.write(`${JSON.stringify(report, null, 2)}\n`);
else {
  process.stdout.write(
    [
      `IPC_SCHEMAS keys              : ${rows.length}`,
      `IPC_COMMANDS entries          : ${commandPairs.length}`,
      `host dispatch entries         : ${hostDispatch.size}`,
      `sidecar methods known         : ${sidecarMethods.size}`,
      `NOT_IMPLEMENTED sites         : ${allNotImplemented.length}`,
      ``,
      `schemas without a command name: ${report.schemasMissingFromCommands.length}`,
      `commands without a schema     : ${report.commandsMissingFromSchemas.length}`,
      `commands missing from host    : ${report.commandsMissingFromHost.length}`,
      ``,
      ...Object.entries(verdictCounts).sort((a, b) => b[1] - a[1]).map(([k, v]) => `${k.padEnd(30)} ${v}`),
      ``,
      `wrote ${path.relative(ROOT, outPath)}`,
    ].join("\n"),
  );
}
```

配套脚本（同目录，均为只读扫描）：

| 脚本 | 作用 |
| --- | --- |
| `run-all.ps1` | **一次性重放全部 9 步**（顺序有依赖）。重放后 `docs-links` 与 `publication` 两个门禁的实测结果一并打印 |
| `audit-event-producers.mjs` | `EVENT_NAMES` 逐个在 `native/`+`agent/` 做 `git grep -c --fixed-strings` → `event-producers.json` |
| `scan-liars.mjs` | `git ls-files` 全量扫"已实现/Rust 发出/引用已删路径"断言 → `liars.json` |
| `audit-sidecar-returns.mjs` | 从 `sidecar.py:258-305` 的 `elif method ==` 链解析 handler，取每个 handler 的 `return` 字面量键集 → `sidecar-returns.json` |
| `audit-strictness.mjs` | 86 行响应 schema 的**根构造子**（strictObject / object / EMPTY_PAYLOAD）→ `response-strictness.json` |
| `audit-optionality.mjs` | 深度 0 的 key 逐一分 required/optional（先屏蔽注释再按逗号切分）→ `response-optionality.json` |
| `audit-response-diff.mjs` | 把上面五份合并成表 2 的判定 → `response-diff.json`、`table2.md` —— **表 2 的唯一作者** |
| `audit-ni-reach.mjs` | 5 个 NI 站点的字面量可达性 → §四 |
| `render-report.mjs` | 渲染表 1 / 表 3 / 表 4（**不写表 2**） |
| `build-report.mjs` | 组装本文件（含附录 A/B） |
| `verify-report.mjs` / `verify-report2.mjs` | 交付前自检：附录 JSON 能解析、生成器逐字一致、表 1 有 86 行 |

> **一个必须记住的坑**：`render-report.mjs` 早期版本内含一份**就地渲染表 2** 的旧代码，与 `audit-response-diff.mjs` 竞争写同一个 `table2.md`。谁后跑谁的版本生效——于是本报告「表 2」一节一度嵌入了更旧的一份（`zod 根构造` 整列显示 `?`、判定退化为"会抛"）。该旧代码已删除并留了注释；表 2 现在只由 `audit-response-diff.mjs` 产出。**改动这两个脚本中的任何一个之后，都必须跑一遍 `run-all.ps1`**，不要单跑其中一个。

## 附录 B — 原始输出

### B.1 生成器 stdout（逐字）

```
IPC_SCHEMAS keys              : 86
IPC_COMMANDS entries          : 86
host dispatch entries         : 89
sidecar methods known         : 27
NOT_IMPLEMENTED sites         : 5

schemas without a command name: 0
commands without a schema     : 0
commands missing from host    : 0

端到端可达                          81
UI 无调用点（死命令）                   3
仅 preview fixture 使用           2

wrote target\r2\contract-matrix.json
```

### B.2 `target/r2/contract-matrix.json`

**去噪视图**（每行一条 row，便于 grep）：删掉三个**仅供脚本内部判断、不构成证据**的字段（`handlerUnionKeys`、`handlerOpaque`、`siblingOnlyKeys`），其余**一字未改**（含 `rows[].hostKeys`、`extraInHost`、`missingFromHost`、`hostKeyOpaque`、`hostKeyResolved`、`ui`、`sidecar` 等全部原始字段）。**完整 368 KB 美化版本**同时写盘在 `target/r2/contract-matrix.json`（未删字段）。

```json
{
  "root": "D:\\yukinal",
  "counts": {"ipcSchemasKeys":86,"ipcCommandsEntries":86,"hostDispatchEntries":89,"sidecarMethods":27,"notImplementedSites":5,"端到端可达":81,"仅 preview fixture 使用":2,"UI 无调用点（死命令）":3},
  "notImplementedSites": [{"line":7643,"text":"return failure(\"NOT_IMPLEMENTED\","},{"line":7914,"text":"return failure(\"NOT_IMPLEMENTED\","},{"line":9072,"text":"return failure(\"NOT_IMPLEMENTED\", \"This Agent command has not migrated yet: \" + method);"},{"line":9089,"text":"return failure(\"NOT_IMPLEMENTED\", \"This Agent run command has not migrated yet: \" + method);"},{"line":9654,"text":"return failure(\"NOT_IMPLEMENTED\", \"This provider command has not migrated yet: \" + method);"}],
  "genericNotImplemented": 7643,
  "sidecarMethods": [{"method":"agent.approval.respond","line":73,"table":"METHODS","dispatch":290,"async":true},{"method":"agent.context.delete","line":76,"table":"METHODS","dispatch":296,"async":true},{"method":"agent.context.reset","line":74,"table":"METHODS","dispatch":292,"async":true},{"method":"agent.context.resetAll","line":75,"table":"METHODS","dispatch":294,"async":true},{"method":"agent.memory.delete","line":79,"table":"METHODS","dispatch":302,"async":true},{"method":"agent.memory.list","line":77,"table":"METHODS","dispatch":298,"async":true},{"method":"agent.memory.save","line":78,"table":"METHODS","dispatch":300,"async":true},{"method":"agent.run.start","line":71,"table":"METHODS","dispatch":286,"async":true},{"method":"agent.run.stop","line":72,"table":"METHODS","dispatch":288,"async":true},{"method":"docker.inspect.normalize","line":66,"table":"METHODS","dispatch":202},{"method":"docker.logs.normalize","line":65,"table":"METHODS","dispatch":200},{"method":"docker.ps.normalize","line":64,"table":"METHODS","dispatch":198},{"method":"initialize","line":59,"table":"METHODS","async":true},{"method":"package.inspect.normalize","line":68,"table":"METHODS","dispatch":206},{"method":"provider.activate","line":85,"table":"METHODS","dispatch":284,"async":true},{"method":"provider.delete","line":84,"table":"METHODS","dispatch":282,"async":true},{"method":"provider.list","line":82,"table":"METHODS","dispatch":278,"async":true},{"method":"provider.models","line":80,"table":"METHODS","dispatch":274,"async":true},{"method":"provider.save","line":83,"table":"METHODS","dispatch":280,"async":true},{"method":"provider.test","line":81,"table":"METHODS","dispatch":276,"async":true},{"method":"server.logs.normalize","line":62,"table":"METHODS","dispatch":194},{"method":"server.services.normalize","line":63,"table":"METHODS","dispatch":196},{"method":"server.snapshot.normalize","line":61,"table":"METHODS","dispatch":192},{"method":"system.describe","line":70,"table":"METHODS","dispatch":211},{"method":"system.ping","line":60,"table":"METHODS","dispatch":190},{"method":"systemd.inspect.normalize","line":67,"table":"METHODS","dispatch":204},{"method":"tools.list","line":69,"table":"METHODS","dispatch":208}],
  "kMethodsMaps": [{"line":9077,"entries":[{"command":"agent_run_start","method":"agent.run.start"},{"command":"agent_run_stop","method":"agent.run.stop"},{"command":"agent_approval_respond","method":"agent.approval.respond"},{"command":"agent_context_reset","method":"agent.context.reset"},{"command":"agent_context_reset_all","method":"agent.context.resetAll"},{"command":"agent_memory_list","method":"agent.memory.list"},{"command":"agent_memory_save","method":"agent.memory.save"},{"command":"agent_memory_delete","method":"agent.memory.delete"}]},{"line":9644,"entries":[{"command":"provider_list","method":"provider.list"},{"command":"provider_save","method":"provider.save"},{"command":"provider_activate","method":"provider.activate"},{"command":"provider_delete","method":"provider.delete"},{"command":"provider_models","method":"provider.models"},{"command":"provider_test","method":"provider.test"}]}],
  "commandsMissingFromHost": [],
  "schemasMissingFromCommands": [],
  "commandsMissingFromSchemas": [],
  "rows": [
    {"command":"core_ping","camel":"corePing","line":228,"host":{"status":"inline","handler":null,"line":7138},"sidecar":{"status":"host-local","method":null,"line":null},"ui":[{"file":"apps/desktop/src/lib/runtime.ts","line":26,"text":"queryFn: () => callDesktop(IPC_COMMANDS.corePing, {}),"}],"uiLiteral":[],"fixture":{"file":"apps/desktop/src/lib/preview-fixtures.ts","line":429},"verdict":"端到端可达","responseKeys":["version","os"],"responseKind":"named","responseVia":"packages/shared/src/schemas/ipc.ts:116","hostKeys":["os","version"],"hostKeyOpaque":[],"hostKeyScope":{"from":7138,"to":7143,"via":"explicit-branch","handler":"invoke"},"hostKeyExcluded":[{"from":7139,"to":7141,"names":[],"notNames":[],"prefix":null}],"hostKeyShapeGuards":[],"hostKeySource":"native/desktop_host/src/ipc_router.cpp:7138,7142-7143","hostKeyNarrowed":true,"extraInHost":[],"missingFromHost":[],"hostKeyResolved":[]},
    {"command":"server_list","camel":"serverList","line":229,"host":{"status":"inline","handler":null,"line":7145},"sidecar":{"status":"host-local","method":null,"line":null},"ui":[{"file":"apps/desktop/src/lib/servers.ts","line":14,"text":"queryFn: async () => (await callDesktop(IPC_COMMANDS.serverList, {})).servers,"}],"uiLiteral":[],"fixture":{"file":"apps/desktop/src/lib/preview-fixtures.ts","line":431},"verdict":"端到端可达","responseKeys":["servers"],"responseKind":"named","responseVia":"packages/shared/src/schemas/ipc.ts:121","hostKeys":["servers"],"hostKeyOpaque":[],"hostKeyScope":{"from":7145,"to":7157,"via":"explicit-branch","handler":"invoke"},"hostKeyExcluded":[{"from":7146,"to":7148,"names":[],"notNames":[],"prefix":null},{"from":7149,"to":7151,"names":[],"notNames":[],"prefix":null}],"hostKeyShapeGuards":[],"hostKeySource":"native/desktop_host/src/ipc_router.cpp:7145,7152-7157","hostKeyNarrowed":true,"extraInHost":[],"missingFromHost":[],"hostKeyResolved":[{"callee":"list_servers","file":"native/desktop_host/src/database.cpp","line":1534,"keys":["servers"]}]},
    {"command":"workspace_list","camel":"workspaceList","line":230,"host":{"status":"inline","handler":null,"line":7159},"sidecar":{"status":"host-local","method":null,"line":null},"ui":[{"file":"apps/desktop/src/features/projects/ProjectsPane.tsx","line":25,"text":"callDesktop(IPC_COMMANDS.workspaceList, {}),"}],"uiLiteral":[],"fixture":{"file":"apps/desktop/src/lib/preview-fixtures.ts","line":444},"verdict":"端到端可达","responseKeys":["workspaces"],"responseKind":"named","responseVia":"packages/shared/src/schemas/server.ts:128","hostKeys":["workspaces"],"hostKeyOpaque":[],"hostKeyScope":{"from":7159,"to":7171,"via":"explicit-branch","handler":"invoke"},"hostKeyExcluded":[{"from":7160,"to":7162,"names":[],"notNames":[],"prefix":null},{"from":7163,"to":7165,"names":[],"notNames":[],"prefix":null}],"hostKeyShapeGuards":[],"hostKeySource":"native/desktop_host/src/ipc_router.cpp:7159,7166-7171","hostKeyNarrowed":true,"extraInHost":[],"missingFromHost":[],"hostKeyResolved":[{"callee":"list_workspaces","file":"native/desktop_host/src/database.cpp","line":1807,"keys":["workspaces"]}]},
    {"command":"server_add","camel":"serverAdd","line":231,"host":{"status":"handler","handler":"server_command","line":6035},"sidecar":{"status":"n/a","method":null,"line":null,"reason":null},"ui":[{"file":"apps/desktop/src/features/servers/AddServerModal.tsx","line":96,"text":": callDesktop(IPC_COMMANDS.serverAdd, buildServerInput(values));"}],"uiLiteral":[],"fixture":null,"verdict":"端到端可达","responseKeys":["server"],"responseKind":"named","responseVia":"packages/shared/src/schemas/ipc.ts:122","hostKeys":["server"],"hostKeyOpaque":[],"hostKeyScope":{"from":8141,"to":8352,"via":"fall-through","handler":"server_command"},"hostKeyExcluded":[{"from":8143,"to":8145,"names":[],"notNames":[],"prefix":null},{"from":8146,"to":8148,"names":[],"notNames":[],"prefix":null},{"from":8150,"to":8179,"names":["server_delete"],"notNames":[],"prefix":null},{"from":8155,"to":8157,"names":[],"notNames":[],"prefix":null},{"from":8162,"to":8164,"names":[],"notNames":[],"prefix":null},{"from":8167,"to":8177,"names":[],"notNames":[],"prefix":null},{"from":8171,"to":8173,"names":[],"notNames":[],"prefix":null},{"from":8183,"to":8185,"names":[],"notNames":[],"prefix":null},{"from":8191,"to":8203,"names":[],"notNames":[],"prefix":null},{"from":8209,"to":8211,"names":[],"notNames":[],"prefix":null},{"from":8214,"to":8223,"names":["identity"],"notNames":[],"prefix":null},{"from":8217,"to":8219,"names":[],"notNames":[],"prefix":null},{"from":8237,"to":8241,"names":["password"],"notNames":[],"prefix":null},{"from":8245,"to":8255,"names":[],"notNames":[],"prefix":null},{"from":8252,"to":8254,"names":[],"notNames":[],"prefix":null},{"from":8266,"to":8273,"names":["certificate"],"notNames":[],"prefix":null},{"from":8269,"to":8272,"names":[],"notNames":[],"prefix":null},{"from":8291,"to":8304,"names":[],"notNames":[],"prefix":null},{"from":8319,"to":8321,"names":[],"notNames":[],"prefix":null},{"from":8337,"to":8349,"names":[],"notNames":[],"prefix":null},{"from":8338,"to":8348,"names":[],"notNames":[],"prefix":null},{"from":8342,"to":8344,"names":[],"notNames":[],"prefix":null}],"hostKeyShapeGuards":[],"hostKeySource":"native/desktop_host/src/ipc_router.cpp:8141-8142,8149,8180-8182,8186-8190,8204-8208,8212-8213,8224-8236,8242-8244,8256-8265,8274-8290,8305-8318,8322-8336,8350-8352","hostKeyNarrowed":true,"extraInHost":[],"missingFromHost":[],"hostKeyResolved":[]},
    {"command":"server_update","camel":"serverUpdate","line":232,"host":{"status":"handler","handler":"server_command","line":6035},"sidecar":{"status":"n/a","method":null,"line":null,"reason":null},"ui":[{"file":"apps/desktop/src/features/servers/AddServerModal.tsx","line":95,"text":"? callDesktop(IPC_COMMANDS.serverUpdate, buildServerInput(values, server.id))"}],"uiLiteral":[],"fixture":null,"verdict":"端到端可达","responseKeys":["server"],"responseKind":"named","responseVia":"packages/shared/src/schemas/ipc.ts:122","hostKeys":["server"],"hostKeyOpaque":[],"hostKeyScope":{"from":8141,"to":8352,"via":"fall-through","handler":"server_command"},"hostKeyExcluded":[{"from":8143,"to":8145,"names":[],"notNames":[],"prefix":null},{"from":8146,"to":8148,"names":[],"notNames":[],"prefix":null},{"from":8150,"to":8179,"names":["server_delete"],"notNames":[],"prefix":null},{"from":8155,"to":8157,"names":[],"notNames":[],"prefix":null},{"from":8162,"to":8164,"names":[],"notNames":[],"prefix":null},{"from":8167,"to":8177,"names":[],"notNames":[],"prefix":null},{"from":8171,"to":8173,"names":[],"notNames":[],"prefix":null},{"from":8183,"to":8185,"names":[],"notNames":[],"prefix":null},{"from":8191,"to":8203,"names":[],"notNames":[],"prefix":null},{"from":8209,"to":8211,"names":[],"notNames":[],"prefix":null},{"from":8214,"to":8223,"names":["identity"],"notNames":[],"prefix":null},{"from":8217,"to":8219,"names":[],"notNames":[],"prefix":null},{"from":8237,"to":8241,"names":["password"],"notNames":[],"prefix":null},{"from":8245,"to":8255,"names":[],"notNames":[],"prefix":null},{"from":8252,"to":8254,"names":[],"notNames":[],"prefix":null},{"from":8266,"to":8273,"names":["certificate"],"notNames":[],"prefix":null},{"from":8269,"to":8272,"names":[],"notNames":[],"prefix":null},{"from":8291,"to":8304,"names":[],"notNames":[],"prefix":null},{"from":8319,"to":8321,"names":[],"notNames":[],"prefix":null},{"from":8337,"to":8349,"names":[],"notNames":[],"prefix":null},{"from":8338,"to":8348,"names":[],"notNames":[],"prefix":null},{"from":8342,"to":8344,"names":[],"notNames":[],"prefix":null}],"hostKeyShapeGuards":[],"hostKeySource":"native/desktop_host/src/ipc_router.cpp:8141-8142,8149,8180-8182,8186-8190,8204-8208,8212-8213,8224-8236,8242-8244,8256-8265,8274-8290,8305-8318,8322-8336,8350-8352","hostKeyNarrowed":true,"extraInHost":[],"missingFromHost":[],"hostKeyResolved":[]},
    {"command":"server_delete","camel":"serverDelete","line":233,"host":{"status":"handler","handler":"server_command","line":6035},"sidecar":{"status":"n/a","method":null,"line":null,"reason":null},"ui":[{"file":"apps/desktop/src/lib/servers.ts","line":28,"text":"const command = type === \"connect\" ? IPC_COMMANDS.serverConnect : type === \"disconnect\" ? IPC_COMMANDS.serverD"}],"uiLiteral":[],"fixture":{"file":"apps/desktop/src/lib/preview-fixtures.ts","line":439},"verdict":"端到端可达","responseKeys":["deleted"],"responseKind":"inline-strict","responseVia":null,"hostKeys":["deleted"],"hostKeyOpaque":[],"hostKeyScope":{"from":8150,"to":8179,"via":"explicit-branch","handler":"server_command"},"hostKeyExcluded":[{"from":8155,"to":8157,"names":[],"notNames":[],"prefix":null},{"from":8162,"to":8164,"names":[],"notNames":[],"prefix":null},{"from":8167,"to":8177,"names":[],"notNames":[],"prefix":null},{"from":8171,"to":8173,"names":[],"notNames":[],"prefix":null}],"hostKeyShapeGuards":[],"hostKeySource":"native/desktop_host/src/ipc_router.cpp:8150-8154,8158-8161,8165-8166,8178-8179","hostKeyNarrowed":true,"extraInHost":[],"missingFromHost":[],"hostKeyResolved":[]},
    {"command":"server_connect","camel":"serverConnect","line":237,"host":{"status":"handler","handler":"connection_command","line":6055},"sidecar":{"status":"n/a","method":null,"line":null,"reason":null},"ui":[{"file":"apps/desktop/src/lib/servers.ts","line":28,"text":"const command = type === \"connect\" ? IPC_COMMANDS.serverConnect : type === \"disconnect\" ? IPC_COMMANDS.serverD"}],"uiLiteral":[],"fixture":{"file":"apps/desktop/src/lib/preview-fixtures.ts","line":433},"verdict":"端到端可达","responseKeys":["status"],"responseKind":"named","responseVia":"packages/shared/src/schemas/ipc.ts:123","hostKeys":["status"],"hostKeyOpaque":[],"hostKeyScope":{"from":8760,"to":8846,"via":"fall-through","handler":"connection_command"},"hostKeyExcluded":[{"from":8762,"to":8764,"names":[],"notNames":[],"prefix":null},{"from":8766,"to":8793,"names":["server_auth_respond","server_auth_cancel"],"notNames":[],"prefix":null},{"from":8774,"to":8776,"names":[],"notNames":[],"prefix":null},{"from":8778,"to":8780,"names":[],"notNames":[],"prefix":null},{"from":8781,"to":8791,"names":[],"notNames":[],"prefix":null},{"from":8783,"to":8785,"names":[],"notNames":[],"prefix":null},{"from":8787,"to":8789,"names":[],"notNames":[],"prefix":null},{"from":8799,"to":8801,"names":[],"notNames":[],"prefix":null},{"from":8803,"to":8815,"names":["server_disconnect"],"notNames":[],"prefix":null},{"from":8824,"to":8835,"names":[],"notNames":[],"prefix":null}],"hostKeyShapeGuards":[],"hostKeySource":"native/desktop_host/src/ipc_router.cpp:8760-8761,8765,8794-8798,8802,8816-8823,8836-8846","hostKeyNarrowed":true,"extraInHost":[],"missingFromHost":[],"hostKeyResolved":[]},
    {"command":"server_disconnect","camel":"serverDisconnect","line":241,"host":{"status":"handler","handler":"connection_command","line":6055},"sidecar":{"status":"n/a","method":null,"line":null,"reason":null},"ui":[{"file":"apps/desktop/src/lib/servers.ts","line":28,"text":"const command = type === \"connect\" ? IPC_COMMANDS.serverConnect : type === \"disconnect\" ? IPC_COMMANDS.serverD"}],"uiLiteral":[],"fixture":{"file":"apps/desktop/src/lib/preview-fixtures.ts","line":436},"verdict":"端到端可达","responseKeys":[],"responseKind":"empty","responseVia":null,"hostKeys":[],"hostKeyOpaque":[],"hostKeyScope":{"from":8803,"to":8815,"via":"explicit-branch","handler":"connection_command"},"hostKeyExcluded":[],"hostKeyShapeGuards":[],"hostKeySource":"native/desktop_host/src/ipc_router.cpp:8803-8815","hostKeyNarrowed":true,"extraInHost":[],"missingFromHost":[],"hostKeyResolved":[]},
    {"command":"server_auth_respond","camel":"serverAuthRespond","line":245,"host":{"status":"handler","handler":"connection_command","line":6056},"sidecar":{"status":"n/a","method":null,"line":null,"reason":null},"ui":[{"file":"apps/desktop/src/features/servers/ServerAuthChallengeModal.tsx","line":72,"text":"const { accepted } = await callDesktop(IPC_COMMANDS.serverAuthRespond, {"}],"uiLiteral":[],"fixture":null,"verdict":"端到端可达","responseKeys":["accepted"],"responseKind":"inline-strict","responseVia":null,"hostKeys":["accepted"],"hostKeyOpaque":[],"hostKeyScope":{"from":8766,"to":8793,"via":"explicit-branch","handler":"connection_command"},"hostKeyExcluded":[{"from":8774,"to":8776,"names":[],"notNames":[],"prefix":null},{"from":8778,"to":8780,"names":[],"notNames":[],"prefix":null},{"from":8781,"to":8791,"names":[],"notNames":[],"prefix":null},{"from":8783,"to":8785,"names":[],"notNames":[],"prefix":null},{"from":8787,"to":8789,"names":[],"notNames":[],"prefix":null}],"hostKeyShapeGuards":[],"hostKeySource":"native/desktop_host/src/ipc_router.cpp:8766-8773,8777,8792-8793","hostKeyNarrowed":true,"extraInHost":[],"missingFromHost":[],"hostKeyResolved":[]},
    {"command":"server_auth_cancel","camel":"serverAuthCancel","line":252,"host":{"status":"handler","handler":"connection_command","line":6056},"sidecar":{"status":"n/a","method":null,"line":null,"reason":null},"ui":[{"file":"apps/desktop/src/features/servers/ServerAuthChallengeModal.tsx","line":91,"text":"const { accepted } = await callDesktop(IPC_COMMANDS.serverAuthCancel, {"}],"uiLiteral":[],"fixture":null,"verdict":"端到端可达","responseKeys":["accepted"],"responseKind":"inline-strict","responseVia":null,"hostKeys":["accepted"],"hostKeyOpaque":[],"hostKeyScope":{"from":8766,"to":8793,"via":"explicit-branch","handler":"connection_command"},"hostKeyExcluded":[{"from":8774,"to":8776,"names":[],"notNames":[],"prefix":null},{"from":8778,"to":8780,"names":[],"notNames":[],"prefix":null},{"from":8781,"to":8791,"names":[],"notNames":[],"prefix":null},{"from":8783,"to":8785,"names":[],"notNames":[],"prefix":null},{"from":8787,"to":8789,"names":[],"notNames":[],"prefix":null}],"hostKeyShapeGuards":[],"hostKeySource":"native/desktop_host/src/ipc_router.cpp:8766-8773,8777,8792-8793","hostKeyNarrowed":true,"extraInHost":[],"missingFromHost":[],"hostKeyResolved":[]},
    {"command":"server_host_key_status","camel":"serverHostKeyStatus","line":256,"host":{"status":"handler","handler":"host_key_command","line":6044},"sidecar":{"status":"n/a","method":null,"line":null,"reason":null},"ui":[{"file":"apps/desktop/src/lib/host-key.ts","line":41,"text":"queryFn: async () => callDesktop(IPC_COMMANDS.serverHostKeyStatus, { serverId }),"}],"uiLiteral":[],"fixture":{"file":"apps/desktop/src/lib/preview-fixtures.ts","line":580},"verdict":"端到端可达","responseKeys":["host","port","pinned","pinnedFingerprint"],"responseKind":"named","responseVia":"packages/shared/src/schemas/ipc.ts:185","hostKeys":["host","pinned","pinnedFingerprint","port"],"hostKeyOpaque":[],"hostKeyScope":{"from":8494,"to":8499,"via":"explicit-branch","handler":"host_key_command"},"hostKeyExcluded":[],"hostKeyShapeGuards":[],"hostKeySource":"native/desktop_host/src/ipc_router.cpp:8494-8499","hostKeyNarrowed":true,"extraInHost":[],"missingFromHost":[],"hostKeyResolved":[]},
    {"command":"server_host_key_probe","camel":"serverHostKeyProbe","line":260,"host":{"status":"handler","handler":"server_read_command","line":6048},"sidecar":{"status":"n/a","method":null,"line":null,"reason":null},"ui":[{"file":"apps/desktop/src/lib/host-key.ts","line":72,"text":"mutationFn: () => callDesktop(IPC_COMMANDS.serverHostKeyProbe, { serverId }),"}],"uiLiteral":[],"fixture":null,"verdict":"端到端可达","responseKeys":["host","port","probeTicket","presentedFingerprint","comparison","pinnedFingerprint"],"responseKind":"named","responseVia":"packages/shared/src/schemas/ipc.ts:192","hostKeys":["comparison","host","pinnedFingerprint","port","presentedFingerprint","probeTicket"],"hostKeyOpaque":[],"hostKeyScope":{"from":8605,"to":8636,"via":"explicit-branch","handler":"server_read_command"},"hostKeyExcluded":[{"from":8612,"to":8615,"names":[],"notNames":[],"prefix":null},{"from":8617,"to":8620,"names":[],"notNames":[],"prefix":null}],"hostKeyShapeGuards":[],"hostKeySource":"native/desktop_host/src/ipc_router.cpp:8605-8611,8616,8621-8636","hostKeyNarrowed":true,"extraInHost":[],"missingFromHost":[],"hostKeyResolved":[]},
    {"command":"server_host_key_trust","camel":"serverHostKeyTrust","line":264,"host":{"status":"handler","handler":"server_read_command","line":6048},"sidecar":{"status":"n/a","method":null,"line":null,"reason":null},"ui":[{"file":"apps/desktop/src/lib/host-key.ts","line":83,"text":"callDesktop(IPC_COMMANDS.serverHostKeyTrust, { serverId, probeTicket, fingerprint }),"}],"uiLiteral":[],"fixture":null,"verdict":"端到端可达","responseKeys":["host","port","fingerprint","alreadyPinned"],"responseKind":"named","responseVia":"packages/shared/src/schemas/ipc.ts:202","hostKeys":["alreadyPinned","fingerprint","host","port"],"hostKeyOpaque":[],"hostKeyScope":{"from":8577,"to":8666,"via":"explicit-branch","handler":"server_read_command"},"hostKeyExcluded":[{"from":8582,"to":8584,"names":[],"notNames":[],"prefix":null},{"from":8585,"to":8587,"names":[],"notNames":[],"prefix":null},{"from":8589,"to":8591,"names":[],"notNames":[],"prefix":null},{"from":8601,"to":8603,"names":[],"notNames":[],"prefix":null},{"from":8605,"to":8636,"names":["server_host_key_probe"],"notNames":[],"prefix":null},{"from":8612,"to":8615,"names":[],"notNames":[],"prefix":null},{"from":8617,"to":8620,"names":[],"notNames":[],"prefix":null},{"from":8639,"to":8641,"names":[],"notNames":[],"prefix":null},{"from":8644,"to":8646,"names":[],"notNames":[],"prefix":null},{"from":8652,"to":8656,"names":[],"notNames":[],"prefix":null}],"hostKeyShapeGuards":[],"hostKeySource":"native/desktop_host/src/ipc_router.cpp:8577-8581,8588,8592-8600,8604,8637-8638,8642-8643,8647-8651,8657-8666","hostKeyNarrowed":true,"extraInHost":[],"missingFromHost":[],"hostKeyResolved":[]},
    {"command":"server_host_key_forget","camel":"serverHostKeyForget","line":272,"host":{"status":"handler","handler":"host_key_command","line":6044},"sidecar":{"status":"n/a","method":null,"line":null,"reason":null},"ui":[{"file":"apps/desktop/src/lib/host-key.ts","line":88,"text":"mutationFn: () => callDesktop(IPC_COMMANDS.serverHostKeyForget, { serverId }),"}],"uiLiteral":[],"fixture":null,"verdict":"端到端可达","responseKeys":["host","port","forgotten"],"responseKind":"named","responseVia":"packages/shared/src/schemas/ipc.ts:209","hostKeys":["forgotten","host","port"],"hostKeyOpaque":[],"hostKeyScope":{"from":8464,"to":8509,"via":"fall-through","handler":"host_key_command"},"hostKeyExcluded":[{"from":8466,"to":8468,"names":[],"notNames":[],"prefix":null},{"from":8473,"to":8476,"names":[],"notNames":[],"prefix":null},{"from":8487,"to":8489,"names":[],"notNames":[],"prefix":null},{"from":8494,"to":8499,"names":["server_host_key_status"],"notNames":[],"prefix":null}],"hostKeyShapeGuards":[],"hostKeySource":"native/desktop_host/src/ipc_router.cpp:8464-8465,8469-8472,8477-8486,8490-8493,8500-8509","hostKeyNarrowed":true,"extraInHost":[],"missingFromHost":[],"hostKeyResolved":[]},
    {"command":"server_snapshot","camel":"serverSnapshot","line":276,"host":{"status":"handler","handler":"server_read_command","line":6050},"sidecar":{"status":"implemented","method":"server.snapshot.normalize","line":192,"reason":null,"details":[{"method":"server.snapshot.normalize","ok":true,"line":192}],"source":"native/desktop_host/src/ipc_router.cpp:8549-8759"},"ui":[{"file":"apps/desktop/src/features/overview/ServerOverview.tsx","line":172,"text":"return (await callDesktop(IPC_COMMANDS.serverSnapshot, { serverId: selectedServerId })).snapshot;"},{"file":"apps/desktop/src/features/servers/ServerTreeItem.tsx","line":23,"text":"queryFn: async () => (await callDesktop(IPC_COMMANDS.serverSnapshot, { serverId: server.id })).snapshot,"}],"uiLiteral":[],"fixture":{"file":"apps/desktop/src/lib/preview-fixtures.ts","line":465},"verdict":"端到端可达","responseKeys":["snapshot"],"responseKind":"named","responseVia":"packages/shared/src/schemas/ipc.ts:124","hostKeys":["snapshot"],"hostKeyOpaque":[],"hostKeyScope":{"from":8549,"to":8759,"via":"fall-through","handler":"server_read_command"},"hostKeyExcluded":[{"from":8551,"to":8553,"names":[],"notNames":[],"prefix":null},{"from":8554,"to":8576,"names":[],"notNames":[],"prefix":"remote_file_"},{"from":8556,"to":8559,"names":[],"notNames":[],"prefix":null},{"from":8562,"to":8564,"names":[],"notNames":[],"prefix":null},{"from":8566,"to":8568,"names":["remote_file_list"],"notNames":[],"prefix":null},{"from":8569,"to":8571,"names":["remote_file_read"],"notNames":[],"prefix":null},{"from":8577,"to":8666,"names":["server_host_key_probe","server_host_key_trust"],"notNames":[],"prefix":null},{"from":8582,"to":8584,"names":[],"notNames":[],"prefix":null},{"from":8585,"to":8587,"names":[],"notNames":[],"prefix":null},{"from":8589,"to":8591,"names":[],"notNames":[],"prefix":null},{"from":8601,"to":8603,"names":[],"notNames":[],"prefix":null},{"from":8605,"to":8636,"names":["server_host_key_probe"],"notNames":[],"prefix":null},{"from":8612,"to":8615,"names":[],"notNames":[],"prefix":null},{"from":8617,"to":8620,"names":[],"notNames":[],"prefix":null},{"from":8639,"to":8641,"names":[],"notNames":[],"prefix":null},{"from":8644,"to":8646,"names":[],"notNames":[],"prefix":null},{"from":8652,"to":8656,"names":[],"notNames":[],"prefix":null},{"from":8672,"to":8674,"names":[],"notNames":[],"prefix":null},{"from":8676,"to":8712,"names":["server_logs","server_services"],"notNames":[],"prefix":null},{"from":8685,"to":8687,"names":[],"notNames":[],"prefix":null},{"from":8695,"to":8700,"names":[],"notNames":[],"prefix":null},{"from":8701,"to":8703,"names":[],"notNames":[],"prefix":null},{"from":8728,"to":8731,"names":[],"notNames":[],"prefix":null}],"hostKeyShapeGuards":[],"hostKeySource":"native/desktop_host/src/ipc_router.cpp:8549-8550,8667-8671,8675,8713-8727,8732-8759","hostKeyNarrowed":true,"extraInHost":[],"missingFromHost":[],"hostKeyResolved":[]},
    {"command":"server_services","camel":"serverServices","line":280,"host":{"status":"handler","handler":"server_read_command","line":6049},"sidecar":{"status":"implemented","method":"server.snapshot.normalize","line":192,"reason":null,"details":[{"method":"server.snapshot.normalize","ok":true,"line":192}],"source":"native/desktop_host/src/ipc_router.cpp:8676-8759"},"ui":[{"file":"apps/desktop/src/features/services/ServicesPane.tsx","line":42,"text":"return callDesktop(IPC_COMMANDS.serverServices, { serverId: selectedServerId });"}],"uiLiteral":[],"fixture":{"file":"apps/desktop/src/lib/preview-fixtures.ts","line":467},"verdict":"端到端可达","responseKeys":["source","services","message"],"responseKind":"named","responseVia":"packages/shared/src/schemas/service.ts:17","hostKeys":["message","services","source"],"hostKeyOpaque":[{"callee":null,"passthrough":"output","text":"normalized.at(\"output\")};"},{"callee":null,"pyForward":"server.services.normalize","file":"agent/src/yukinal_agent/ssh_tools.py","line":2168,"keys":["message","services","source"],"text":"_parse_service_inventory → agent/src/yukinal_agent/ssh_tools.py:2168"}],"hostKeyScope":{"from":8676,"to":8712,"via":"explicit-branch","handler":"server_read_command"},"hostKeyExcluded":[{"from":8685,"to":8687,"names":[],"notNames":[],"prefix":null},{"from":8695,"to":8700,"names":[],"notNames":[],"prefix":null},{"from":8701,"to":8703,"names":[],"notNames":[],"prefix":null}],"hostKeyShapeGuards":[],"hostKeySource":"native/desktop_host/src/ipc_router.cpp:8676-8684,8688-8694,8704-8712","hostKeyNarrowed":true,"extraInHost":[],"missingFromHost":[],"hostKeyResolved":[]},
    {"command":"server_logs","camel":"serverLogs","line":284,"host":{"status":"handler","handler":"server_read_command","line":6049},"sidecar":{"status":"implemented","method":"server.snapshot.normalize","line":192,"reason":null,"details":[{"method":"server.snapshot.normalize","ok":true,"line":192}],"source":"native/desktop_host/src/ipc_router.cpp:8676-8759"},"ui":[{"file":"apps/desktop/src/features/logs/LogsPane.tsx","line":42,"text":"return callDesktop(IPC_COMMANDS.serverLogs, { serverId: selectedServerId });"}],"uiLiteral":[],"fixture":{"file":"apps/desktop/src/lib/preview-fixtures.ts","line":476},"verdict":"端到端可达","responseKeys":["source","lines","message"],"responseKind":"named","responseVia":"packages/shared/src/schemas/log.ts:20","hostKeys":["lines","message","source"],"hostKeyOpaque":[{"callee":null,"passthrough":"output","text":"normalized.at(\"output\")};"},{"callee":null,"pyForward":"server.logs.normalize","file":"agent/src/yukinal_agent/ssh_tools.py","line":2235,"keys":["lines","message","source"],"text":"_parse_server_logs → agent/src/yukinal_agent/ssh_tools.py:2235"}],"hostKeyScope":{"from":8676,"to":8712,"via":"explicit-branch","handler":"server_read_command"},"hostKeyExcluded":[{"from":8685,"to":8687,"names":[],"notNames":[],"prefix":null},{"from":8695,"to":8700,"names":[],"notNames":[],"prefix":null},{"from":8701,"to":8703,"names":[],"notNames":[],"prefix":null}],"hostKeyShapeGuards":[],"hostKeySource":"native/desktop_host/src/ipc_router.cpp:8676-8684,8688-8694,8704-8712","hostKeyNarrowed":true,"extraInHost":[],"missingFromHost":[],"hostKeyResolved":[]},
    {"command":"remote_file_list","camel":"remoteFileList","line":288,"host":{"status":"handler","handler":"server_read_command","line":6050},"sidecar":{"status":"n/a","method":null,"line":null,"reason":null},"ui":[{"file":"apps/desktop/src/features/files/RemoteFilesPane.tsx","line":41,"text":"queryFn: () => callDesktop(IPC_COMMANDS.remoteFileList, { serverId: serverId as string, path }),"}],"uiLiteral":[],"fixture":{"file":"apps/desktop/src/lib/preview-fixtures.ts","line":486},"verdict":"端到端可达","responseKeys":["path","entries"],"responseKind":"inline-strict","responseVia":null,"hostKeys":["entries","path"],"hostKeyOpaque":[],"hostKeyScope":{"from":8566,"to":8568,"via":"explicit-branch","handler":"server_read_command"},"hostKeyExcluded":[],"hostKeyShapeGuards":[],"hostKeySource":"native/desktop_host/src/ipc_router.cpp:8566-8568","hostKeyNarrowed":true,"extraInHost":[],"missingFromHost":[],"hostKeyResolved":[{"callee":"run_remote_file_list","file":"native/desktop_host/src/desktop_ssh.cpp","line":548,"keys":["entries","path"]}]},
    {"command":"remote_file_read","camel":"remoteFileRead","line":292,"host":{"status":"handler","handler":"server_read_command","line":6051},"sidecar":{"status":"n/a","method":null,"line":null,"reason":null},"ui":[],"uiLiteral":[],"fixture":{"file":"apps/desktop/src/lib/preview-fixtures.ts","line":500},"verdict":"仅 preview fixture 使用","responseKeys":["path","content","truncated"],"responseKind":"inline-strict","responseVia":null,"hostKeys":["content","path","truncated"],"hostKeyOpaque":[],"hostKeyScope":{"from":8569,"to":8571,"via":"explicit-branch","handler":"server_read_command"},"hostKeyExcluded":[],"hostKeyShapeGuards":[],"hostKeySource":"native/desktop_host/src/ipc_router.cpp:8569-8571","hostKeyNarrowed":true,"extraInHost":[],"missingFromHost":[],"hostKeyResolved":[{"callee":"run_remote_file_read","file":"native/desktop_host/src/desktop_ssh.cpp","line":604,"keys":["content","path","truncated"]}]},
    {"command":"remote_file_preview","camel":"remoteFilePreview","line":296,"host":{"status":"handler","handler":"server_read_command","line":6051},"sidecar":{"status":"implemented","method":"server.snapshot.normalize","line":192,"reason":null,"details":[{"method":"server.snapshot.normalize","ok":true,"line":192}],"source":"native/desktop_host/src/ipc_router.cpp:8549-8759"},"ui":[{"file":"apps/desktop/src/features/files/RemoteFilesPane.tsx","line":46,"text":"queryFn: () => callDesktop(IPC_COMMANDS.remoteFilePreview, { serverId: serverId as string, path: selectedRemot"}],"uiLiteral":[],"fixture":{"file":"apps/desktop/src/lib/preview-fixtures.ts","line":506},"verdict":"端到端可达","responseKeys":["name","size","kind","truncated","text","dataUrl"],"responseKind":"named","responseVia":"packages/shared/src/schemas/file.ts:22","hostKeys":["dataUrl","kind","name","size","text","truncated"],"hostKeyOpaque":[],"hostKeyScope":{"from":8554,"to":8576,"via":"prefix-guard","handler":"server_read_command"},"hostKeyExcluded":[{"from":8556,"to":8559,"names":[],"notNames":[],"prefix":null},{"from":8562,"to":8564,"names":[],"notNames":[],"prefix":null},{"from":8566,"to":8568,"names":["remote_file_list"],"notNames":[],"prefix":null},{"from":8569,"to":8571,"names":["remote_file_read"],"notNames":[],"prefix":null}],"hostKeyShapeGuards":[],"hostKeySource":"native/desktop_host/src/ipc_router.cpp:8554-8555,8560-8561,8565,8572-8576","hostKeyNarrowed":true,"extraInHost":[],"missingFromHost":[],"hostKeyResolved":[{"callee":"run_remote_file_preview","file":"native/desktop_host/src/desktop_ssh.cpp","line":636,"keys":["dataUrl","kind","name","size","text","truncated"]}]},
    {"command":"file_prepare_remote_drag","camel":"filePrepareRemoteDrag","line":300,"host":{"status":"handler","handler":"remote_drag_prepare_command","line":7618},"sidecar":{"status":"n/a","method":null,"line":null,"reason":null},"ui":[{"file":"apps/desktop/src/features/files/RemoteFilesPane.tsx","line":145,"text":"const prepared = await callDesktop(IPC_COMMANDS.filePrepareRemoteDrag, { serverId, path: selectedRemote.path }"}],"uiLiteral":[],"fixture":null,"verdict":"端到端可达","responseKeys":["dragId","name","size"],"responseKind":"named","responseVia":"packages/shared/src/schemas/file.ts:41","hostKeys":["dragId","name","size"],"hostKeyOpaque":[],"hostKeyScope":{"from":7918,"to":8076,"via":"fall-through","handler":"remote_drag_prepare_command"},"hostKeyExcluded":[{"from":7923,"to":7925,"names":[],"notNames":[],"prefix":null},{"from":7928,"to":7930,"names":[],"notNames":[],"prefix":null},{"from":7931,"to":7933,"names":[],"notNames":[],"prefix":null},{"from":7935,"to":7937,"names":[],"notNames":[],"prefix":null},{"from":7949,"to":7951,"names":[],"notNames":[],"prefix":null},{"from":7954,"to":7957,"names":[],"notNames":[],"prefix":null},{"from":7964,"to":7966,"names":[],"notNames":[],"prefix":null},{"from":7967,"to":7970,"names":[],"notNames":[],"prefix":null},{"from":7971,"to":7974,"names":[],"notNames":[],"prefix":null},{"from":7982,"to":7985,"names":[],"notNames":[],"prefix":null},{"from":8013,"to":8025,"names":[],"notNames":[],"prefix":null},{"from":8020,"to":8022,"names":[],"notNames":[],"prefix":null},{"from":8026,"to":8033,"names":[],"notNames":[],"prefix":null},{"from":8028,"to":8030,"names":[],"notNames":[],"prefix":null},{"from":8041,"to":8044,"names":[],"notNames":[],"prefix":null},{"from":8048,"to":8051,"names":[],"notNames":[],"prefix":null},{"from":8054,"to":8056,"names":[],"notNames":[],"prefix":null},{"from":8060,"to":8062,"names":[],"notNames":[],"prefix":null}],"hostKeyShapeGuards":[],"hostKeySource":"native/desktop_host/src/ipc_router.cpp:7918-7922,7926-7927,7934,7938-7948,7952-7953,7958-7963,7975-7981,7986-8012,8034-8040,8045-8047,8052-8053,8057-8059,8063-8076","hostKeyNarrowed":true,"extraInHost":[],"missingFromHost":[],"hostKeyResolved":[{"callee":"transfer_error_reply","file":"native/desktop_host/src/ipc_router.cpp","line":5788,"keys":[]}]},
    {"command":"file_drag_out_start","camel":"fileDragOutStart","line":307,"host":{"status":"handler","handler":"remote_drag_start_command","line":7622},"sidecar":{"status":"n/a","method":null,"line":null,"reason":null},"ui":[{"file":"apps/desktop/src/features/files/RemoteFilesPane.tsx","line":151,"text":"void callDesktop(IPC_COMMANDS.fileDragOutStart, { dragId })"}],"uiLiteral":[],"fixture":null,"verdict":"端到端可达","responseKeys":[],"responseKind":"empty","responseVia":null,"hostKeys":[],"hostKeyOpaque":[],"hostKeyScope":{"from":8077,"to":8140,"via":"fall-through","handler":"remote_drag_start_command"},"hostKeyExcluded":[{"from":8082,"to":8084,"names":[],"notNames":[],"prefix":null},{"from":8092,"to":8094,"names":[],"notNames":[],"prefix":null},{"from":8095,"to":8097,"names":[],"notNames":[],"prefix":null},{"from":8104,"to":8106,"names":[],"notNames":[],"prefix":null},{"from":8133,"to":8137,"names":[],"notNames":[],"prefix":null}],"hostKeyShapeGuards":[],"hostKeySource":"native/desktop_host/src/ipc_router.cpp:8077-8081,8085-8091,8098-8103,8107-8132,8138-8140","hostKeyNarrowed":true,"extraInHost":[],"missingFromHost":[],"hostKeyResolved":[]},
    {"command":"local_file_preview","camel":"localFilePreview","line":311,"host":{"status":"inline","handler":null,"line":7572},"sidecar":{"status":"host-local","method":null,"line":null},"ui":[{"file":"apps/desktop/src/features/files/RemoteFilesPane.tsx","line":51,"text":"queryFn: () => callDesktop(IPC_COMMANDS.localFilePreview, { handleId: selectedLocal?.handleId as string }),"}],"uiLiteral":[],"fixture":{"file":"apps/desktop/src/lib/preview-fixtures.ts","line":508},"verdict":"端到端可达","responseKeys":["name","size","kind","truncated","text","dataUrl"],"responseKind":"named","responseVia":"packages/shared/src/schemas/file.ts:22","hostKeys":["dataUrl","kind","name","size","text","truncated"],"hostKeyOpaque":[],"hostKeyScope":{"from":7572,"to":7587,"via":"explicit-branch","handler":"invoke"},"hostKeyExcluded":[{"from":7577,"to":7579,"names":[],"notNames":[],"prefix":null}],"hostKeyShapeGuards":[],"hostKeySource":"native/desktop_host/src/ipc_router.cpp:7572-7576,7580-7587","hostKeyNarrowed":true,"extraInHost":[],"missingFromHost":[],"hostKeyResolved":[{"callee":"make_file_preview","file":"native/desktop_host/src/local_paths.cpp","line":381,"keys":["dataUrl","kind","name","size","text","truncated"]}]},
    {"command":"local_path_pick","camel":"localPathPick","line":315,"host":{"status":"inline","handler":null,"line":7589},"sidecar":{"status":"host-local","method":null,"line":null},"ui":[{"file":"apps/desktop/src/features/files/RemoteFilesPane.tsx","line":104,"text":"const response = await callDesktop(IPC_COMMANDS.localPathPick, { kind });"}],"uiLiteral":[],"fixture":{"file":"apps/desktop/src/lib/preview-fixtures.ts","line":510},"verdict":"端到端可达","responseKeys":["handles"],"responseKind":"inline-strict","responseVia":null,"hostKeys":["handles"],"hostKeyOpaque":[],"hostKeyScope":{"from":7589,"to":7610,"via":"explicit-branch","handler":"invoke"},"hostKeyExcluded":[{"from":7604,"to":7607,"names":[],"notNames":[],"prefix":null}],"hostKeyShapeGuards":[],"hostKeySource":"native/desktop_host/src/ipc_router.cpp:7589-7603,7608-7610","hostKeyNarrowed":true,"extraInHost":[],"missingFromHost":[],"hostKeyResolved":[]},
    {"command":"file_transfer_upload","camel":"fileTransferUpload","line":319,"host":{"status":"handler","handler":"transfer_command","line":7612},"sidecar":{"status":"n/a","method":null,"line":null,"reason":null},"ui":[{"file":"apps/desktop/src/features/files/RemoteFilesPane.tsx","line":122,"text":"await callDesktop(IPC_COMMANDS.fileTransferUpload, {"}],"uiLiteral":[],"fixture":null,"verdict":"端到端可达","responseKeys":["transfer"],"responseKind":"inline-strict","responseVia":null,"hostKeys":["transfer"],"hostKeyOpaque":[],"hostKeyScope":{"from":7726,"to":7774,"via":"explicit-branch","handler":"transfer_command"},"hostKeyExcluded":[{"from":7731,"to":7734,"names":[],"notNames":[],"prefix":null},{"from":7737,"to":7739,"names":[],"notNames":[],"prefix":null},{"from":7742,"to":7744,"names":[],"notNames":[],"prefix":null},{"from":7747,"to":7750,"names":[],"notNames":[],"prefix":null},{"from":7763,"to":7766,"names":[],"notNames":[],"prefix":null}],"hostKeyShapeGuards":[],"hostKeySource":"native/desktop_host/src/ipc_router.cpp:7726-7730,7735-7736,7740-7741,7745-7746,7751-7762,7767-7774","hostKeyNarrowed":true,"extraInHost":[],"missingFromHost":[],"hostKeyResolved":[{"callee":"transfer_error_reply","file":"native/desktop_host/src/ipc_router.cpp","line":5788,"keys":[]},{"callee":"transfer_snapshot_reply","file":"native/desktop_host/src/ipc_router.cpp","line":5780,"keys":["transfer"]}]},
    {"command":"file_transfer_download","camel":"fileTransferDownload","line":327,"host":{"status":"handler","handler":"transfer_command","line":7612},"sidecar":{"status":"n/a","method":null,"line":null,"reason":null},"ui":[{"file":"apps/desktop/src/features/files/RemoteFilesPane.tsx","line":134,"text":"await callDesktop(IPC_COMMANDS.fileTransferDownload, {"}],"uiLiteral":[],"fixture":null,"verdict":"端到端可达","responseKeys":["transfer"],"responseKind":"inline-strict","responseVia":null,"hostKeys":["transfer"],"hostKeyOpaque":[],"hostKeyScope":{"from":7776,"to":7830,"via":"explicit-branch","handler":"transfer_command"},"hostKeyExcluded":[{"from":7781,"to":7784,"names":[],"notNames":[],"prefix":null},{"from":7786,"to":7788,"names":[],"notNames":[],"prefix":null},{"from":7791,"to":7793,"names":[],"notNames":[],"prefix":null},{"from":7795,"to":7797,"names":[],"notNames":[],"prefix":null},{"from":7800,"to":7802,"names":[],"notNames":[],"prefix":null},{"from":7804,"to":7806,"names":[],"notNames":[],"prefix":null},{"from":7819,"to":7822,"names":[],"notNames":[],"prefix":null}],"hostKeyShapeGuards":[],"hostKeySource":"native/desktop_host/src/ipc_router.cpp:7776-7780,7785,7789-7790,7794,7798-7799,7803,7807-7818,7823-7830","hostKeyNarrowed":true,"extraInHost":[],"missingFromHost":[],"hostKeyResolved":[{"callee":"transfer_error_reply","file":"native/desktop_host/src/ipc_router.cpp","line":5788,"keys":[]},{"callee":"transfer_snapshot_reply","file":"native/desktop_host/src/ipc_router.cpp","line":5780,"keys":["transfer"]}]},
    {"command":"file_transfer_list","camel":"fileTransferList","line":335,"host":{"status":"handler","handler":"transfer_command","line":7613},"sidecar":{"status":"n/a","method":null,"line":null,"reason":null},"ui":[{"file":"apps/desktop/src/features/files/RemoteFilesPane.tsx","line":56,"text":"queryFn: async () => (await callDesktop(IPC_COMMANDS.fileTransferList, { serverId: serverId as string })).tran"}],"uiLiteral":[],"fixture":{"file":"apps/desktop/src/lib/preview-fixtures.ts","line":519},"verdict":"端到端可达","responseKeys":["transfers"],"responseKind":"inline-strict","responseVia":null,"hostKeys":["transfers"],"hostKeyOpaque":[],"hostKeyScope":{"from":7649,"to":7693,"via":"explicit-branch","handler":"transfer_command"},"hostKeyExcluded":[{"from":7654,"to":7663,"names":[],"notNames":[],"prefix":null},{"from":7655,"to":7657,"names":[],"notNames":[],"prefix":null},{"from":7659,"to":7661,"names":[],"notNames":[],"prefix":null},{"from":7670,"to":7682,"names":[],"notNames":[],"prefix":null},{"from":7673,"to":7675,"names":[],"notNames":[],"prefix":null}],"hostKeyShapeGuards":[],"hostKeySource":"native/desktop_host/src/ipc_router.cpp:7649-7653,7664-7669,7683-7693","hostKeyNarrowed":true,"extraInHost":[],"missingFromHost":[],"hostKeyResolved":[{"callee":"transfer_error_reply","file":"native/desktop_host/src/ipc_router.cpp","line":5788,"keys":[]}]},
    {"command":"file_transfer_get","camel":"fileTransferGet","line":339,"host":{"status":"handler","handler":"transfer_command","line":7613},"sidecar":{"status":"n/a","method":null,"line":null,"reason":null},"ui":[],"uiLiteral":[],"fixture":{"file":"apps/desktop/src/lib/preview-fixtures.ts","line":514},"verdict":"仅 preview fixture 使用","responseKeys":["transfer"],"responseKind":"inline-strict","responseVia":null,"hostKeys":["transfer"],"hostKeyOpaque":[],"hostKeyScope":{"from":7695,"to":7724,"via":"explicit-branch","handler":"transfer_command"},"hostKeyExcluded":[{"from":7700,"to":7703,"names":[],"notNames":[],"prefix":null},{"from":7711,"to":7713,"names":[],"notNames":[],"prefix":null},{"from":7716,"to":7718,"names":[],"notNames":[],"prefix":null}],"hostKeyShapeGuards":[],"hostKeySource":"native/desktop_host/src/ipc_router.cpp:7695-7699,7704-7710,7714-7715,7719-7724","hostKeyNarrowed":true,"extraInHost":[],"missingFromHost":[],"hostKeyResolved":[{"callee":"transfer_error_reply","file":"native/desktop_host/src/ipc_router.cpp","line":5788,"keys":[]},{"callee":"transfer_snapshot_reply","file":"native/desktop_host/src/ipc_router.cpp","line":5780,"keys":["transfer"]}]},
    {"command":"file_transfer_cancel","camel":"fileTransferCancel","line":343,"host":{"status":"handler","handler":"transfer_command","line":7614},"sidecar":{"status":"n/a","method":null,"line":null,"reason":null},"ui":[{"file":"apps/desktop/src/features/files/RemoteFilesPane.tsx","line":228,"text":"<ul className=\"transfer-list\">{(transfers.data ?? []).map((transfer) => <TransferCard key={transfer.transferId"}],"uiLiteral":[],"fixture":null,"verdict":"端到端可达","responseKeys":["transfer"],"responseKind":"inline-strict","responseVia":null,"hostKeys":["transfer"],"hostKeyOpaque":[],"hostKeyScope":{"from":7832,"to":7859,"via":"explicit-branch","handler":"transfer_command"},"hostKeyExcluded":[{"from":7837,"to":7840,"names":[],"notNames":[],"prefix":null},{"from":7847,"to":7852,"names":[],"notNames":[],"prefix":null},{"from":7848,"to":7850,"names":[],"notNames":[],"prefix":null}],"hostKeyShapeGuards":[],"hostKeySource":"native/desktop_host/src/ipc_router.cpp:7832-7836,7841-7846,7853-7859","hostKeyNarrowed":true,"extraInHost":[],"missingFromHost":[],"hostKeyResolved":[{"callee":"transfer_error_reply","file":"native/desktop_host/src/ipc_router.cpp","line":5788,"keys":[]},{"callee":"transfer_snapshot_reply","file":"native/desktop_host/src/ipc_router.cpp","line":5780,"keys":["transfer"]}]},
    {"command":"file_transfer_resolve_conflict","camel":"fileTransferResolveConflict","line":347,"host":{"status":"handler","handler":"transfer_command","line":7614},"sidecar":{"status":"n/a","method":null,"line":null,"reason":null},"ui":[{"file":"apps/desktop/src/features/files/RemoteFilesPane.tsx","line":161,"text":"await callDesktop(IPC_COMMANDS.fileTransferResolveConflict, { transferId: transfer.transferId, action });"}],"uiLiteral":[],"fixture":null,"verdict":"端到端可达","responseKeys":["transfer"],"responseKind":"inline-strict","responseVia":null,"hostKeys":["transfer"],"hostKeyOpaque":[],"hostKeyScope":{"from":7861,"to":7912,"via":"explicit-branch","handler":"transfer_command"},"hostKeyExcluded":[{"from":7866,"to":7868,"names":[],"notNames":[],"prefix":null},{"from":7870,"to":7873,"names":[],"notNames":[],"prefix":null},{"from":7878,"to":7880,"names":[],"notNames":[],"prefix":null},{"from":7893,"to":7898,"names":[],"notNames":[],"prefix":null},{"from":7894,"to":7896,"names":[],"notNames":[],"prefix":null},{"from":7901,"to":7903,"names":[],"notNames":[],"prefix":null},{"from":7904,"to":7906,"names":[],"notNames":[],"prefix":null}],"hostKeyShapeGuards":[],"hostKeySource":"native/desktop_host/src/ipc_router.cpp:7861-7865,7869,7874-7877,7881-7892,7899-7900,7907-7912","hostKeyNarrowed":true,"extraInHost":[],"missingFromHost":[],"hostKeyResolved":[{"callee":"transfer_error_reply","file":"native/desktop_host/src/ipc_router.cpp","line":5788,"keys":[]},{"callee":"transfer_snapshot_reply","file":"native/desktop_host/src/ipc_router.cpp","line":5780,"keys":["transfer"]}]},
    {"command":"activity_list","camel":"activityList","line":351,"host":{"status":"inline","handler":null,"line":7173},"sidecar":{"status":"host-local","method":null,"line":null},"ui":[{"file":"apps/desktop/src/features/activity/ActivityFeed.tsx","line":63,"text":"await callDesktop(IPC_COMMANDS.activityList, scoped ? { serverId: serverId as string, limit: 50 } : { limit: 1"},{"file":"apps/desktop/src/features/overview/InfrastructureDashboard.tsx","line":23,"text":"queryFn: async () => (await callDesktop(IPC_COMMANDS.activityList, { limit: 100 })).activities,"},{"file":"apps/desktop/src/features/overview/ServerOverview.tsx","line":393,"text":"queryFn: async () => (await callDesktop(IPC_COMMANDS.activityList, { serverId, limit: 50 })).activities,"},{"file":"apps/desktop/src/features/workpages/useWorkpageOperations.ts","line":20,"text":"queryFn: async () => (await callDesktop(IPC_COMMANDS.activityList, { ...(serverId ? { serverId } : {}), limit:"}],"uiLiteral":[],"fixture":{"file":"apps/desktop/src/lib/preview-fixtures.ts","line":521},"verdict":"端到端可达","responseKeys":["activities"],"responseKind":"inline-strict","responseVia":null,"hostKeys":["activities"],"hostKeyOpaque":[],"hostKeyScope":{"from":7173,"to":7217,"via":"explicit-branch","handler":"invoke"},"hostKeyExcluded":[{"from":7175,"to":7177,"names":[],"notNames":[],"prefix":null},{"from":7180,"to":7189,"names":[],"notNames":[],"prefix":null},{"from":7181,"to":7183,"names":[],"notNames":[],"prefix":null},{"from":7185,"to":7187,"names":[],"notNames":[],"prefix":null},{"from":7191,"to":7208,"names":[],"notNames":[],"prefix":null},{"from":7193,"to":7199,"names":[],"notNames":[],"prefix":null},{"from":7195,"to":7197,"names":[],"notNames":[],"prefix":null},{"from":7201,"to":7203,"names":[],"notNames":[],"prefix":null},{"from":7209,"to":7211,"names":[],"notNames":[],"prefix":null}],"hostKeyShapeGuards":[],"hostKeySource":"native/desktop_host/src/ipc_router.cpp:7173-7174,7178-7179,7190,7212-7217","hostKeyNarrowed":true,"extraInHost":[],"missingFromHost":[],"hostKeyResolved":[{"callee":"list_activities","file":"native/desktop_host/src/database.cpp","line":1893,"keys":["activities"]}]},
    {"command":"tool_execution_list","camel":"toolExecutionList","line":358,"host":{"status":"inline","handler":null,"line":7219},"sidecar":{"status":"host-local","method":null,"line":null},"ui":[{"file":"apps/desktop/src/features/activity/ActivityFeed.tsx","line":114,"text":"callDesktop(IPC_COMMANDS.toolExecutionList, { traceId: traceId as string, limit: 50 }),"},{"file":"apps/desktop/src/features/workpages/useWorkpageOperations.ts","line":25,"text":"queryFn: async () => (await callDesktop(IPC_COMMANDS.toolExecutionList, { ...(serverId ? { serverId } : {}), l"}],"uiLiteral":[],"fixture":{"file":"apps/desktop/src/lib/preview-fixtures.ts","line":532},"verdict":"端到端可达","responseKeys":["executions"],"responseKind":"named","responseVia":"packages/shared/src/schemas/activity.ts:41","hostKeys":["executions"],"hostKeyOpaque":[],"hostKeyScope":{"from":7219,"to":7272,"via":"explicit-branch","handler":"invoke"},"hostKeyExcluded":[{"from":7221,"to":7223,"names":[],"notNames":[],"prefix":null},{"from":7226,"to":7233,"names":[],"notNames":[],"prefix":null},{"from":7227,"to":7229,"names":[],"notNames":[],"prefix":null},{"from":7235,"to":7244,"names":[],"notNames":[],"prefix":null},{"from":7236,"to":7238,"names":[],"notNames":[],"prefix":null},{"from":7240,"to":7242,"names":[],"notNames":[],"prefix":null},{"from":7246,"to":7263,"names":[],"notNames":[],"prefix":null},{"from":7248,"to":7254,"names":[],"notNames":[],"prefix":null},{"from":7250,"to":7252,"names":[],"notNames":[],"prefix":null},{"from":7256,"to":7258,"names":[],"notNames":[],"prefix":null},{"from":7264,"to":7266,"names":[],"notNames":[],"prefix":null}],"hostKeyShapeGuards":[],"hostKeySource":"native/desktop_host/src/ipc_router.cpp:7219-7220,7224-7225,7234,7245,7267-7272","hostKeyNarrowed":true,"extraInHost":[],"missingFromHost":[],"hostKeyResolved":[{"callee":"list_tool_executions","file":"native/desktop_host/src/database.cpp","line":1970,"keys":["executions"]}]},
    {"command":"terminal_open","camel":"terminalOpen","line":366,"host":{"status":"handler","handler":"terminal_command","line":6060},"sidecar":{"status":"n/a","method":null,"line":null,"reason":null},"ui":[{"file":"apps/desktop/src/features/terminal/TerminalPane.tsx","line":176,"text":"void callDesktop(IPC_COMMANDS.terminalOpen, {"}],"uiLiteral":[],"fixture":{"file":"apps/desktop/src/lib/preview-fixtures.ts","line":541},"verdict":"端到端可达","responseKeys":["terminalSessionId"],"responseKind":"named","responseVia":"packages/shared/src/schemas/ipc.ts:125","hostKeys":["terminalSessionId"],"hostKeyOpaque":[],"hostKeyScope":{"from":8855,"to":8872,"via":"explicit-branch","handler":"terminal_command"},"hostKeyExcluded":[{"from":8864,"to":8866,"names":[],"notNames":[],"prefix":null}],"hostKeyShapeGuards":[],"hostKeySource":"native/desktop_host/src/ipc_router.cpp:8855-8863,8867-8872","hostKeyNarrowed":true,"extraInHost":[],"missingFromHost":[],"hostKeyResolved":[]},
    {"command":"terminal_write","camel":"terminalWrite","line":374,"host":{"status":"handler","handler":"terminal_command","line":6060},"sidecar":{"status":"n/a","method":null,"line":null,"reason":null},"ui":[{"file":"apps/desktop/src/features/terminal/TerminalPane.tsx","line":197,"text":"void callDesktop(IPC_COMMANDS.terminalWrite, {"}],"uiLiteral":[],"fixture":{"file":"apps/desktop/src/lib/preview-fixtures.ts","line":543},"verdict":"端到端可达","responseKeys":[],"responseKind":"empty","responseVia":null,"hostKeys":[],"hostKeyOpaque":[],"hostKeyScope":{"from":8847,"to":8918,"via":"fall-through","handler":"terminal_command"},"hostKeyExcluded":[{"from":8849,"to":8851,"names":[],"notNames":[],"prefix":null},{"from":8852,"to":8854,"names":[],"notNames":[],"prefix":null},{"from":8855,"to":8872,"names":["terminal_open"],"notNames":[],"prefix":null},{"from":8864,"to":8866,"names":[],"notNames":[],"prefix":null},{"from":8880,"to":8882,"names":[],"notNames":[],"prefix":null},{"from":8884,"to":8886,"names":[],"notNames":[],"prefix":null},{"from":8888,"to":8900,"names":[],"notNames":[],"prefix":null},{"from":8889,"to":8891,"names":[],"notNames":[],"prefix":null},{"from":8893,"to":8895,"names":[],"notNames":[],"prefix":null},{"from":8896,"to":8898,"names":[],"notNames":[],"prefix":null},{"from":8901,"to":8914,"names":[],"notNames":[],"prefix":null},{"from":8902,"to":8904,"names":[],"notNames":[],"prefix":null},{"from":8907,"to":8909,"names":[],"notNames":[],"prefix":null},{"from":8910,"to":8912,"names":[],"notNames":[],"prefix":null}],"hostKeyShapeGuards":[],"hostKeySource":"native/desktop_host/src/ipc_router.cpp:8847-8848,8873-8879,8883,8887,8915-8918","hostKeyNarrowed":true,"extraInHost":[],"missingFromHost":[],"hostKeyResolved":[]},
    {"command":"terminal_resize","camel":"terminalResize","line":378,"host":{"status":"handler","handler":"terminal_command","line":6061},"sidecar":{"status":"n/a","method":null,"line":null,"reason":null},"ui":[{"file":"apps/desktop/src/features/terminal/TerminalPane.tsx","line":192,"text":"void callDesktop(IPC_COMMANDS.terminalResize, { terminalSessionId, cols: term.cols, rows: term.rows }).catch(r"},{"file":"apps/desktop/src/features/terminal/TerminalPane.tsx","line":205,"text":"void callDesktop(IPC_COMMANDS.terminalResize, {"}],"uiLiteral":[],"fixture":{"file":"apps/desktop/src/lib/preview-fixtures.ts","line":544},"verdict":"端到端可达","responseKeys":[],"responseKind":"empty","responseVia":null,"hostKeys":[],"hostKeyOpaque":[],"hostKeyScope":{"from":8847,"to":8918,"via":"fall-through","handler":"terminal_command"},"hostKeyExcluded":[{"from":8849,"to":8851,"names":[],"notNames":[],"prefix":null},{"from":8852,"to":8854,"names":[],"notNames":[],"prefix":null},{"from":8855,"to":8872,"names":["terminal_open"],"notNames":[],"prefix":null},{"from":8864,"to":8866,"names":[],"notNames":[],"prefix":null},{"from":8880,"to":8882,"names":[],"notNames":[],"prefix":null},{"from":8884,"to":8886,"names":[],"notNames":[],"prefix":null},{"from":8888,"to":8900,"names":[],"notNames":[],"prefix":null},{"from":8889,"to":8891,"names":[],"notNames":[],"prefix":null},{"from":8893,"to":8895,"names":[],"notNames":[],"prefix":null},{"from":8896,"to":8898,"names":[],"notNames":[],"prefix":null},{"from":8901,"to":8914,"names":[],"notNames":[],"prefix":null},{"from":8902,"to":8904,"names":[],"notNames":[],"prefix":null},{"from":8907,"to":8909,"names":[],"notNames":[],"prefix":null},{"from":8910,"to":8912,"names":[],"notNames":[],"prefix":null}],"hostKeyShapeGuards":[],"hostKeySource":"native/desktop_host/src/ipc_router.cpp:8847-8848,8873-8879,8883,8887,8915-8918","hostKeyNarrowed":true,"extraInHost":[],"missingFromHost":[],"hostKeyResolved":[]},
    {"command":"terminal_close","camel":"terminalClose","line":386,"host":{"status":"handler","handler":"terminal_command","line":6061},"sidecar":{"status":"n/a","method":null,"line":null,"reason":null},"ui":[{"file":"apps/desktop/src/features/terminal/TerminalPane.tsx","line":183,"text":"void callDesktop(IPC_COMMANDS.terminalClose, { terminalSessionId }).catch(() => {});"},{"file":"apps/desktop/src/features/terminal/TerminalPane.tsx","line":227,"text":"void callDesktop(IPC_COMMANDS.terminalClose, { terminalSessionId: sessionId }).catch(() => {});"}],"uiLiteral":[],"fixture":{"file":"apps/desktop/src/lib/preview-fixtures.ts","line":545},"verdict":"端到端可达","responseKeys":[],"responseKind":"empty","responseVia":null,"hostKeys":[],"hostKeyOpaque":[],"hostKeyScope":{"from":8847,"to":8918,"via":"fall-through","handler":"terminal_command"},"hostKeyExcluded":[{"from":8849,"to":8851,"names":[],"notNames":[],"prefix":null},{"from":8852,"to":8854,"names":[],"notNames":[],"prefix":null},{"from":8855,"to":8872,"names":["terminal_open"],"notNames":[],"prefix":null},{"from":8864,"to":8866,"names":[],"notNames":[],"prefix":null},{"from":8880,"to":8882,"names":[],"notNames":[],"prefix":null},{"from":8884,"to":8886,"names":[],"notNames":[],"prefix":null},{"from":8888,"to":8900,"names":[],"notNames":[],"prefix":null},{"from":8889,"to":8891,"names":[],"notNames":[],"prefix":null},{"from":8893,"to":8895,"names":[],"notNames":[],"prefix":null},{"from":8896,"to":8898,"names":[],"notNames":[],"prefix":null},{"from":8901,"to":8914,"names":[],"notNames":[],"prefix":null},{"from":8902,"to":8904,"names":[],"notNames":[],"prefix":null},{"from":8907,"to":8909,"names":[],"notNames":[],"prefix":null},{"from":8910,"to":8912,"names":[],"notNames":[],"prefix":null}],"hostKeyShapeGuards":[],"hostKeySource":"native/desktop_host/src/ipc_router.cpp:8847-8848,8873-8879,8883,8887,8915-8918","hostKeyNarrowed":true,"extraInHost":[],"missingFromHost":[],"hostKeyResolved":[]},
    {"command":"agent_spawn","camel":"agentSpawn","line":390,"host":{"status":"handler","handler":"agent_command","line":7547},"sidecar":{"status":"n/a","method":null,"line":null,"reason":null},"ui":[{"file":"apps/desktop/src/lib/runtime.ts","line":60,"text":"mutationFn: () => callDesktop(IPC_COMMANDS.agentSpawn, {}),"}],"uiLiteral":[{"file":"apps/desktop/src/lib/preview-chat.ts","line":4,"text":"\"agent_status\", \"agent_spawn\", \"agent_logs\", \"agent_run_start\", \"agent_run_stop\","}],"fixture":null,"verdict":"端到端可达","responseKeys":["pid","protocolVersion","agentVersion","entry","toolCount","alreadyRunning"],"responseKind":"named","responseVia":"packages/shared/src/schemas/ipc.ts:135","hostKeys":["agentVersion","alreadyRunning","entry","pid","protocolVersion","toolCount"],"hostKeyOpaque":[],"hostKeyScope":{"from":9055,"to":9071,"via":"explicit-branch","handler":"agent_command"},"hostKeyExcluded":[],"hostKeyShapeGuards":[],"hostKeySource":"native/desktop_host/src/ipc_router.cpp:9055-9071","hostKeyNarrowed":true,"extraInHost":[],"missingFromHost":[],"hostKeyResolved":[]},
    {"command":"agent_kill","camel":"agentKill","line":391,"host":{"status":"handler","handler":"agent_command","line":7547},"sidecar":{"status":"n/a","method":null,"line":null,"reason":null},"ui":[],"uiLiteral":[],"fixture":null,"verdict":"UI 无调用点（死命令）","responseKeys":["killed"],"responseKind":"inline-strict","responseVia":null,"hostKeys":["killed"],"hostKeyOpaque":[],"hostKeyScope":{"from":9047,"to":9054,"via":"explicit-branch","handler":"agent_command"},"hostKeyExcluded":[{"from":9049,"to":9052,"names":[],"notNames":[],"prefix":null}],"hostKeyShapeGuards":[],"hostKeySource":"native/desktop_host/src/ipc_router.cpp:9047-9048,9053-9054","hostKeyNarrowed":true,"extraInHost":[],"missingFromHost":[],"hostKeyResolved":[]},
    {"command":"agent_status","camel":"agentStatus","line":392,"host":{"status":"handler","handler":"agent_command","line":7547},"sidecar":{"status":"n/a","method":null,"line":null,"reason":null},"ui":[{"file":"apps/desktop/src/lib/runtime.ts","line":40,"text":"queryFn: () => callDesktop(IPC_COMMANDS.agentStatus, {}),"}],"uiLiteral":[{"file":"apps/desktop/src/lib/preview-chat.ts","line":4,"text":"\"agent_status\", \"agent_spawn\", \"agent_logs\", \"agent_run_start\", \"agent_run_stop\","}],"fixture":{"file":"apps/desktop/src/lib/preview-fixtures.ts","line":551},"verdict":"端到端可达","responseKeys":["running","pid","protocolVersion","agentVersion","toolCount","entry","startedAt","lastExit","restart"],"responseKind":"named","responseVia":"packages/shared/src/schemas/ipc.ts:144","hostKeys":["agentVersion","entry","lastExit","pid","protocolVersion","running","startedAt","toolCount"],"hostKeyOpaque":[],"hostKeyScope":{"from":9010,"to":9037,"via":"explicit-branch","handler":"agent_command"},"hostKeyExcluded":[{"from":9028,"to":9035,"names":[],"notNames":[],"prefix":null}],"hostKeyShapeGuards":[],"hostKeySource":"native/desktop_host/src/ipc_router.cpp:9010-9027,9036-9037","hostKeyNarrowed":true,"extraInHost":[],"missingFromHost":["restart"],"hostKeyResolved":[]},
    {"command":"agent_logs","camel":"agentLogs","line":393,"host":{"status":"handler","handler":"agent_command","line":7548},"sidecar":{"status":"n/a","method":null,"line":null,"reason":null},"ui":[{"file":"apps/desktop/src/lib/runtime.ts","line":53,"text":"queryFn: () => callDesktop(IPC_COMMANDS.agentLogs, {}),"}],"uiLiteral":[{"file":"apps/desktop/src/lib/preview-chat.ts","line":4,"text":"\"agent_status\", \"agent_spawn\", \"agent_logs\", \"agent_run_start\", \"agent_run_stop\","}],"fixture":{"file":"apps/desktop/src/lib/preview-fixtures.ts","line":553},"verdict":"端到端可达","responseKeys":["lines","capacity"],"responseKind":"named","responseVia":"packages/shared/src/schemas/ipc.ts:161","hostKeys":["capacity","lines"],"hostKeyOpaque":[],"hostKeyScope":{"from":9038,"to":9046,"via":"explicit-branch","handler":"agent_command"},"hostKeyExcluded":[{"from":9041,"to":9044,"names":[],"notNames":[],"prefix":null}],"hostKeyShapeGuards":[],"hostKeySource":"native/desktop_host/src/ipc_router.cpp:9038-9040,9045-9046","hostKeyNarrowed":true,"extraInHost":[],"missingFromHost":[],"hostKeyResolved":[]},
    {"command":"agent_run_start","camel":"agentRunStart","line":394,"host":{"status":"handler","handler":"agent_run_command","line":7560},"sidecar":{"status":"implemented","method":"agent.run.start","line":286,"reason":null,"details":[{"method":"agent.run.start","ok":true,"line":286}],"source":"native/desktop_host/src/ipc_router.cpp:9094-9180"},"ui":[{"file":"apps/desktop/src/features/agent/useAgentRun.ts","line":385,"text":"const request: IpcCommandMap[typeof IPC_COMMANDS.agentRunStart][\"params\"] = {"},{"file":"apps/desktop/src/features/agent/useAgentRun.ts","line":404,"text":"const admissionResponse = await callDesktop(IPC_COMMANDS.agentRunStart, {"},{"file":"apps/desktop/src/features/agent/useAgentRun.ts","line":410,"text":"const response = await callDesktop(IPC_COMMANDS.agentRunStart, {"}],"uiLiteral":[{"file":"apps/desktop/src/lib/preview-chat.ts","line":4,"text":"\"agent_status\", \"agent_spawn\", \"agent_logs\", \"agent_run_start\", \"agent_run_stop\","},{"file":"apps/desktop/src/lib/preview-chat.ts","line":22,"text":"if (command === \"agent_run_start\") {"}],"fixture":null,"verdict":"端到端可达","responseKeys":["runId","started","duplicate","resumed","result"],"responseKind":"inline-strict","responseVia":null,"hostKeys":[],"hostKeyOpaque":[{"callee":null,"rpcForward":true,"text":"client.request(found->second, params, timeout)};"}],"hostKeyScope":{"from":9119,"to":9128,"via":"explicit-branch","handler":"agent_run_command"},"hostKeyExcluded":[],"hostKeyShapeGuards":[],"hostKeySource":"native/desktop_host/src/ipc_router.cpp:9119-9128","hostKeyNarrowed":true,"extraInHost":[],"missingFromHost":["runId","started","duplicate","resumed","result"],"hostKeyResolved":[]},
    {"command":"agent_run_stop","camel":"agentRunStop","line":443,"host":{"status":"handler","handler":"agent_run_command","line":7560},"sidecar":{"status":"implemented","method":"agent.run.stop","line":288,"reason":null,"details":[{"method":"agent.run.stop","ok":true,"line":288}],"source":"native/desktop_host/src/ipc_router.cpp:9075-9180"},"ui":[{"file":"apps/desktop/src/features/agent/useAgentRun.ts","line":436,"text":"const { stopped } = await callDesktop(IPC_COMMANDS.agentRunStop, { runId });"}],"uiLiteral":[{"file":"apps/desktop/src/lib/preview-chat.ts","line":4,"text":"\"agent_status\", \"agent_spawn\", \"agent_logs\", \"agent_run_start\", \"agent_run_stop\","}],"fixture":null,"verdict":"端到端可达","responseKeys":["stopped"],"responseKind":"inline-strict","responseVia":null,"hostKeys":[],"hostKeyOpaque":[{"callee":null,"rpcForward":true,"text":"client.request(found->second, params, timeout)};"}],"hostKeyScope":{"from":9119,"to":9128,"via":"explicit-branch","handler":"agent_run_command"},"hostKeyExcluded":[],"hostKeyShapeGuards":[],"hostKeySource":"native/desktop_host/src/ipc_router.cpp:9119-9128","hostKeyNarrowed":true,"extraInHost":[],"missingFromHost":["stopped"],"hostKeyResolved":[]},
    {"command":"agent_context_reset","camel":"agentContextReset","line":447,"host":{"status":"handler","handler":"agent_run_command","line":7558},"sidecar":{"status":"implemented","method":"agent.context.reset","line":292,"reason":null,"details":[{"method":"agent.context.reset","ok":true,"line":292}],"source":"native/desktop_host/src/ipc_router.cpp:9094-9180"},"ui":[{"file":"apps/desktop/src/features/agent/AgentPanel.tsx","line":391,"text":"const result = await callDesktop(IPC_COMMANDS.agentContextReset, { sessionId: resetSession, focusServerId: res"},{"file":"apps/desktop/src/lib/preview-memory.ts","line":29,"text":"if (command === IPC_COMMANDS.agentContextReset) return { reset: true };"}],"uiLiteral":[{"file":"apps/desktop/src/lib/preview-chat.ts","line":5,"text":"\"agent_context_reset\", \"agent_context_reset_all\","},{"file":"apps/desktop/src/lib/preview-chat.ts","line":18,"text":"if ([\"chat_session_list\", \"chat_session_create\", \"agent_context_reset\"].includes(command)) {"}],"fixture":null,"verdict":"端到端可达","responseKeys":["reset"],"responseKind":"inline-strict","responseVia":null,"hostKeys":[],"hostKeyOpaque":[{"callee":null,"rpcForward":true,"text":"client.request(found->second, params, timeout)};"}],"hostKeyScope":{"from":9119,"to":9128,"via":"explicit-branch","handler":"agent_run_command"},"hostKeyExcluded":[],"hostKeyShapeGuards":[],"hostKeySource":"native/desktop_host/src/ipc_router.cpp:9119-9128","hostKeyNarrowed":true,"extraInHost":[],"missingFromHost":["reset"],"hostKeyResolved":[]},
    {"command":"agent_context_reset_all","camel":"agentContextResetAll","line":451,"host":{"status":"handler","handler":"agent_run_command","line":7558},"sidecar":{"status":"implemented","method":"agent.context.resetAll","line":294,"reason":null,"details":[{"method":"agent.context.resetAll","ok":true,"line":294}],"source":"native/desktop_host/src/ipc_router.cpp:9075-9180"},"ui":[{"file":"apps/desktop/src/features/agent/MemoryLibrary.tsx","line":38,"text":"const reset = useMutation({ mutationFn: () => callDesktop(IPC_COMMANDS.agentContextResetAll, {}), onSuccess: ("},{"file":"apps/desktop/src/lib/preview-memory.ts","line":30,"text":"if (command === IPC_COMMANDS.agentContextResetAll) return { reset: true, count: 0 };"}],"uiLiteral":[{"file":"apps/desktop/src/lib/preview-chat.ts","line":5,"text":"\"agent_context_reset\", \"agent_context_reset_all\","}],"fixture":null,"verdict":"端到端可达","responseKeys":["reset","count"],"responseKind":"inline-strict","responseVia":null,"hostKeys":[],"hostKeyOpaque":[{"callee":null,"rpcForward":true,"text":"client.request(found->second, params, timeout)};"}],"hostKeyScope":{"from":9119,"to":9128,"via":"explicit-branch","handler":"agent_run_command"},"hostKeyExcluded":[],"hostKeyShapeGuards":[],"hostKeySource":"native/desktop_host/src/ipc_router.cpp:9119-9128","hostKeyNarrowed":true,"extraInHost":[],"missingFromHost":["reset","count"],"hostKeyResolved":[]},
    {"command":"agent_memory_list","camel":"agentMemoryList","line":455,"host":{"status":"handler","handler":"agent_run_command","line":7559},"sidecar":{"status":"implemented","method":"agent.memory.list","line":298,"reason":null,"details":[{"method":"agent.memory.list","ok":true,"line":298}],"source":"native/desktop_host/src/ipc_router.cpp:9075-9180"},"ui":[{"file":"apps/desktop/src/features/agent/MemoryLibrary.tsx","line":30,"text":"queryFn: async () => (await callDesktop(IPC_COMMANDS.agentMemoryList, { ...memoryScope, query, limit: 100 }))."},{"file":"apps/desktop/src/lib/preview-memory.ts","line":12,"text":"if (command === IPC_COMMANDS.agentMemoryList) return { memories: memories.filter((memory) =>"}],"uiLiteral":[],"fixture":null,"verdict":"端到端可达","responseKeys":["memories"],"responseKind":"inline-strict","responseVia":null,"hostKeys":[],"hostKeyOpaque":[{"callee":null,"rpcForward":true,"text":"client.request(found->second, params, timeout)};"}],"hostKeyScope":{"from":9119,"to":9128,"via":"explicit-branch","handler":"agent_run_command"},"hostKeyExcluded":[],"hostKeyShapeGuards":[],"hostKeySource":"native/desktop_host/src/ipc_router.cpp:9119-9128","hostKeyNarrowed":true,"extraInHost":[],"missingFromHost":["memories"],"hostKeyResolved":[]},
    {"command":"agent_memory_save","camel":"agentMemorySave","line":456,"host":{"status":"handler","handler":"agent_run_command","line":7559},"sidecar":{"status":"implemented","method":"agent.memory.save","line":300,"reason":null,"details":[{"method":"agent.memory.save","ok":true,"line":300}],"source":"native/desktop_host/src/ipc_router.cpp:9075-9180"},"ui":[{"file":"apps/desktop/src/features/agent/MemoryLibrary.tsx","line":32,"text":"const save = useMutation({ mutationFn: () => callDesktop(IPC_COMMANDS.agentMemorySave, {"},{"file":"apps/desktop/src/lib/preview-memory.ts","line":15,"text":"if (command === IPC_COMMANDS.agentMemorySave) {"}],"uiLiteral":[],"fixture":null,"verdict":"端到端可达","responseKeys":["memory"],"responseKind":"inline-strict","responseVia":null,"hostKeys":[],"hostKeyOpaque":[{"callee":null,"rpcForward":true,"text":"client.request(found->second, params, timeout)};"}],"hostKeyScope":{"from":9119,"to":9128,"via":"explicit-branch","handler":"agent_run_command"},"hostKeyExcluded":[],"hostKeyShapeGuards":[],"hostKeySource":"native/desktop_host/src/ipc_router.cpp:9119-9128","hostKeyNarrowed":true,"extraInHost":[],"missingFromHost":["memory"],"hostKeyResolved":[]},
    {"command":"agent_memory_delete","camel":"agentMemoryDelete","line":457,"host":{"status":"handler","handler":"agent_run_command","line":7559},"sidecar":{"status":"implemented","method":"agent.memory.delete","line":302,"reason":null,"details":[{"method":"agent.memory.delete","ok":true,"line":302}],"source":"native/desktop_host/src/ipc_router.cpp:9075-9180"},"ui":[{"file":"apps/desktop/src/features/agent/MemoryLibrary.tsx","line":35,"text":"const remove = useMutation({ mutationFn: (memory: AgentMemory) => callDesktop(IPC_COMMANDS.agentMemoryDelete, "},{"file":"apps/desktop/src/lib/preview-memory.ts","line":24,"text":"if (command === IPC_COMMANDS.agentMemoryDelete) {"}],"uiLiteral":[],"fixture":null,"verdict":"端到端可达","responseKeys":["deleted"],"responseKind":"inline-strict","responseVia":null,"hostKeys":[],"hostKeyOpaque":[{"callee":null,"rpcForward":true,"text":"client.request(found->second, params, timeout)};"}],"hostKeyScope":{"from":9119,"to":9128,"via":"explicit-branch","handler":"agent_run_command"},"hostKeyExcluded":[],"hostKeyShapeGuards":[],"hostKeySource":"native/desktop_host/src/ipc_router.cpp:9119-9128","hostKeyNarrowed":true,"extraInHost":[],"missingFromHost":["deleted"],"hostKeyResolved":[]},
    {"command":"agent_approval_respond","camel":"agentApprovalRespond","line":458,"host":{"status":"handler","handler":"agent_run_command","line":7561},"sidecar":{"status":"implemented","method":"agent.approval.respond","line":290,"reason":null,"details":[{"method":"agent.approval.respond","ok":true,"line":290}],"source":"native/desktop_host/src/ipc_router.cpp:9075-9180"},"ui":[{"file":"apps/desktop/src/features/agent/useAgentRun.ts","line":460,"text":"const { accepted } = await callDesktop(IPC_COMMANDS.agentApprovalRespond, {"}],"uiLiteral":[],"fixture":null,"verdict":"端到端可达","responseKeys":["accepted"],"responseKind":"inline-strict","responseVia":null,"hostKeys":["accepted"],"hostKeyOpaque":[],"hostKeyScope":{"from":9075,"to":9180,"via":"fall-through","handler":"agent_run_command"},"hostKeyExcluded":[{"from":9088,"to":9090,"names":[],"notNames":[],"prefix":null},{"from":9091,"to":9093,"names":[],"notNames":[],"prefix":null},{"from":9094,"to":9111,"names":["agent_run_start","agent_context_reset"],"notNames":[],"prefix":null},{"from":9100,"to":9106,"names":[],"notNames":[],"prefix":null},{"from":9102,"to":9104,"names":[],"notNames":[],"prefix":null},{"from":9119,"to":9128,"names":[],"notNames":["agent_approval_respond"],"prefix":null},{"from":9137,"to":9144,"names":[],"notNames":[],"prefix":null},{"from":9165,"to":9169,"names":[],"notNames":[],"prefix":null}],"hostKeyShapeGuards":[],"hostKeySource":"native/desktop_host/src/ipc_router.cpp:9075-9087,9112-9118,9129-9136,9145-9164,9170-9180","hostKeyNarrowed":true,"extraInHost":[],"missingFromHost":[],"hostKeyResolved":[]},
    {"command":"chat_session_list","camel":"chatSessionList","line":462,"host":{"status":"inline","handler":null,"line":7274},"sidecar":{"status":"host-local","method":null,"line":null},"ui":[{"file":"apps/desktop/src/features/agent/AgentHistoryPane.tsx","line":137,"text":"callDesktop(IPC_COMMANDS.chatSessionList, {"},{"file":"apps/desktop/src/features/agent/useRecentAgentSessions.ts","line":11,"text":"queryFn: () => callDesktop(IPC_COMMANDS.chatSessionList, { serverId, archived: false, limit }),"}],"uiLiteral":[{"file":"apps/desktop/src/lib/preview-chat.ts","line":7,"text":"\"chat_session_list\", \"chat_session_get\", \"chat_session_create\", \"chat_message_append\","},{"file":"apps/desktop/src/lib/preview-chat.ts","line":18,"text":"if ([\"chat_session_list\", \"chat_session_create\", \"agent_context_reset\"].includes(command)) {"}],"fixture":{"file":"apps/desktop/src/lib/preview-fixtures.ts","line":555},"verdict":"端到端可达","responseKeys":["sessions","counts"],"responseKind":"named","responseVia":"packages/shared/src/schemas/chat.ts:45","hostKeys":["counts","sessions"],"hostKeyOpaque":[],"hostKeyScope":{"from":7274,"to":7317,"via":"explicit-branch","handler":"invoke"},"hostKeyExcluded":[{"from":7279,"to":7285,"names":[],"notNames":[],"prefix":null},{"from":7281,"to":7283,"names":[],"notNames":[],"prefix":null},{"from":7287,"to":7292,"names":[],"notNames":[],"prefix":null},{"from":7288,"to":7290,"names":[],"notNames":[],"prefix":null},{"from":7294,"to":7296,"names":[],"notNames":[],"prefix":null},{"from":7298,"to":7300,"names":[],"notNames":[],"prefix":null},{"from":7301,"to":7303,"names":[],"notNames":[],"prefix":null},{"from":7306,"to":7312,"names":[],"notNames":[],"prefix":null},{"from":7308,"to":7310,"names":[],"notNames":[],"prefix":null}],"hostKeyShapeGuards":[],"hostKeySource":"native/desktop_host/src/ipc_router.cpp:7274-7278,7286,7293,7297,7304-7305,7313-7317","hostKeyNarrowed":true,"extraInHost":[],"missingFromHost":[],"hostKeyResolved":[{"callee":"list_chat_sessions","file":"native/desktop_host/src/database.cpp","line":4630,"keys":["counts","sessions"]}]},
    {"command":"chat_session_get","camel":"chatSessionGet","line":474,"host":{"status":"inline","handler":null,"line":7319},"sidecar":{"status":"host-local","method":null,"line":null},"ui":[{"file":"apps/desktop/src/features/agent/AgentHistoryPane.tsx","line":282,"text":"const detail = await callDesktop(IPC_COMMANDS.chatSessionGet, { sessionId: session.id });"},{"file":"apps/desktop/src/features/agent/AgentPanel.tsx","line":375,"text":"const detail = await callDesktop(IPC_COMMANDS.chatSessionGet, { sessionId });"}],"uiLiteral":[{"file":"apps/desktop/src/lib/preview-chat.ts","line":7,"text":"\"chat_session_list\", \"chat_session_get\", \"chat_session_create\", \"chat_message_append\","}],"fixture":{"file":"apps/desktop/src/lib/preview-fixtures.ts","line":564},"verdict":"端到端可达","responseKeys":["session","messages"],"responseKind":"named","responseVia":"packages/shared/src/schemas/chat.ts:50","hostKeys":["messages","session"],"hostKeyOpaque":[],"hostKeyScope":{"from":7319,"to":7336,"via":"explicit-branch","handler":"invoke"},"hostKeyExcluded":[{"from":7324,"to":7327,"names":[],"notNames":[],"prefix":null},{"from":7328,"to":7330,"names":[],"notNames":[],"prefix":null}],"hostKeyShapeGuards":[],"hostKeySource":"native/desktop_host/src/ipc_router.cpp:7319-7323,7331-7336","hostKeyNarrowed":true,"extraInHost":[],"missingFromHost":[],"hostKeyResolved":[{"callee":"get_chat_session","file":"native/desktop_host/src/database.cpp","line":4707,"keys":["messages","session"]}]},
    {"command":"chat_session_create","camel":"chatSessionCreate","line":478,"host":{"status":"inline","handler":null,"line":7338},"sidecar":{"status":"host-local","method":null,"line":null},"ui":[{"file":"apps/desktop/src/features/agent/useChatSessions.ts","line":127,"text":"const created = await callDesktop(IPC_COMMANDS.chatSessionCreate, {"}],"uiLiteral":[{"file":"apps/desktop/src/lib/preview-chat.ts","line":7,"text":"\"chat_session_list\", \"chat_session_get\", \"chat_session_create\", \"chat_message_append\","},{"file":"apps/desktop/src/lib/preview-chat.ts","line":18,"text":"if ([\"chat_session_list\", \"chat_session_create\", \"agent_context_reset\"].includes(command)) {"}],"fixture":null,"verdict":"端到端可达","responseKeys":["session"],"responseKind":"named","responseVia":"packages/shared/src/schemas/chat.ts:36","hostKeys":["session"],"hostKeyOpaque":[],"hostKeyScope":{"from":7338,"to":7383,"via":"explicit-branch","handler":"invoke"},"hostKeyExcluded":[{"from":7343,"to":7345,"names":[],"notNames":[],"prefix":null},{"from":7347,"to":7353,"names":[],"notNames":[],"prefix":null},{"from":7349,"to":7351,"names":[],"notNames":[],"prefix":null},{"from":7355,"to":7361,"names":[],"notNames":[],"prefix":null},{"from":7357,"to":7359,"names":[],"notNames":[],"prefix":null},{"from":7363,"to":7372,"names":[],"notNames":[],"prefix":null},{"from":7364,"to":7366,"names":[],"notNames":[],"prefix":null},{"from":7368,"to":7370,"names":[],"notNames":[],"prefix":null},{"from":7373,"to":7375,"names":[],"notNames":[],"prefix":null}],"hostKeyShapeGuards":[],"hostKeySource":"native/desktop_host/src/ipc_router.cpp:7338-7342,7346,7354,7362,7376-7383","hostKeyNarrowed":true,"extraInHost":[],"missingFromHost":[],"hostKeyResolved":[{"callee":"create_chat_session","file":"native/desktop_host/src/database.cpp","line":4713,"keys":["session"]}]},
    {"command":"chat_message_append","camel":"chatMessageAppend","line":487,"host":{"status":"inline","handler":null,"line":7385},"sidecar":{"status":"host-local","method":null,"line":null},"ui":[{"file":"apps/desktop/src/features/agent/useChatSessions.ts","line":92,"text":"await callDesktop(IPC_COMMANDS.chatMessageAppend, {"}],"uiLiteral":[{"file":"apps/desktop/src/lib/preview-chat.ts","line":7,"text":"\"chat_session_list\", \"chat_session_get\", \"chat_session_create\", \"chat_message_append\","}],"fixture":null,"verdict":"端到端可达","responseKeys":["message"],"responseKind":"named","responseVia":"packages/shared/src/schemas/chat.ts:55","hostKeys":["message"],"hostKeyOpaque":[],"hostKeyScope":{"from":7385,"to":7476,"via":"explicit-branch","handler":"invoke"},"hostKeyExcluded":[{"from":7392,"to":7395,"names":[],"notNames":[],"prefix":null},{"from":7401,"to":7403,"names":[],"notNames":[],"prefix":null},{"from":7405,"to":7407,"names":[],"notNames":[],"prefix":null},{"from":7409,"to":7414,"names":[],"notNames":[],"prefix":null},{"from":7410,"to":7412,"names":[],"notNames":[],"prefix":null},{"from":7415,"to":7417,"names":[],"notNames":[],"prefix":null},{"from":7419,"to":7425,"names":[],"notNames":[],"prefix":null},{"from":7421,"to":7423,"names":[],"notNames":[],"prefix":null},{"from":7427,"to":7433,"names":[],"notNames":[],"prefix":null},{"from":7429,"to":7431,"names":[],"notNames":[],"prefix":null},{"from":7436,"to":7453,"names":[],"notNames":[],"prefix":null},{"from":7446,"to":7451,"names":[],"notNames":[],"prefix":null},{"from":7448,"to":7450,"names":[],"notNames":[],"prefix":null},{"from":7455,"to":7461,"names":[],"notNames":[],"prefix":null},{"from":7457,"to":7459,"names":[],"notNames":[],"prefix":null},{"from":7462,"to":7465,"names":[],"notNames":[],"prefix":null},{"from":7466,"to":7468,"names":[],"notNames":[],"prefix":null}],"hostKeyShapeGuards":[],"hostKeySource":"native/desktop_host/src/ipc_router.cpp:7385-7391,7396-7400,7404,7408,7418,7426,7434-7435,7454,7469-7476","hostKeyNarrowed":true,"extraInHost":[],"missingFromHost":[],"hostKeyResolved":[{"callee":"append_chat_message","file":"native/desktop_host/src/database.cpp","line":4736,"keys":["message"]}]},
    {"command":"chat_session_archive","camel":"chatSessionArchive","line":506,"host":{"status":"inline","handler":null,"line":7478},"sidecar":{"status":"host-local","method":null,"line":null},"ui":[{"file":"apps/desktop/src/features/agent/AgentHistoryPane.tsx","line":160,"text":"callDesktop(IPC_COMMANDS.chatSessionArchive, { sessionId, archived }),"}],"uiLiteral":[{"file":"apps/desktop/src/lib/preview-chat.ts","line":8,"text":"\"chat_session_archive\", \"chat_session_rename\", \"chat_session_delete\","}],"fixture":null,"verdict":"端到端可达","responseKeys":["session"],"responseKind":"named","responseVia":"packages/shared/src/schemas/chat.ts:36","hostKeys":["session"],"hostKeyOpaque":[],"hostKeyScope":{"from":7478,"to":7499,"via":"explicit-branch","handler":"invoke"},"hostKeyExcluded":[{"from":7484,"to":7487,"names":[],"notNames":[],"prefix":null},{"from":7489,"to":7491,"names":[],"notNames":[],"prefix":null}],"hostKeyShapeGuards":[],"hostKeySource":"native/desktop_host/src/ipc_router.cpp:7478-7483,7488,7492-7499","hostKeyNarrowed":true,"extraInHost":[],"missingFromHost":[],"hostKeyResolved":[{"callee":"set_chat_session_archived","file":"native/desktop_host/src/database.cpp","line":4793,"keys":["session"]}]},
    {"command":"chat_session_rename","camel":"chatSessionRename","line":510,"host":{"status":"inline","handler":null,"line":7501},"sidecar":{"status":"host-local","method":null,"line":null},"ui":[{"file":"apps/desktop/src/features/agent/AgentHistoryPane.tsx","line":170,"text":"callDesktop(IPC_COMMANDS.chatSessionRename, { sessionId, title }),"}],"uiLiteral":[{"file":"apps/desktop/src/lib/preview-chat.ts","line":8,"text":"\"chat_session_archive\", \"chat_session_rename\", \"chat_session_delete\","}],"fixture":null,"verdict":"端到端可达","responseKeys":["session"],"responseKind":"named","responseVia":"packages/shared/src/schemas/chat.ts:36","hostKeys":["session"],"hostKeyOpaque":[],"hostKeyScope":{"from":7501,"to":7519,"via":"explicit-branch","handler":"invoke"},"hostKeyExcluded":[{"from":7507,"to":7510,"names":[],"notNames":[],"prefix":null},{"from":7511,"to":7513,"names":[],"notNames":[],"prefix":null}],"hostKeyShapeGuards":[],"hostKeySource":"native/desktop_host/src/ipc_router.cpp:7501-7506,7514-7519","hostKeyNarrowed":true,"extraInHost":[],"missingFromHost":[],"hostKeyResolved":[{"callee":"rename_chat_session","file":"native/desktop_host/src/database.cpp","line":4808,"keys":["session"]}]},
    {"command":"chat_session_delete","camel":"chatSessionDelete","line":517,"host":{"status":"inline","handler":null,"line":7521},"sidecar":{"status":"host-local","method":null,"line":null},"ui":[{"file":"apps/desktop/src/features/agent/AgentHistoryPane.tsx","line":183,"text":"mutationFn: (sessionId: string) => callDesktop(IPC_COMMANDS.chatSessionDelete, { sessionId }),"}],"uiLiteral":[{"file":"apps/desktop/src/lib/preview-chat.ts","line":8,"text":"\"chat_session_archive\", \"chat_session_rename\", \"chat_session_delete\","}],"fixture":null,"verdict":"端到端可达","responseKeys":["deleted"],"responseKind":"inline-strict","responseVia":null,"hostKeys":["deleted"],"hostKeyOpaque":[],"hostKeyScope":{"from":7521,"to":7545,"via":"explicit-branch","handler":"invoke"},"hostKeyExcluded":[{"from":7526,"to":7529,"names":[],"notNames":[],"prefix":null},{"from":7530,"to":7532,"names":[],"notNames":[],"prefix":null},{"from":7536,"to":7540,"names":[],"notNames":[],"prefix":null}],"hostKeyShapeGuards":[],"hostKeySource":"native/desktop_host/src/ipc_router.cpp:7521-7525,7533-7535,7541-7545","hostKeyNarrowed":true,"extraInHost":[],"missingFromHost":[],"hostKeyResolved":[{"callee":"delete_chat_session","file":"native/desktop_host/src/database.cpp","line":4822,"keys":["deleted"]}]},
    {"command":"provider_list","camel":"providerList","line":521,"host":{"status":"handler","handler":"provider_command","line":7552},"sidecar":{"status":"implemented","method":"provider.list","line":278,"reason":null,"details":[{"method":"provider.list","ok":true,"line":278}],"source":"native/desktop_host/src/ipc_router.cpp:9643-9665"},"ui":[{"file":"apps/desktop/src/lib/providers.ts","line":20,"text":"queryFn: async () => (await callDesktop(IPC_COMMANDS.providerList, {})).providers,"}],"uiLiteral":[{"file":"apps/desktop/src/lib/preview-chat.ts","line":6,"text":"\"provider_list\", \"provider_save\", \"provider_activate\", \"provider_delete\", \"provider_models\", \"provider_test\","}],"fixture":{"file":"apps/desktop/src/lib/preview-fixtures.ts","line":547},"verdict":"端到端可达","responseKeys":["providers"],"responseKind":"inline-strict","responseVia":null,"hostKeys":[],"hostKeyOpaque":[{"callee":null,"rpcForward":true,"text":"client.request(found->second, params, std::chrono::milliseconds(60000)"}],"hostKeyScope":{"from":9643,"to":9665,"via":"fall-through","handler":"provider_command"},"hostKeyExcluded":[{"from":9653,"to":9655,"names":[],"notNames":[],"prefix":null}],"hostKeyShapeGuards":[],"hostKeySource":"native/desktop_host/src/ipc_router.cpp:9643-9652,9656-9665","hostKeyNarrowed":true,"extraInHost":[],"missingFromHost":["providers"],"hostKeyResolved":[]},
    {"command":"provider_save","camel":"providerSave","line":522,"host":{"status":"handler","handler":"provider_command","line":7552},"sidecar":{"status":"implemented","method":"provider.save","line":280,"reason":null,"details":[{"method":"provider.save","ok":true,"line":280}],"source":"native/desktop_host/src/ipc_router.cpp:9643-9665"},"ui":[{"file":"apps/desktop/src/features/settings/RuntimeSettings.tsx","line":296,"text":"return callDesktop(IPC_COMMANDS.providerSave, providerSavePayload({"}],"uiLiteral":[{"file":"apps/desktop/src/lib/preview-chat.ts","line":6,"text":"\"provider_list\", \"provider_save\", \"provider_activate\", \"provider_delete\", \"provider_models\", \"provider_test\","}],"fixture":null,"verdict":"端到端可达","responseKeys":["provider"],"responseKind":"inline-strict","responseVia":null,"hostKeys":[],"hostKeyOpaque":[{"callee":null,"rpcForward":true,"text":"client.request(found->second, params, std::chrono::milliseconds(60000)"}],"hostKeyScope":{"from":9643,"to":9665,"via":"fall-through","handler":"provider_command"},"hostKeyExcluded":[{"from":9653,"to":9655,"names":[],"notNames":[],"prefix":null}],"hostKeyShapeGuards":[],"hostKeySource":"native/desktop_host/src/ipc_router.cpp:9643-9652,9656-9665","hostKeyNarrowed":true,"extraInHost":[],"missingFromHost":["provider"],"hostKeyResolved":[]},
    {"command":"provider_activate","camel":"providerActivate","line":526,"host":{"status":"handler","handler":"provider_command","line":7553},"sidecar":{"status":"implemented","method":"provider.activate","line":284,"reason":null,"details":[{"method":"provider.activate","ok":true,"line":284}],"source":"native/desktop_host/src/ipc_router.cpp:9643-9665"},"ui":[{"file":"apps/desktop/src/features/onboarding/GettingStarted.tsx","line":46,"text":"await callDesktop(IPC_COMMANDS.providerActivate, { providerId: provider.id });"},{"file":"apps/desktop/src/features/settings/RuntimeSettings.tsx","line":173,"text":"mutationFn: (providerId: string) => callDesktop(IPC_COMMANDS.providerActivate, { providerId }), onSuccess: onS"}],"uiLiteral":[{"file":"apps/desktop/src/lib/preview-chat.ts","line":6,"text":"\"provider_list\", \"provider_save\", \"provider_activate\", \"provider_delete\", \"provider_models\", \"provider_test\","}],"fixture":null,"verdict":"端到端可达","responseKeys":["provider"],"responseKind":"inline-strict","responseVia":null,"hostKeys":[],"hostKeyOpaque":[{"callee":null,"rpcForward":true,"text":"client.request(found->second, params, std::chrono::milliseconds(60000)"}],"hostKeyScope":{"from":9643,"to":9665,"via":"fall-through","handler":"provider_command"},"hostKeyExcluded":[{"from":9653,"to":9655,"names":[],"notNames":[],"prefix":null}],"hostKeyShapeGuards":[],"hostKeySource":"native/desktop_host/src/ipc_router.cpp:9643-9652,9656-9665","hostKeyNarrowed":true,"extraInHost":[],"missingFromHost":["provider"],"hostKeyResolved":[]},
    {"command":"provider_delete","camel":"providerDelete","line":530,"host":{"status":"handler","handler":"provider_command","line":7553},"sidecar":{"status":"implemented","method":"provider.delete","line":282,"reason":null,"details":[{"method":"provider.delete","ok":true,"line":282}],"source":"native/desktop_host/src/ipc_router.cpp:9643-9665"},"ui":[{"file":"apps/desktop/src/features/settings/RuntimeSettings.tsx","line":189,"text":"mutationFn: (provider: AiProviderConfig) => callDesktop(IPC_COMMANDS.providerDelete, { providerId: provider.id"}],"uiLiteral":[{"file":"apps/desktop/src/lib/preview-chat.ts","line":6,"text":"\"provider_list\", \"provider_save\", \"provider_activate\", \"provider_delete\", \"provider_models\", \"provider_test\","}],"fixture":null,"verdict":"端到端可达","responseKeys":["deleted","credentialReclaimed"],"responseKind":"named","responseVia":"packages/shared/src/schemas/provider.ts:211","hostKeys":[],"hostKeyOpaque":[{"callee":null,"rpcForward":true,"text":"client.request(found->second, params, std::chrono::milliseconds(60000)"}],"hostKeyScope":{"from":9643,"to":9665,"via":"fall-through","handler":"provider_command"},"hostKeyExcluded":[{"from":9653,"to":9655,"names":[],"notNames":[],"prefix":null}],"hostKeyShapeGuards":[],"hostKeySource":"native/desktop_host/src/ipc_router.cpp:9643-9652,9656-9665","hostKeyNarrowed":true,"extraInHost":[],"missingFromHost":["deleted","credentialReclaimed"],"hostKeyResolved":[]},
    {"command":"provider_models","camel":"providerModels","line":534,"host":{"status":"handler","handler":"provider_command","line":7554},"sidecar":{"status":"implemented","method":"provider.models","line":274,"reason":null,"details":[{"method":"provider.models","ok":true,"line":274}],"source":"native/desktop_host/src/ipc_router.cpp:9643-9665"},"ui":[{"file":"apps/desktop/src/features/settings/RuntimeSettings.tsx","line":273,"text":"queryFn: async () => (await callDesktop(IPC_COMMANDS.providerModels, { providerId: provider!.id })).models, re"}],"uiLiteral":[{"file":"apps/desktop/src/lib/preview-chat.ts","line":6,"text":"\"provider_list\", \"provider_save\", \"provider_activate\", \"provider_delete\", \"provider_models\", \"provider_test\","}],"fixture":{"file":"apps/desktop/src/lib/preview-fixtures.ts","line":549},"verdict":"端到端可达","responseKeys":["models"],"responseKind":"inline-strict","responseVia":null,"hostKeys":[],"hostKeyOpaque":[{"callee":null,"rpcForward":true,"text":"client.request(found->second, params, std::chrono::milliseconds(60000)"}],"hostKeyScope":{"from":9643,"to":9665,"via":"fall-through","handler":"provider_command"},"hostKeyExcluded":[{"from":9653,"to":9655,"names":[],"notNames":[],"prefix":null}],"hostKeyShapeGuards":[],"hostKeySource":"native/desktop_host/src/ipc_router.cpp:9643-9652,9656-9665","hostKeyNarrowed":true,"extraInHost":[],"missingFromHost":["models"],"hostKeyResolved":[]},
    {"command":"mcp_server_list","camel":"mcpServerList","line":538,"host":{"status":"handler","handler":"mcp_command","line":7565},"sidecar":{"status":"n/a","method":null,"line":null,"reason":null},"ui":[{"file":"apps/desktop/src/lib/mcp.ts","line":47,"text":"queryFn: async () => callDesktop(IPC_COMMANDS.mcpServerList, {}),"}],"uiLiteral":[],"fixture":{"file":"apps/desktop/src/lib/preview-fixtures.ts","line":576},"verdict":"端到端可达","responseKeys":["servers"],"responseKind":"named","responseVia":"packages/shared/src/schemas/mcp.ts:159","hostKeys":["servers"],"hostKeyOpaque":[],"hostKeyScope":{"from":9206,"to":9218,"via":"explicit-branch","handler":"mcp_command"},"hostKeyExcluded":[{"from":9207,"to":9209,"names":[],"notNames":[],"prefix":null}],"hostKeyShapeGuards":[],"hostKeySource":"native/desktop_host/src/ipc_router.cpp:9206,9210-9218","hostKeyNarrowed":true,"extraInHost":[],"missingFromHost":[],"hostKeyResolved":[]},
    {"command":"mcp_server_save","camel":"mcpServerSave","line":539,"host":{"status":"handler","handler":"mcp_command","line":7565},"sidecar":{"status":"n/a","method":null,"line":null,"reason":null},"ui":[{"file":"apps/desktop/src/lib/mcp.ts","line":73,"text":"mutationFn: (input) => callDesktop(IPC_COMMANDS.mcpServerSave, { input }),"}],"uiLiteral":[],"fixture":null,"verdict":"端到端可达","responseKeys":["config","status","tools","unavailable"],"responseKind":"named","responseVia":"packages/shared/src/schemas/mcp.ts:152","hostKeys":["config","status","tools","unavailable"],"hostKeyOpaque":[],"hostKeyScope":{"from":9220,"to":9444,"via":"explicit-branch","handler":"mcp_command"},"hostKeyExcluded":[{"from":9236,"to":9238,"names":[],"notNames":[],"prefix":null},{"from":9239,"to":9241,"names":[],"notNames":[],"prefix":null},{"from":9242,"to":9244,"names":[],"notNames":[],"prefix":null},{"from":9265,"to":9271,"names":[],"notNames":[],"prefix":null},{"from":9277,"to":9297,"names":[],"notNames":[],"prefix":null},{"from":9278,"to":9280,"names":[],"notNames":[],"prefix":null},{"from":9282,"to":9284,"names":[],"notNames":[],"prefix":null},{"from":9286,"to":9296,"names":[],"notNames":[],"prefix":null},{"from":9287,"to":9289,"names":[],"notNames":[],"prefix":null},{"from":9291,"to":9293,"names":[],"notNames":[],"prefix":null},{"from":9298,"to":9300,"names":[],"notNames":[],"prefix":null},{"from":9312,"to":9314,"names":[],"notNames":[],"prefix":null},{"from":9322,"to":9370,"names":[],"notNames":[],"prefix":null},{"from":9323,"to":9325,"names":[],"notNames":[],"prefix":null},{"from":9328,"to":9330,"names":[],"notNames":[],"prefix":null},{"from":9332,"to":9334,"names":[],"notNames":[],"prefix":null},{"from":9339,"to":9341,"names":[],"notNames":[],"prefix":null},{"from":9343,"to":9359,"names":[],"notNames":[],"prefix":null},{"from":9346,"to":9348,"names":[],"notNames":[],"prefix":null},{"from":9378,"to":9380,"names":[],"notNames":[],"prefix":null},{"from":9386,"to":9392,"names":[],"notNames":[],"prefix":null},{"from":9405,"to":9438,"names":[],"notNames":[],"prefix":null},{"from":9408,"to":9411,"names":[],"notNames":[],"prefix":null},{"from":9413,"to":9419,"names":[],"notNames":[],"prefix":null},{"from":9415,"to":9417,"names":[],"notNames":[],"prefix":null},{"from":9422,"to":9426,"names":[],"notNames":[],"prefix":null},{"from":9428,"to":9435,"names":[],"notNames":[],"prefix":null},{"from":9430,"to":9433,"names":[],"notNames":[],"prefix":null}],"hostKeyShapeGuards":[],"hostKeySource":"native/desktop_host/src/ipc_router.cpp:9220-9235,9245-9264,9272-9276,9301-9311,9315-9321,9371-9377,9381-9385,9393-9404,9439-9444","hostKeyNarrowed":true,"extraInHost":[],"missingFromHost":[],"hostKeyResolved":[{"callee":"view_for","file":"native/desktop_host/src/ipc_router.cpp","line":9186,"keys":["config","status","tools","unavailable"]}]},
    {"command":"mcp_server_delete","camel":"mcpServerDelete","line":543,"host":{"status":"handler","handler":"mcp_command","line":7566},"sidecar":{"status":"n/a","method":null,"line":null,"reason":null},"ui":[{"file":"apps/desktop/src/lib/mcp.ts","line":77,"text":"mutationFn: (serverId) => callDesktop(IPC_COMMANDS.mcpServerDelete, { serverId }),"}],"uiLiteral":[],"fixture":null,"verdict":"端到端可达","responseKeys":["deleted","stopped"],"responseKind":"named","responseVia":"packages/shared/src/schemas/mcp.ts:202","hostKeys":["deleted","stopped"],"hostKeyOpaque":[],"hostKeyScope":{"from":9181,"to":9642,"via":"fall-through","handler":"mcp_command"},"hostKeyExcluded":[{"from":9182,"to":9184,"names":[],"notNames":[],"prefix":null},{"from":9197,"to":9200,"names":[],"notNames":[],"prefix":null},{"from":9206,"to":9218,"names":["mcp_server_list"],"notNames":[],"prefix":null},{"from":9207,"to":9209,"names":[],"notNames":[],"prefix":null},{"from":9220,"to":9444,"names":["mcp_server_save"],"notNames":[],"prefix":null},{"from":9236,"to":9238,"names":[],"notNames":[],"prefix":null},{"from":9239,"to":9241,"names":[],"notNames":[],"prefix":null},{"from":9242,"to":9244,"names":[],"notNames":[],"prefix":null},{"from":9265,"to":9271,"names":[],"notNames":[],"prefix":null},{"from":9277,"to":9297,"names":[],"notNames":[],"prefix":null},{"from":9278,"to":9280,"names":[],"notNames":[],"prefix":null},{"from":9282,"to":9284,"names":[],"notNames":[],"prefix":null},{"from":9286,"to":9296,"names":[],"notNames":[],"prefix":null},{"from":9287,"to":9289,"names":[],"notNames":[],"prefix":null},{"from":9291,"to":9293,"names":[],"notNames":[],"prefix":null},{"from":9298,"to":9300,"names":[],"notNames":[],"prefix":null},{"from":9312,"to":9314,"names":[],"notNames":[],"prefix":null},{"from":9322,"to":9370,"names":[],"notNames":[],"prefix":null},{"from":9323,"to":9325,"names":[],"notNames":[],"prefix":null},{"from":9328,"to":9330,"names":[],"notNames":[],"prefix":null},{"from":9332,"to":9334,"names":[],"notNames":[],"prefix":null},{"from":9339,"to":9341,"names":[],"notNames":[],"prefix":null},{"from":9343,"to":9359,"names":[],"notNames":[],"prefix":null},{"from":9346,"to":9348,"names":[],"notNames":[],"prefix":null},{"from":9378,"to":9380,"names":[],"notNames":[],"prefix":null},{"from":9386,"to":9392,"names":[],"notNames":[],"prefix":null},{"from":9405,"to":9438,"names":[],"notNames":[],"prefix":null},{"from":9408,"to":9411,"names":[],"notNames":[],"prefix":null},{"from":9413,"to":9419,"names":[],"notNames":[],"prefix":null},{"from":9415,"to":9417,"names":[],"notNames":[],"prefix":null},{"from":9422,"to":9426,"names":[],"notNames":[],"prefix":null},{"from":9428,"to":9435,"names":[],"notNames":[],"prefix":null},{"from":9430,"to":9433,"names":[],"notNames":[],"prefix":null},{"from":9446,"to":9472,"names":["mcp_server_start","mcp_server_stop"],"notNames":[],"prefix":null},{"from":9451,"to":9453,"names":[],"notNames":[],"prefix":null},{"from":9454,"to":9456,"names":[],"notNames":[],"prefix":null},{"from":9458,"to":9467,"names":["mcp_server_start"],"notNames":[],"prefix":null},{"from":9474,"to":9534,"names":["mcp_server_review"],"notNames":[],"prefix":null},{"from":9483,"to":9485,"names":[],"notNames":[],"prefix":null},{"from":9491,"to":9493,"names":[],"notNames":[],"prefix":null},{"from":9497,"to":9499,"names":[],"notNames":[],"prefix":null},{"from":9501,"to":9503,"names":[],"notNames":[],"prefix":null},{"from":9507,"to":9513,"names":[],"notNames":[],"prefix":null},{"from":9536,"to":9587,"names":["mcp_oauth_connect"],"notNames":[],"prefix":null},{"from":9541,"to":9543,"names":[],"notNames":[],"prefix":null},{"from":9547,"to":9550,"names":[],"notNames":[],"prefix":null},{"from":9589,"to":9607,"names":["mcp_oauth_cancel"],"notNames":[],"prefix":null},{"from":9594,"to":9596,"names":[],"notNames":[],"prefix":null},{"from":9614,"to":9616,"names":[],"notNames":[],"prefix":null},{"from":9622,"to":9625,"names":[],"notNames":[],"prefix":null},{"from":9627,"to":9634,"names":[],"notNames":[],"prefix":null},{"from":9630,"to":9632,"names":[],"notNames":[],"prefix":null}],"hostKeyShapeGuards":[],"hostKeySource":"native/desktop_host/src/ipc_router.cpp:9181,9185-9196,9201-9205,9219,9445,9473,9535,9588,9608-9613,9617-9621,9626,9635-9642","hostKeyNarrowed":true,"extraInHost":[],"missingFromHost":[],"hostKeyResolved":[]},
    {"command":"mcp_server_start","camel":"mcpServerStart","line":547,"host":{"status":"handler","handler":"mcp_command","line":7567},"sidecar":{"status":"n/a","method":null,"line":null,"reason":null},"ui":[{"file":"apps/desktop/src/lib/mcp.ts","line":81,"text":"mutationFn: (serverId) => callDesktop(IPC_COMMANDS.mcpServerStart, { serverId }),"}],"uiLiteral":[],"fixture":null,"verdict":"端到端可达","responseKeys":["config","status","tools","unavailable"],"responseKind":"named","responseVia":"packages/shared/src/schemas/mcp.ts:152","hostKeys":["config","status","tools","unavailable"],"hostKeyOpaque":[],"hostKeyScope":{"from":9458,"to":9467,"via":"explicit-branch","handler":"mcp_command"},"hostKeyExcluded":[],"hostKeyShapeGuards":[],"hostKeySource":"native/desktop_host/src/ipc_router.cpp:9458-9467","hostKeyNarrowed":true,"extraInHost":[],"missingFromHost":[],"hostKeyResolved":[{"callee":"view_for","file":"native/desktop_host/src/ipc_router.cpp","line":9186,"keys":["config","status","tools","unavailable"]},{"callee":"start","file":"native/desktop_host/src/desktop_mcp.cpp","line":1166,"keys":["config","status","tools"]}]},
    {"command":"mcp_server_stop","camel":"mcpServerStop","line":551,"host":{"status":"handler","handler":"mcp_command","line":7567},"sidecar":{"status":"n/a","method":null,"line":null,"reason":null},"ui":[{"file":"apps/desktop/src/lib/mcp.ts","line":85,"text":"mutationFn: (serverId) => callDesktop(IPC_COMMANDS.mcpServerStop, { serverId }),"}],"uiLiteral":[],"fixture":null,"verdict":"端到端可达","responseKeys":["server","shutdown"],"responseKind":"named","responseVia":"packages/shared/src/schemas/mcp.ts:213","hostKeys":["server","shutdown"],"hostKeyOpaque":[],"hostKeyScope":{"from":9446,"to":9472,"via":"explicit-branch","handler":"mcp_command"},"hostKeyExcluded":[{"from":9451,"to":9453,"names":[],"notNames":[],"prefix":null},{"from":9454,"to":9456,"names":[],"notNames":[],"prefix":null},{"from":9458,"to":9467,"names":["mcp_server_start"],"notNames":[],"prefix":null}],"hostKeyShapeGuards":[],"hostKeySource":"native/desktop_host/src/ipc_router.cpp:9446-9450,9457,9468-9472","hostKeyNarrowed":true,"extraInHost":[],"missingFromHost":[],"hostKeyResolved":[{"callee":"stop","file":"native/desktop_host/src/desktop_mcp.cpp","line":1173,"keys":["server","shutdown"]}]},
    {"command":"mcp_server_review","camel":"mcpServerReview","line":555,"host":{"status":"handler","handler":"mcp_command","line":7566},"sidecar":{"status":"n/a","method":null,"line":null,"reason":null},"ui":[{"file":"apps/desktop/src/lib/mcp.ts","line":89,"text":"mutationFn: (input) => callDesktop(IPC_COMMANDS.mcpServerReview, input),"}],"uiLiteral":[],"fixture":null,"verdict":"端到端可达","responseKeys":["config","status","tools","unavailable"],"responseKind":"named","responseVia":"packages/shared/src/schemas/mcp.ts:152","hostKeys":["config","status","tools","unavailable"],"hostKeyOpaque":[],"hostKeyScope":{"from":9474,"to":9534,"via":"explicit-branch","handler":"mcp_command"},"hostKeyExcluded":[{"from":9483,"to":9485,"names":[],"notNames":[],"prefix":null},{"from":9491,"to":9493,"names":[],"notNames":[],"prefix":null},{"from":9497,"to":9499,"names":[],"notNames":[],"prefix":null},{"from":9501,"to":9503,"names":[],"notNames":[],"prefix":null},{"from":9507,"to":9513,"names":[],"notNames":[],"prefix":null}],"hostKeyShapeGuards":[],"hostKeySource":"native/desktop_host/src/ipc_router.cpp:9474-9482,9486-9490,9494-9496,9500,9504-9506,9514-9534","hostKeyNarrowed":true,"extraInHost":[],"missingFromHost":[],"hostKeyResolved":[{"callee":"view_for","file":"native/desktop_host/src/ipc_router.cpp","line":9186,"keys":["config","status","tools","unavailable"]}]},
    {"command":"mcp_oauth_connect","camel":"mcpOAuthConnect","line":559,"host":{"status":"handler","handler":"mcp_command","line":7568},"sidecar":{"status":"n/a","method":null,"line":null,"reason":null},"ui":[{"file":"apps/desktop/src/lib/mcp.ts","line":93,"text":"mutationFn: (serverId) => callDesktop(IPC_COMMANDS.mcpOAuthConnect, { serverId }),"}],"uiLiteral":[],"fixture":null,"verdict":"端到端可达","responseKeys":["serverId","issuer","scopes","tokenEndpoint"],"responseKind":"named","responseVia":"packages/shared/src/schemas/mcp.ts:184","hostKeys":["issuer","scopes","serverId","tokenEndpoint"],"hostKeyOpaque":[],"hostKeyScope":{"from":9536,"to":9587,"via":"explicit-branch","handler":"mcp_command"},"hostKeyExcluded":[{"from":9541,"to":9543,"names":[],"notNames":[],"prefix":null},{"from":9547,"to":9550,"names":[],"notNames":[],"prefix":null}],"hostKeyShapeGuards":[],"hostKeySource":"native/desktop_host/src/ipc_router.cpp:9536-9540,9544-9546,9551-9587","hostKeyNarrowed":true,"extraInHost":[],"missingFromHost":[],"hostKeyResolved":[{"callee":"oauth_connect","file":"native/desktop_host/src/desktop_oauth.cpp","line":1717,"keys":["issuer","scopes","serverId","tokenEndpoint"]},{"callee":"open_external_url","file":"native/desktop_host/src/ipc_router.cpp","line":10515,"keys":[]}]},
    {"command":"mcp_oauth_cancel","camel":"mcpOAuthCancel","line":563,"host":{"status":"handler","handler":"mcp_command","line":7568},"sidecar":{"status":"n/a","method":null,"line":null,"reason":null},"ui":[{"file":"apps/desktop/src/lib/mcp.ts","line":99,"text":"mutationFn: (serverId) => callDesktop(IPC_COMMANDS.mcpOAuthCancel, { serverId }),"}],"uiLiteral":[],"fixture":null,"verdict":"端到端可达","responseKeys":["accepted"],"responseKind":"named","responseVia":"packages/shared/src/schemas/mcp.ts:191","hostKeys":["accepted"],"hostKeyOpaque":[],"hostKeyScope":{"from":9589,"to":9607,"via":"explicit-branch","handler":"mcp_command"},"hostKeyExcluded":[{"from":9594,"to":9596,"names":[],"notNames":[],"prefix":null}],"hostKeyShapeGuards":[],"hostKeySource":"native/desktop_host/src/ipc_router.cpp:9589-9593,9597-9607","hostKeyNarrowed":true,"extraInHost":[],"missingFromHost":[],"hostKeyResolved":[]},
    {"command":"network_proxy_get","camel":"networkProxyGet","line":567,"host":{"status":"handler","handler":"network_proxy_command","line":6040},"sidecar":{"status":"n/a","method":null,"line":null,"reason":null},"ui":[{"file":"apps/desktop/src/lib/network.ts","line":34,"text":"queryFn: async () => await callDesktop(IPC_COMMANDS.networkProxyGet, {}),"}],"uiLiteral":[],"fixture":{"file":"apps/desktop/src/lib/preview-fixtures.ts","line":578},"verdict":"端到端可达","responseKeys":["mode","hasCredential","resolution"],"responseKind":"named","responseVia":"packages/shared/src/schemas/network.ts:27","hostKeys":["hasCredential","mode","resolution"],"hostKeyOpaque":[],"hostKeyScope":{"from":8359,"to":8368,"via":"explicit-branch","handler":"network_proxy_command"},"hostKeyExcluded":[{"from":8360,"to":8362,"names":[],"notNames":[],"prefix":null}],"hostKeyShapeGuards":[],"hostKeySource":"native/desktop_host/src/ipc_router.cpp:8359,8363-8368","hostKeyNarrowed":true,"extraInHost":[],"missingFromHost":[],"hostKeyResolved":[{"callee":"describe_network_proxy","file":"native/desktop_host/src/ipc_router.cpp","line":845,"keys":["hasCredential","mode","resolution"]}]},
    {"command":"network_proxy_save","camel":"networkProxySave","line":571,"host":{"status":"handler","handler":"network_proxy_command","line":6040},"sidecar":{"status":"n/a","method":null,"line":null,"reason":null},"ui":[{"file":"apps/desktop/src/lib/network.ts","line":81,"text":"return await callDesktop(IPC_COMMANDS.networkProxySave, { input: networkProxyInput(form) });"}],"uiLiteral":[],"fixture":null,"verdict":"端到端可达","responseKeys":["mode","hasCredential","resolution"],"responseKind":"named","responseVia":"packages/shared/src/schemas/network.ts:27","hostKeys":["hasCredential","mode","resolution"],"hostKeyOpaque":[],"hostKeyScope":{"from":8353,"to":8463,"via":"fall-through","handler":"network_proxy_command"},"hostKeyExcluded":[{"from":8355,"to":8357,"names":[],"notNames":[],"prefix":null},{"from":8359,"to":8368,"names":["network_proxy_get"],"notNames":[],"prefix":null},{"from":8360,"to":8362,"names":[],"notNames":[],"prefix":null},{"from":8385,"to":8390,"names":[],"notNames":[],"prefix":null},{"from":8386,"to":8388,"names":[],"notNames":[],"prefix":null},{"from":8392,"to":8397,"names":[],"notNames":[],"prefix":null},{"from":8393,"to":8395,"names":[],"notNames":[],"prefix":null},{"from":8398,"to":8400,"names":[],"notNames":[],"prefix":null},{"from":8401,"to":8403,"names":[],"notNames":[],"prefix":null},{"from":8414,"to":8416,"names":[],"notNames":[],"prefix":null},{"from":8418,"to":8420,"names":[],"notNames":[],"prefix":null},{"from":8439,"to":8442,"names":[],"notNames":[],"prefix":null},{"from":8447,"to":8455,"names":[],"notNames":[],"prefix":null}],"hostKeyShapeGuards":[],"hostKeySource":"native/desktop_host/src/ipc_router.cpp:8353-8354,8358,8369-8384,8391,8404-8413,8417,8421-8438,8443-8446,8456-8463","hostKeyNarrowed":true,"extraInHost":[],"missingFromHost":[],"hostKeyResolved":[{"callee":"describe_network_proxy","file":"native/desktop_host/src/ipc_router.cpp","line":845,"keys":["hasCredential","mode","resolution"]}]},
    {"command":"investigation_task_list","camel":"investigationTaskList","line":575,"host":{"status":"inline","handler":null,"line":6065},"sidecar":{"status":"host-local","method":null,"line":null},"ui":[],"uiLiteral":[{"file":"apps/desktop/src/features/investigations/InvestigationsPane.tsx","line":120,"text":"queryFn: () => callDesktop(\"investigation_task_list\", {}),"},{"file":"apps/desktop/src/features/overview/InfrastructureDashboard.tsx","line":27,"text":"queryFn: () => callDesktop(\"investigation_task_list\", {}),"}],"fixture":{"file":"apps/desktop/src/lib/preview-fixtures.ts","line":567},"verdict":"端到端可达","responseKeys":["tasks"],"responseKind":"named","responseVia":"packages/shared/src/schemas/investigation.ts:447","hostKeys":["tasks"],"hostKeyOpaque":[],"hostKeyScope":{"from":6065,"to":6101,"via":"explicit-branch","handler":"invoke"},"hostKeyExcluded":[{"from":6066,"to":6068,"names":[],"notNames":[],"prefix":null},{"from":6073,"to":6083,"names":[],"notNames":[],"prefix":null},{"from":6074,"to":6076,"names":[],"notNames":[],"prefix":null},{"from":6085,"to":6094,"names":[],"notNames":[],"prefix":null},{"from":6086,"to":6088,"names":[],"notNames":[],"prefix":null},{"from":6090,"to":6092,"names":[],"notNames":[],"prefix":null}],"hostKeyShapeGuards":[],"hostKeySource":"native/desktop_host/src/ipc_router.cpp:6065,6069-6072,6084,6095-6101","hostKeyNarrowed":true,"extraInHost":[],"missingFromHost":[],"hostKeyResolved":[{"callee":"list_investigation_tasks","file":"native/desktop_host/src/database.cpp","line":2952,"keys":["tasks"]}]},
    {"command":"investigation_task_get","camel":"investigationTaskGet","line":579,"host":{"status":"inline","handler":null,"line":6103},"sidecar":{"status":"host-local","method":null,"line":null},"ui":[{"file":"apps/desktop/src/features/workpages/WorkpagesDock.tsx","line":128,"text":"const task = useQuery({ queryKey: [\"investigationTask\", selectedTaskId], enabled: operationsOpen && Boolean(se"}],"uiLiteral":[{"file":"apps/desktop/src/features/investigations/InvestigationsPane.tsx","line":126,"text":"queryFn: () => callDesktop(\"investigation_task_get\", { taskId: selectedTaskId as string }),"}],"fixture":{"file":"apps/desktop/src/lib/preview-fixtures.ts","line":569},"verdict":"端到端可达","responseKeys":["task","evidence","findings","decisionBrief","plan","artifacts","runs","steps"],"responseKind":"named","responseVia":"packages/shared/src/schemas/investigation.ts:458","hostKeys":["artifacts","decisionBrief","evidence","findings","plan","runs","steps","task"],"hostKeyOpaque":[],"hostKeyScope":{"from":6103,"to":6122,"via":"explicit-branch","handler":"invoke"},"hostKeyExcluded":[{"from":6104,"to":6106,"names":[],"notNames":[],"prefix":null},{"from":6111,"to":6113,"names":[],"notNames":[],"prefix":null}],"hostKeyShapeGuards":[],"hostKeySource":"native/desktop_host/src/ipc_router.cpp:6103,6107-6110,6114-6122","hostKeyNarrowed":true,"extraInHost":[],"missingFromHost":[],"hostKeyResolved":[{"callee":"get_investigation_task","file":"native/desktop_host/src/database.cpp","line":2998,"keys":["artifacts","decisionBrief","evidence","findings","plan","runs","steps","task"]}]},
    {"command":"investigation_task_create","camel":"investigationTaskCreate","line":583,"host":{"status":"inline","handler":null,"line":6124},"sidecar":{"status":"host-local","method":null,"line":null},"ui":[],"uiLiteral":[{"file":"apps/desktop/src/features/investigations/InvestigationsPane.tsx","line":158,"text":"mutationFn: (input: InvestigationTaskCreateInput) => callDesktop(\"investigation_task_create\", { input }),"}],"fixture":null,"verdict":"端到端可达","responseKeys":["task"],"responseKind":"named","responseVia":"packages/shared/src/schemas/investigation.ts:451","hostKeys":["task"],"hostKeyOpaque":[],"hostKeyScope":{"from":6124,"to":6151,"via":"explicit-branch","handler":"invoke"},"hostKeyExcluded":[{"from":6125,"to":6127,"names":[],"notNames":[],"prefix":null},{"from":6133,"to":6137,"names":[],"notNames":[],"prefix":null},{"from":6140,"to":6143,"names":[],"notNames":[],"prefix":null}],"hostKeyShapeGuards":[],"hostKeySource":"native/desktop_host/src/ipc_router.cpp:6124,6128-6132,6138-6139,6144-6151","hostKeyNarrowed":true,"extraInHost":[],"missingFromHost":[],"hostKeyResolved":[]},
    {"command":"investigation_task_start","camel":"investigationTaskStart","line":587,"host":{"status":"inline","handler":null,"line":6153},"sidecar":{"status":"host-local","method":null,"line":null},"ui":[],"uiLiteral":[{"file":"apps/desktop/src/features/investigations/InvestigationsPane.tsx","line":144,"text":"mutationFn: (taskId: string) => callDesktop(\"investigation_task_start\", { taskId }),"}],"fixture":null,"verdict":"端到端可达","responseKeys":["runId","started"],"responseKind":"named","responseVia":"packages/shared/src/schemas/investigation.ts:453","hostKeys":["runId","started"],"hostKeyOpaque":[],"hostKeyScope":{"from":6153,"to":6383,"via":"explicit-branch","handler":"invoke"},"hostKeyExcluded":[{"from":6154,"to":6156,"names":[],"notNames":[],"prefix":null},{"from":6161,"to":6163,"names":[],"notNames":[],"prefix":null},{"from":6180,"to":6182,"names":[],"notNames":[],"prefix":null},{"from":6183,"to":6196,"names":[],"notNames":[],"prefix":null},{"from":6201,"to":6203,"names":[],"notNames":[],"prefix":null},{"from":6207,"to":6235,"names":[],"notNames":[],"prefix":null},{"from":6208,"to":6214,"names":[],"notNames":[],"prefix":null},{"from":6210,"to":6213,"names":[],"notNames":[],"prefix":null},{"from":6218,"to":6223,"names":[],"notNames":[],"prefix":null},{"from":6276,"to":6315,"names":[],"notNames":[],"prefix":null},{"from":6281,"to":6310,"names":[],"notNames":[],"prefix":null},{"from":6284,"to":6309,"names":[],"notNames":[],"prefix":null},{"from":6290,"to":6300,"names":[],"notNames":[],"prefix":null},{"from":6301,"to":6306,"names":[],"notNames":[],"prefix":null},{"from":6317,"to":6322,"names":[],"notNames":[],"prefix":null},{"from":6323,"to":6350,"names":[],"notNames":[],"prefix":null},{"from":6328,"to":6335,"names":[],"notNames":[],"prefix":null},{"from":6330,"to":6333,"names":[],"notNames":[],"prefix":null},{"from":6339,"to":6341,"names":[],"notNames":[],"prefix":null},{"from":6370,"to":6376,"names":[],"notNames":[],"prefix":null},{"from":6377,"to":6381,"names":[],"notNames":[],"prefix":null}],"hostKeyShapeGuards":[],"hostKeySource":"native/desktop_host/src/ipc_router.cpp:6153,6157-6160,6164-6179,6197-6200,6204-6206,6236-6275,6316,6351-6369,6382-6383","hostKeyNarrowed":true,"extraInHost":[],"missingFromHost":[],"hostKeyResolved":[]},
    {"command":"investigation_task_stop","camel":"investigationTaskStop","line":591,"host":{"status":"inline","handler":null,"line":6569},"sidecar":{"status":"host-local","method":null,"line":null},"ui":[],"uiLiteral":[{"file":"apps/desktop/src/features/investigations/InvestigationsPane.tsx","line":151,"text":"mutationFn: (taskId: string) => callDesktop(\"investigation_task_stop\", { taskId }),"}],"fixture":null,"verdict":"端到端可达","responseKeys":["task"],"responseKind":"named","responseVia":"packages/shared/src/schemas/investigation.ts:451","hostKeys":["task"],"hostKeyOpaque":[],"hostKeyScope":{"from":6569,"to":6612,"via":"explicit-branch","handler":"invoke"},"hostKeyExcluded":[{"from":6570,"to":6572,"names":[],"notNames":[],"prefix":null},{"from":6577,"to":6579,"names":[],"notNames":[],"prefix":null}],"hostKeyShapeGuards":[],"hostKeySource":"native/desktop_host/src/ipc_router.cpp:6569,6573-6576,6580-6612","hostKeyNarrowed":true,"extraInHost":[],"missingFromHost":[],"hostKeyResolved":[]},
    {"command":"investigation_task_status_update","camel":"investigationTaskStatusUpdate","line":595,"host":{"status":"inline","handler":null,"line":6524},"sidecar":{"status":"host-local","method":null,"line":null},"ui":[],"uiLiteral":[],"fixture":null,"verdict":"UI 无调用点（死命令）","responseKeys":["task"],"responseKind":"named","responseVia":"packages/shared/src/schemas/investigation.ts:451","hostKeys":["task"],"hostKeyOpaque":[],"hostKeyScope":{"from":6524,"to":6567,"via":"explicit-branch","handler":"invoke"},"hostKeyExcluded":[{"from":6525,"to":6527,"names":[],"notNames":[],"prefix":null},{"from":6535,"to":6537,"names":[],"notNames":[],"prefix":null},{"from":6543,"to":6551,"names":[],"notNames":[],"prefix":null},{"from":6544,"to":6546,"names":[],"notNames":[],"prefix":null},{"from":6548,"to":6550,"names":[],"notNames":[],"prefix":null},{"from":6554,"to":6556,"names":[],"notNames":[],"prefix":null}],"hostKeyShapeGuards":[],"hostKeySource":"native/desktop_host/src/ipc_router.cpp:6524,6528-6534,6538-6542,6552-6553,6557-6567","hostKeyNarrowed":true,"extraInHost":[],"missingFromHost":[],"hostKeyResolved":[]},
    {"command":"investigation_task_recover","camel":"investigationTaskRecover","line":599,"host":{"status":"inline","handler":null,"line":6385},"sidecar":{"status":"host-local","method":null,"line":null},"ui":[],"uiLiteral":[{"file":"apps/desktop/src/features/investigations/InvestigationsPane.tsx","line":192,"text":"mutationFn: (input: { taskId: string; optionId?: string }) => callDesktop(\"investigation_task_recover\", { inpu"}],"fixture":null,"verdict":"端到端可达","responseKeys":["task"],"responseKind":"named","responseVia":"packages/shared/src/schemas/investigation.ts:451","hostKeys":["task"],"hostKeyOpaque":[],"hostKeyScope":{"from":6385,"to":6522,"via":"explicit-branch","handler":"invoke"},"hostKeyExcluded":[{"from":6386,"to":6388,"names":[],"notNames":[],"prefix":null},{"from":6398,"to":6400,"names":[],"notNames":[],"prefix":null},{"from":6402,"to":6410,"names":[],"notNames":[],"prefix":null},{"from":6403,"to":6405,"names":[],"notNames":[],"prefix":null},{"from":6407,"to":6409,"names":[],"notNames":[],"prefix":null},{"from":6423,"to":6437,"names":[],"notNames":[],"prefix":null},{"from":6426,"to":6433,"names":[],"notNames":[],"prefix":null},{"from":6428,"to":6431,"names":[],"notNames":[],"prefix":null},{"from":6434,"to":6436,"names":[],"notNames":[],"prefix":null},{"from":6438,"to":6440,"names":[],"notNames":[],"prefix":null},{"from":6445,"to":6449,"names":[],"notNames":[],"prefix":null},{"from":6452,"to":6454,"names":[],"notNames":[],"prefix":null},{"from":6469,"to":6485,"names":[],"notNames":[],"prefix":null},{"from":6474,"to":6483,"names":[],"notNames":[],"prefix":null},{"from":6476,"to":6480,"names":[],"notNames":[],"prefix":null},{"from":6498,"to":6510,"names":[],"notNames":[],"prefix":null}],"hostKeyShapeGuards":[],"hostKeySource":"native/desktop_host/src/ipc_router.cpp:6385,6389-6397,6401,6411-6422,6441-6444,6450-6451,6455-6468,6486-6497,6511-6522","hostKeyNarrowed":true,"extraInHost":[],"missingFromHost":[],"hostKeyResolved":[]},
    {"command":"investigation_brief_select","camel":"investigationBriefSelect","line":603,"host":{"status":"inline","handler":null,"line":6614},"sidecar":{"status":"host-local","method":null,"line":null},"ui":[],"uiLiteral":[{"file":"apps/desktop/src/features/investigations/InvestigationsPane.tsx","line":179,"text":"mutationFn: (input: InvestigationBriefSelectInput) => callDesktop(\"investigation_brief_select\", { input }),"}],"fixture":null,"verdict":"端到端可达","responseKeys":["brief","continuation"],"responseKind":"named","responseVia":"packages/shared/src/schemas/investigation.ts:494","hostKeys":["brief","continuation"],"hostKeyOpaque":[],"hostKeyScope":{"from":6614,"to":6714,"via":"explicit-branch","handler":"invoke"},"hostKeyExcluded":[{"from":6615,"to":6617,"names":[],"notNames":[],"prefix":null},{"from":6631,"to":6633,"names":[],"notNames":[],"prefix":null},{"from":6658,"to":6665,"names":[],"notNames":[],"prefix":null},{"from":6660,"to":6663,"names":[],"notNames":[],"prefix":null},{"from":6668,"to":6691,"names":[],"notNames":[],"prefix":null},{"from":6672,"to":6690,"names":[],"notNames":[],"prefix":null},{"from":6675,"to":6685,"names":[],"notNames":[],"prefix":null},{"from":6705,"to":6707,"names":[],"notNames":[],"prefix":null}],"hostKeyShapeGuards":[],"hostKeySource":"native/desktop_host/src/ipc_router.cpp:6614,6618-6630,6634-6657,6666-6667,6692-6704,6708-6714","hostKeyNarrowed":true,"extraInHost":[],"missingFromHost":[],"hostKeyResolved":[]},
    {"command":"investigation_retention_preview","camel":"investigationRetentionPreview","line":607,"host":{"status":"inline","handler":null,"line":6716},"sidecar":{"status":"host-local","method":null,"line":null},"ui":[],"uiLiteral":[{"file":"apps/desktop/src/features/investigations/InvestigationsPane.tsx","line":220,"text":"mutationFn: (taskId: string) => callDesktop(\"investigation_retention_preview\", { taskId }),"}],"fixture":null,"verdict":"端到端可达","responseKeys":["preview"],"responseKind":"named","responseVia":"packages/shared/src/schemas/retention.ts:42","hostKeys":["preview"],"hostKeyOpaque":[],"hostKeyScope":{"from":6716,"to":6765,"via":"explicit-branch","handler":"invoke"},"hostKeyExcluded":[{"from":6717,"to":6719,"names":[],"notNames":[],"prefix":null},{"from":6725,"to":6727,"names":[],"notNames":[],"prefix":null},{"from":6729,"to":6738,"names":[],"notNames":[],"prefix":null},{"from":6730,"to":6732,"names":[],"notNames":[],"prefix":null},{"from":6734,"to":6737,"names":[],"notNames":[],"prefix":null},{"from":6742,"to":6752,"names":[],"notNames":[],"prefix":null},{"from":6743,"to":6746,"names":[],"notNames":[],"prefix":null},{"from":6748,"to":6750,"names":[],"notNames":[],"prefix":null},{"from":6755,"to":6757,"names":[],"notNames":[],"prefix":null}],"hostKeyShapeGuards":[],"hostKeySource":"native/desktop_host/src/ipc_router.cpp:6716,6720-6724,6728,6739-6741,6753-6754,6758-6765","hostKeyNarrowed":true,"extraInHost":[],"missingFromHost":[],"hostKeyResolved":[]},
    {"command":"investigation_retention_prune","camel":"investigationRetentionPrune","line":611,"host":{"status":"inline","handler":null,"line":6767},"sidecar":{"status":"host-local","method":null,"line":null},"ui":[],"uiLiteral":[{"file":"apps/desktop/src/features/investigations/InvestigationsPane.tsx","line":223,"text":"mutationFn: (input: InvestigationRetentionPruneInput) => callDesktop(\"investigation_retention_prune\", { input "}],"fixture":null,"verdict":"端到端可达","responseKeys":["result"],"responseKind":"named","responseVia":"packages/shared/src/schemas/retention.ts:65","hostKeys":["result"],"hostKeyOpaque":[],"hostKeyScope":{"from":6767,"to":6824,"via":"explicit-branch","handler":"invoke"},"hostKeyExcluded":[{"from":6768,"to":6770,"names":[],"notNames":[],"prefix":null},{"from":6782,"to":6784,"names":[],"notNames":[],"prefix":null},{"from":6786,"to":6788,"names":[],"notNames":[],"prefix":null},{"from":6789,"to":6792,"names":[],"notNames":[],"prefix":null},{"from":6794,"to":6796,"names":[],"notNames":[],"prefix":null},{"from":6806,"to":6808,"names":[],"notNames":[],"prefix":null},{"from":6814,"to":6816,"names":[],"notNames":[],"prefix":null}],"hostKeyShapeGuards":[],"hostKeySource":"native/desktop_host/src/ipc_router.cpp:6767,6771-6781,6785,6793,6797-6805,6809-6813,6817-6824","hostKeyNarrowed":true,"extraInHost":[],"missingFromHost":[],"hostKeyResolved":[]},
    {"command":"investigation_schedule_list","camel":"investigationScheduleList","line":615,"host":{"status":"inline","handler":null,"line":6826},"sidecar":{"status":"host-local","method":null,"line":null},"ui":[],"uiLiteral":[{"file":"apps/desktop/src/features/investigations/InvestigationsPane.tsx","line":132,"text":"queryFn: () => callDesktop(\"investigation_schedule_list\", {}),"},{"file":"apps/desktop/src/features/overview/InfrastructureDashboard.tsx","line":31,"text":"queryFn: () => callDesktop(\"investigation_schedule_list\", {}), refetchInterval: 60_000,"}],"fixture":{"file":"apps/desktop/src/lib/preview-fixtures.ts","line":572},"verdict":"端到端可达","responseKeys":["schedules"],"responseKind":"named","responseVia":"packages/shared/src/schemas/schedule.ts:72","hostKeys":["schedules"],"hostKeyOpaque":[],"hostKeyScope":{"from":6826,"to":6839,"via":"explicit-branch","handler":"invoke"},"hostKeyExcluded":[{"from":6827,"to":6829,"names":[],"notNames":[],"prefix":null},{"from":6830,"to":6832,"names":[],"notNames":[],"prefix":null}],"hostKeyShapeGuards":[],"hostKeySource":"native/desktop_host/src/ipc_router.cpp:6826,6833-6839","hostKeyNarrowed":true,"extraInHost":[],"missingFromHost":[],"hostKeyResolved":[{"callee":"list_investigation_schedules","file":"native/desktop_host/src/database.cpp","line":4041,"keys":["schedules"]}]},
    {"command":"investigation_schedule_runs","camel":"investigationScheduleRuns","line":619,"host":{"status":"inline","handler":null,"line":6841},"sidecar":{"status":"host-local","method":null,"line":null},"ui":[],"uiLiteral":[{"file":"apps/desktop/src/features/investigations/InvestigationsPane.tsx","line":141,"text":"queryFn: () => callDesktop(\"investigation_schedule_runs\", { scheduleId: selectedScheduleId as string }),"}],"fixture":{"file":"apps/desktop/src/lib/preview-fixtures.ts","line":574},"verdict":"端到端可达","responseKeys":["runs"],"responseKind":"named","responseVia":"packages/shared/src/schemas/schedule.ts:76","hostKeys":["runs"],"hostKeyOpaque":[],"hostKeyScope":{"from":6841,"to":6860,"via":"explicit-branch","handler":"invoke"},"hostKeyExcluded":[{"from":6842,"to":6844,"names":[],"notNames":[],"prefix":null},{"from":6850,"to":6853,"names":[],"notNames":[],"prefix":null}],"hostKeyShapeGuards":[],"hostKeySource":"native/desktop_host/src/ipc_router.cpp:6841,6845-6849,6854-6860","hostKeyNarrowed":true,"extraInHost":[],"missingFromHost":[],"hostKeyResolved":[{"callee":"list_investigation_schedule_runs","file":"native/desktop_host/src/database.cpp","line":4069,"keys":["runs"]}]},
    {"command":"investigation_schedule_create","camel":"investigationScheduleCreate","line":623,"host":{"status":"inline","handler":null,"line":6862},"sidecar":{"status":"host-local","method":null,"line":null},"ui":[],"uiLiteral":[{"file":"apps/desktop/src/features/investigations/InvestigationsPane.tsx","line":207,"text":"mutationFn: (taskId: string) => callDesktop(\"investigation_schedule_create\", {"}],"fixture":null,"verdict":"端到端可达","responseKeys":["schedule"],"responseKind":"inline-strict","responseVia":null,"hostKeys":["schedule"],"hostKeyOpaque":[],"hostKeyScope":{"from":6862,"to":6948,"via":"explicit-branch","handler":"invoke"},"hostKeyExcluded":[{"from":6863,"to":6865,"names":[],"notNames":[],"prefix":null},{"from":6877,"to":6888,"names":[],"notNames":[],"prefix":null},{"from":6890,"to":6892,"names":[],"notNames":[],"prefix":null},{"from":6904,"to":6909,"names":[],"notNames":[],"prefix":null},{"from":6906,"to":6908,"names":[],"notNames":[],"prefix":null}],"hostKeyShapeGuards":[],"hostKeySource":"native/desktop_host/src/ipc_router.cpp:6862,6866-6876,6889,6893-6903,6910-6948","hostKeyNarrowed":true,"extraInHost":[],"missingFromHost":[],"hostKeyResolved":[]},
    {"command":"investigation_schedule_update","camel":"investigationScheduleUpdate","line":627,"host":{"status":"inline","handler":null,"line":6950},"sidecar":{"status":"host-local","method":null,"line":null},"ui":[],"uiLiteral":[{"file":"apps/desktop/src/features/investigations/InvestigationsPane.tsx","line":213,"text":"mutationFn: (input: InvestigationScheduleUpdateInput) => callDesktop(\"investigation_schedule_update\", { input "}],"fixture":null,"verdict":"端到端可达","responseKeys":["schedule"],"responseKind":"inline-strict","responseVia":null,"hostKeys":["schedule"],"hostKeyOpaque":[],"hostKeyScope":{"from":6950,"to":7097,"via":"explicit-branch","handler":"invoke"},"hostKeyExcluded":[{"from":6951,"to":6953,"names":[],"notNames":[],"prefix":null},{"from":6966,"to":6980,"names":[],"notNames":[],"prefix":null},{"from":6992,"to":6995,"names":[],"notNames":[],"prefix":null},{"from":7000,"to":7002,"names":[],"notNames":[],"prefix":null},{"from":7007,"to":7009,"names":[],"notNames":[],"prefix":null},{"from":7020,"to":7024,"names":[],"notNames":[],"prefix":null},{"from":7025,"to":7028,"names":[],"notNames":[],"prefix":null},{"from":7031,"to":7033,"names":[],"notNames":[],"prefix":null},{"from":7034,"to":7036,"names":[],"notNames":[],"prefix":null},{"from":7037,"to":7039,"names":[],"notNames":[],"prefix":null},{"from":7040,"to":7042,"names":[],"notNames":[],"prefix":null},{"from":7044,"to":7046,"names":[],"notNames":[],"prefix":null},{"from":7048,"to":7080,"names":[],"notNames":[],"prefix":null},{"from":7049,"to":7051,"names":[],"notNames":[],"prefix":null},{"from":7054,"to":7057,"names":[],"notNames":[],"prefix":null},{"from":7065,"to":7067,"names":[],"notNames":[],"prefix":null},{"from":7068,"to":7070,"names":[],"notNames":[],"prefix":null},{"from":7071,"to":7073,"names":[],"notNames":[],"prefix":null},{"from":7074,"to":7077,"names":[],"notNames":[],"prefix":null},{"from":7087,"to":7089,"names":[],"notNames":[],"prefix":null}],"hostKeyShapeGuards":[],"hostKeySource":"native/desktop_host/src/ipc_router.cpp:6950,6954-6965,6981-6991,6996-6999,7003-7006,7010-7019,7029-7030,7043,7047,7081-7086,7090-7097","hostKeyNarrowed":true,"extraInHost":[],"missingFromHost":[],"hostKeyResolved":[]},
    {"command":"investigation_schedule_tick","camel":"investigationScheduleTick","line":631,"host":{"status":"inline","handler":null,"line":7099},"sidecar":{"status":"host-local","method":null,"line":null},"ui":[],"uiLiteral":[],"fixture":null,"verdict":"UI 无调用点（死命令）","responseKeys":["claimed","skipped"],"responseKind":"named","responseVia":"packages/shared/src/schemas/schedule.ts:85","hostKeys":["claimed","skipped"],"hostKeyOpaque":[],"hostKeyScope":{"from":7099,"to":7136,"via":"explicit-branch","handler":"invoke"},"hostKeyExcluded":[{"from":7100,"to":7102,"names":[],"notNames":[],"prefix":null},{"from":7113,"to":7119,"names":[],"notNames":[],"prefix":null},{"from":7115,"to":7118,"names":[],"notNames":[],"prefix":null},{"from":7121,"to":7127,"names":[],"notNames":[],"prefix":null},{"from":7123,"to":7125,"names":[],"notNames":[],"prefix":null}],"hostKeyShapeGuards":[],"hostKeySource":"native/desktop_host/src/ipc_router.cpp:7099,7103-7112,7120,7128-7136","hostKeyNarrowed":true,"extraInHost":[],"missingFromHost":[],"hostKeyResolved":[{"callee":"claim_due_investigation_schedule_runs","file":"native/desktop_host/src/database.cpp","line":4198,"keys":["claimed","skipped"]}]},
    {"command":"provider_test","camel":"providerTest","line":635,"host":{"status":"handler","handler":"provider_command","line":7554},"sidecar":{"status":"implemented","method":"provider.test","line":276,"reason":null,"details":[{"method":"provider.test","ok":true,"line":276}],"source":"native/desktop_host/src/ipc_router.cpp:9643-9665"},"ui":[{"file":"apps/desktop/src/features/onboarding/GettingStarted.tsx","line":32,"text":"await callDesktop(IPC_COMMANDS.providerTest, { providerId: candidate.id });"}],"uiLiteral":[{"file":"apps/desktop/src/lib/preview-chat.ts","line":6,"text":"\"provider_list\", \"provider_save\", \"provider_activate\", \"provider_delete\", \"provider_models\", \"provider_test\","}],"fixture":null,"verdict":"端到端可达","responseKeys":["ok"],"responseKind":"inline-strict","responseVia":null,"hostKeys":[],"hostKeyOpaque":[{"callee":null,"rpcForward":true,"text":"client.request(found->second, params, std::chrono::milliseconds(60000)"}],"hostKeyScope":{"from":9643,"to":9665,"via":"fall-through","handler":"provider_command"},"hostKeyExcluded":[{"from":9653,"to":9655,"names":[],"notNames":[],"prefix":null}],"hostKeyShapeGuards":[],"hostKeySource":"native/desktop_host/src/ipc_router.cpp:9643-9652,9656-9665","hostKeyNarrowed":true,"extraInHost":[],"missingFromHost":["ok"],"hostKeyResolved":[]}
  ]
}
```
