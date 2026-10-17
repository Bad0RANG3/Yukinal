# Yukinal 交接手册：剩余目标与实现指引

> 面向接手后续开发的工程师/Agent。范围是「C++ 桌面宿主 + Python Agent」这条 2.0 主线
> 在 [`docs/rust-removal-plan.md`](./rust-removal-plan.md) 第 8 节验收标准上的剩余工作。
>
> 阅读顺序建议：先读本文件第 1、2 节建立环境与现状，再按第 3 节挑一个块开工；第 4、5 节是
> 必须遵守的约定和会踩的坑。

---

## 1. 现状总览

### 1.1 架构与进程边界

```text
React UI（WebView）
  │ window.__YUKINAL_HOST__（本机 in-process 绑定 + 事件）
  ▼
C++ 桌面宿主 yukinal-desktop-host.exe        ← 唯一桌面宿主
  ├── SQLite（%APPDATA%/dev.yukinal.workspace/yukinal.db，36 段迁移）
  ├── OS 凭据库（credential_store.cpp）
  ├── known_hosts（host_trust.cpp）
  ├── WebView 窗体外壳（windows_window_frame.cpp / window_context.cpp）
  ├── Python Agent sidecar（NDJSON/JSON-RPC，sidecar.cpp）
  │     └── LangGraph / LangChain / MCP / provider（agent/src/yukinal_agent）
  ├── in-process C++ SSH core（yukinal_ssh_core）
  │     ├── 结构化 SSH 操作（server_logs / snapshot / services / connection_check …）
  │     ├── SFTP（remote_file_list/read/preview）
  │     └── PTY（terminal_*，派生子进程 yukinal-ssh-service --terminal）
  └── MCP supervisor（desktop_mcp.cpp，stdio 子进程 + tools/list + tools/call）
```

关键边界（不要打破）：

- **秘密只走两个通道**：Python Agent 交回给 C++ 的 `{target, credentials}` 里的
  `credentials` 映射；以及派生子进程的环境变量。target JSON 只放环境变量名，永远不放明文。
- **权限判断、审批、持久化在 C++ 宿主**；模型只提计划。
- **跨层形状以 `packages/shared` 为准**；C++ 不要自造字段名。

### 1.2 已实现（截至本手册）

UI 命令 81 条中已实现 62 条，剩余 19 条（见 3.x）。已经打通的组：

- 本机数据/文件：`server_list/workspace_list/activity_list/tool_execution_list`、
  `server_add/update/delete`、`server_host_key_status/forget/probe/trust`、
  `network_proxy_get/save`、`chat_session_*`（7）、`local_path_pick`、`local_file_preview`、
  `file_transfer_list/get`、`shell.open_url`、`core_ping`。
- SSH 读：`server_logs`、`server_services`、`server_snapshot`、`server_connect/disconnect/auth_*`、
  `remote_file_list/read/preview`、`terminal_open/write/resize/close`。
- MCP 设置与进程：`mcp_server_list/save/delete/review/start/stop`。
- Agent 桥：`agent_spawn/status/kill/logs`、`agent_run_start/stop`、`agent_approval_respond`、
  `provider_*`（6）。
- investigation 读+状态：`investigation_task_list/get/status_update/stop`。

宿主 RPC（Python → C++，见 `packages/shared/src/types/host.ts`）已实现：

- `host.credentials.set/get/reclaim`、`host.mcp.catalog`、`host.tool.cancel`、
  `host.tool.execute`（**仅 `mcp.*` 分支**）。
- `host.ssh.service_target`、`host.ssh.filesystem_read_target`、
  `host.ssh.filesystem_write_target`、`host.ssh.filesystem_edit_target`。

对应实现文件：

| 文件 | 职责 |
| --- | --- |
| `native/desktop_host/src/ipc_router.cpp` | 所有 UI 命令路由 + `host.*` RPC 服务端 |
| `native/desktop_host/src/database.cpp` | SQLite 读写（含迁移校验） |
| `native/desktop_host/src/desktop_target.cpp` | 服务器+身份+凭据+信任源+代理 → 单目标 payload |
| `native/desktop_host/src/desktop_ssh.cpp` | in-process SSH/SFTP 操作封装 + 非 native 解析 |
| `native/desktop_host/src/desktop_terminal.cpp` | PTY 子进程 + 事件转发 |
| `native/desktop_host/src/desktop_mcp.cpp` | MCP stdio 子进程 + catalog + tools/call |
| `native/desktop_host/src/host_trust.cpp` | known_hosts v1 |
| `native/desktop_host/src/network_proxy.cpp` | 系统/环境代理解析 |
| `native/desktop_host/src/sidecar.cpp` | Python Agent 监管 + NDJSON 分帧 + host RPC 分发 |

---

## 2. 环境、构建、测试

### 2.1 构建与测试

```powershell
# C++（一个配置好的构建目录即可，规范用 native/build-nmake，Release）
cmake --build native/build-nmake --config Release
ctest --test-dir native/build-nmake -C Release --output-on-failure   # 12/12

# 全量门禁（含 TS typecheck、desktop 单测、bundle、Python sidecar smoke、CTest）
$env:YUKINAL_SSH_CMAKE_BUILD_DIR = "native/build-nmake"
pnpm check
```

`native/build-nmake` 已配置 `YUKINAL_BUILD_SSH_SERVICE=ON`（所以 `YUKINAL_DESKTOP_HOST_WITH_SSH_CORE`
打开，in-process SSH 可用）。`pnpm desktop:dev:cpp` 用的是 `native/build-cpp-host`，它把 SSH 关掉，
只用来跑 UI；该构建下 SSH/PTY/SFTP 命令会明确报 “C++ SSH core is not available”。

### 2.2 **必须知道的坑：MSVC 依赖跟踪不全**

这台机器上 NMake 的 `/showIncludes` 依赖跟踪**会漏记头文件**。表现：

- 改了 `ipc_router.hpp`（加成员/改布局）后，只重编了 `ipc_router.cpp`，而 `main.cpp`、
  `test_*.cpp` 的旧 `.obj` 仍按旧布局链接 → 启动即 `0xC0000005`（access violation）或行为错乱。
- 改了 `database.hpp` 只加方法不会改布局，一般安全；**加数据成员一定危险**。

**规避**：改动任何 `desktop_host/include/**` 头文件后，删除受影响目标的 `.obj` 再构建：

```bat
for %%D in (yukinal-desktop-host yukinal_desktop_host_core yukinal_desktop_host_tests ^
            yukinal_desktop_host_database_tests yukinal_desktop_host_sidecar_tests ^
            yukinal_window_context_tests) do rmdir /s /q native\build-nmake\CMakeFiles\%%D.dir
cmake -S native -B native/build-nmake
cmake --build native/build-nmake --config Release
```

（`native/build-nmake/CMakeFiles/*.dir` 需要先确保没有 `yukinal-desktop-host.exe` 在跑，
否则链接会 `LNK1104 无法打开文件`。）

### 2.3 其它环境注意

- 跑 CTest 前先 `taskkill /F /IM yukinal-desktop-host.exe`（Windows 会锁住 exe）。
- `cnm`/Git Bash 下调用 cmd 要用 `cmd.exe //c ...`（单斜杠会被路径转换）。
- 端到端 MCP 测试用测试可执行文件自己当 mock（`--mcp-mock`），无需第三方包。
- 网络到 `github.com:443` 时通时断；`git push` 失败就隔一会儿重试。

### 2.4 已知阻塞

- **靶机 SSH 未验收**：`43.240.221.2:22` 被中间设备过滤（任意端口“接受后挂住”，不发 banner）。
  所有 SSH/SFTP/PTY 命令都只有本地 + Paramiko mock 验证，没有真机 OpenSSH 验证。
  需要放行出口 IP、或提供可达地址/跳板/VPN。
- **MCP 只有 stdio**：HTTP 传输、OAuth、取消未实现。
- **安装器未实现**：`scripts/package.mjs` 已能产出 C++ 宿主分发布局（宿主 + 冻结 Agent + SSH 服务 + 许可证）与 `SHA256SUMS.txt`、`release-manifest.json`，并可用 `pnpm package -- --verify` 复核；NSIS/MSI/DMG/deb 安装器、三平台升级与数据保留仍未做。

---

## 3. 剩余目标（按块，含契约、参考实现、依赖、验收）

> 参考实现都在 git 历史里：被删除的 Rust 宿主位于修订 `45fef20^`
> （`git show 45fef20^:<path>`）。这是权威行为来源。

### 3.1 MCP：HTTP 传输 + OAuth + 取消（中等偏大）

**剩余命令**：`mcp_oauth_connect`、`mcp_oauth_cancel`。
**剩余能力**：`transport == "http"` 的 server 目前保存时被拒；`McpSupervisor` 只支持 stdio。

契约：`packages/shared/src/types/mcp.ts`、`packages/shared/src/schemas/mcp.ts`、
`McpServerConfig.oauth`（`McpOAuthConfig`）。
参考：`45fef20^:crates/core/src/mcp/http.rs`、`.../handle.rs`、`.../catalog.rs`、
`45fef20^:apps/desktop/src-tauri/src/commands/mcp/oauth.rs`、`.../oauth/flow.rs`、
`.../oauth/callback.rs`、`.../mcp/dpop.rs`、`commands/mcp/configuration.rs`。

实现要点：

- HTTP 传输是 **Streamable HTTP**：POST JSON-RPC，`Accept: application/json, text/event-stream`；
  响应可能是 SSE（`data:` 行）；会话头 `Mcp-Session-Id`。需要引入 HTTP 客户端
  （`httplib::httplib` 已在 `native/CMakeLists.txt` 里被 `yukinal-desktop-host` 使用；给
  `yukinal_desktop_host_core` 加同样的链接与 `CPPHTTPLIB_OPENSSL_SUPPORT` 时要小心
  `yukinal_ssh_core` 已用同宏编译 httplib，避免 ODR 不同）。
- 静态头 `httpAuthHeaders` 的 secret 从 OS 凭据库取（`McpSupervisor` 已持有 `credentials`）。
- OAuth：issuer 发现、动态注册、authorization_code/device_code、client secret、DPoP（RFC 9449）、
  token bundle 存凭据库；`mcp_oauth_connect` 打开浏览器回调、`mcp_oauth_cancel` 停止流程。
  `mcp_server_save` 现在对 `oauth` 直接 `NOT_IMPLEMENTED`，需要放开并实现凭据保留/回收语义。
- 取消：`host.tool.execute` 的 MCP 调用要能响应 `host.tool.cancel`（当前 `McpSupervisor::request`
  是阻塞等待，没有取消）。

验收：加一个 HTTP mock（loopback）测 initialize/tools/list/tools/call；OAuth 用本地 mock
issuer；`pnpm check` 全绿。

### 3.2 investigation 写入路径（大）

**剩余命令（11）**：`investigation_task_create`、`investigation_task_start`、
`investigation_task_recover`、`investigation_brief_select`、`investigation_retention_preview`、
`investigation_retention_prune`、`investigation_schedule_list`、`investigation_schedule_runs`、
`investigation_schedule_create`、`investigation_schedule_update`、`investigation_schedule_tick`。

契约：`packages/shared/src/schemas/investigation.ts`、`schemas/investigation.ts`、
`schemas/schedule.ts`、`schemas/retention.ts`。
参考：`45fef20^:apps/desktop/src-tauri/src/commands/investigation.rs`（1230 行）、
`.../investigation/policy.rs`、`.../commands/scheduler.rs`、`.../commands/retention` 相关。

依赖与要点：

- `investigation_task_create`：校验 `scope`（ToolTarget）、guardrails、budget 默认值，
  生成 `itask_*` id，写入 `investigation_tasks`（表已含全部列：guardrails/phase/created_by/
  max_attempts/active_run_id/last_failure）。参考 `create_task` 附近逻辑。
- `investigation_task_start`：**深度绑定 Agent run**——它构造 `autonomous_task_prompt`，再走
  `start_agent_run`（provider 解析、run 受理、permission mode、sidecar 监管）。C++ 侧要
  创建 `investigation_runs` 行并把 run 与 task 绑定，然后调用 sidecar 的 `agent.run.start`
  （C++ 已经会转发 `agent_run_start`）。参考 `start_investigation_task`。
- `investigation_task_recover`：恢复等待审批的 run（有精确前置条件），见
  `resume_waiting_approval`。
- `investigation_brief_select`：写 `investigation_decision_briefs.selected_option_id` + 继续动作。
- retention/schedule：`investigation_schedules`、`investigation_schedule_runs` 表的 CRUD 与
  tick 语义；注意 guardrails 与 `InvestigationRetentionPreview` 的 host 默认 cutoff。

配套的宿主 RPC（见 3.4）必须先落地，否则 Agent 无法写计划/证据，UI 也看不到内容。

验收：DB-only 部分可直接 `desktop_host_database_tests` 覆盖；start/recover 需要 mock
sidecar 或复用 `desktop_host_sidecar_tests` 的真 Agent。

### 3.3 文件传输动作 + 拖拽（大）

**剩余命令（6）**：`file_transfer_upload`、`file_transfer_download`、`file_transfer_cancel`、
`file_transfer_resolve_conflict`、`file_prepare_remote_drag`、`file_drag_out_start`。
**另有内部事件**：`emit_local_drop`（OS 拖入 → `file.local_dropped` 事件）。

契约：`packages/shared/src/types/file.ts`（`TransferSnapshot` 等）、`schemas/ipc.ts`、
`schemas/file.ts`、事件 `file.local_dropped`/`file.transfer*`（`packages/shared/src/events`）。
参考：`45fef20^:apps/desktop/src-tauri/src/commands/files/transfer.rs`（传输管理器）、
`.../files/drag_out.rs`、`.../files/local_paths.rs`。

要点：

- 传输需要**持久化传输管理器**：`file_transfers` 表 + `snapshot_json`/`staging_json`
  （C++ 已有 `list_file_transfers`/`get_file_transfer` 只读，并在读取时把 running→interrupted
  对账）。上传/下载要复用 C++ SFTP 流（`sftp-read`/`sftp-create-exclusive`/`sftp-publish-no-replace`
  /`sftp-stream`），并维护进度事件。
- 冲突解决（skip/overwrite/rename）、取消、恢复语义要与 `TransferSnapshot` 契约一致。
- `file_prepare_remote_drag`/`file_drag_out_start`：一次性 staging ticket + OS 原生拖拽。
  Windows 下需要 IDropSource/DoDragDrop；`emit_local_drop` 需要给 WebView 窗口注册
  IDropTarget（或 `WM_DROPFILES`）——注意 WebView 子窗口才是落点。
- 本机句柄表 `LocalPathHandles`（`local_paths.{hpp,cpp}`）已实现，传输读取本机路径时复用它。

验收：本地 + Paramiko mock 传小文件；断线/重启后 `interrupted` 对账；冲突/取消路径。

### 3.4 宿主 RPC（Agent ↔ C++，大）

Python Agent 会调用但 C++ 尚未实现的 `host.*` 方法（全部在 `handle_host_request` 里分发，
抛 `SidecarRpcError` 表示失败）：

**SSH 目标类**（`packages/shared/src/types/host.ts` 无，但 `agent/src/yukinal_agent/ssh_tools.py`
调用）：

- `host.ssh.filesystem_backup_target`、`host.ssh.filesystem_restore_target`、
  `host.ssh.filesystem_cleanup_target`：在 `resolve_ssh_service_target` 基础上加
  `filesystemBackup/filesystemRestore` 元数据，但**前置是 durable plan 校验 + 备份账本**。
- `host.ssh.effectful_action_target`、`host.ssh.effectful_action_finished`：远程变更动作的
  目标解析与完成记账。参考 `45fef20^:apps/desktop/src-tauri/src/commands/host.rs`
  （`resolve_ssh_effectful_action_target`）与 `commands/host/tools.rs`。

**investigation 类**（`packages/shared/src/types/host.ts` 有完整定义）：

- `host.investigation.plan.record`、`plan.check`、`plan.step_result`
- `host.investigation.evidence.record/fetch/search/correlate/compare`
- `host.investigation.finding.record`、`brief.record`、`artifact.record`
- `host.investigation.retention.preview`
- `host.investigation.filesystem_backup.register/restored/deleted`
- `host.filesystem.backup.list_query`、`host.filesystem.backup.retention_scan`
- `host.context.fetch`

参考：`45fef20^:apps/desktop/src-tauri/src/commands/host/plan.rs`（1766 行）、`evidence.rs`
（978 行）、`tools.rs`（2422 行）、`context.rs`。这些表在 C++ 的 36 段迁移里都已存在
（`investigation_plans`/`_steps`/`_evidence`/`_findings`/`_decision_briefs`/`_artifacts`/
`_runs`），`database.cpp` 已能**读**它们（见 `investigation_task_get`），缺的是**写 + 校验**。

**`host.tool.execute` 的非 MCP 分支**：目前只处理 `mcp.*`；其它工具名返回
`{status:"failed", error}`。当桌面没有配置 managed SSH 服务时，`server.*`/`docker.*`/
`systemd.*`/`package.*`/`filesystem.*` 会走 `host.tool.execute`，需要在 C++ 里分发到
`desktop_ssh.cpp`/SFTP。参考 `host/tools.rs` 的 `host_tool_execute`。

验收：每个 RPC 用 `desktop_host_database_tests`/`desktop_host_sidecar_tests` 覆盖；优先做
plan.record/plan.check/evidence.record（它们是 investigation 写入的地基）。

### 3.5 安装包与三平台验收（大，需真机）

- **已完成（分发布局与完整性清单）**：`scripts/package.mjs` 取代旧的 Tauri 打包路径，把 C++ 桌面宿主、
  冻结 Python Agent（PyInstaller onedir）与 C++ SSH 服务暂存成
  `target/release/bundle/yukinal-<version>-<platform>-<arch>/`，并写出 `SHA256SUMS.txt` 与
  `release-manifest.json`（记录版本、Agent 版本、平台/架构、源码修订、工作区是否脏、每个文件的
  SHA-256/字节数与启动所需的 `YUKINAL_AGENT_ENTRY`/`YUKINAL_SSH_SERVICE_BINARY` 约定）。
  `pnpm release:manifest` 可重建清单，`pnpm package -- --verify` 会逐字节复核并在不一致时非零退出。
  缺失宿主/Agent/SSH 服务产物时 fail-closed，不会产出部分载荷。
- **未完成**：无安装器产物（NSIS/MSI/DMG/deb）、无升级与用户数据保留验收、无真实 UI 冒烟、无代码签名。
- 需要 Windows/macOS/Linux 的安装器（NSIS/MSI/DMG/deb）、升级、用户数据保留、真实 UI 冒烟。
- 参考 `docs/rust-removal-plan.md` 第 7 节删除清理清单与第 8 节完成标准，以及
  `docs/release.md`、`.github/workflows/package.yml`（该 workflow 目前仍不存在，仓库只有 `check.yml`）。

### 3.6 控制平面 / PostgreSQL / 多租户（第四阶段，尚未开工）

brief 第 4 阶段：FastAPI 控制平面 + PostgreSQL（租户/节点/任务/模型配置/审批/审计）、节点主动
HTTPS/WSS 连接、任务持久化与断线恢复、并发控制、指标。当前仓库完全没有该子系统，属于新增
模块，不影响现有单机路径。

### 3.7 真机验收

- 放通靶机 SSH 后，用 `scripts/test_ssh_container.py` 或真实主机验证：连接/快照/日志/服务/
  远端文件/终端 host key 流程全部走真机。
- MCP 用真实 stdio/HTTP server 验证 catalog 与工具调用。
- 目前唯一被验证的是 Paramiko mock + 测试可执行文件自带的 mock MCP server。

### 3.8 收尾核对

- `docs/rust-removal-plan.md` 第 8 节完成标准逐条勾选。
- 命令计数以 `scripts/check.mjs` 与 `packages/shared` 的契约为准；每次加命令同步更新
  `docs/rust-removal-plan.md`、`docs/cpp-desktop-handoff.md`、`docs/rewrite-cpp-python.md` 的
  已实现清单与日期记录。

---

## 4. 必须遵守的约定

1. **秘密不进 JSON/日志**：target JSON 只放环境变量名；`dump()` 出来的对象里搜不到明文
   （已有测试断言）。新增任何带 secret 的命令都要照做。
2. **默认拒绝**：未配置的主机/工具/操作一律拒绝；host key 必须匹配已知 pin 或 CA。
3. **跨层形状以 `packages/shared` 为准**：C++ 输出的字段名/枚举/可空性必须能被对应
   Zod schema 解析；`dispose` 未在契约里的字段会被 `z.strictObject` 拒绝。
4. **写操作先做计划/审批/账本校验**，再动远端；中断的调用保持 outcome-unknown，不自动重放。
5. **不要伪造构建/测试结果**：声称通过就必须实际跑过 `ctest` 与 `pnpm check`，并在文档里
   如实写“未验证”。
6. **不要回退到 Rust/Tauri**：Rust 宿主已删除；未迁移命令返回 `NOT_IMPLEMENTED` 是正确的，
   不要偷偷接回旧宿主。

---

## 5. 建议执行顺序

1. **invesigation 写入地基**：`host.investigation.plan.record/check` + `evidence.record` +
   对应 DB 写入方法。这是 3.2 和 3.4 的共同前置，做完解锁最多命令。
2. **investigation 命令**：`task_create` → `task_start`/`recover` → `brief_select` → schedule/
   retention。
3. **MCP HTTP + OAuth + 取消**（3.1），让远程 MCP 与审批取消可用。
4. **文件传输动作 + 拖拽**（3.3）。
5. **host.tool.execute 非 MCP 分支**（3.4 的最后一块）。
6. **安装包与三平台验收**（3.5）：分发布局 + 完整性清单已做；安装器与真机验收仍待做。
7. **控制平面**（3.6，独立阶段）。
8. 全程：每块一个已验证提交；同步文档；真机可用时补真机验收（3.7）。

---

## 6. 关键文件速查

| 想知道 | 看哪里 |
| --- | --- |
| UI 命令名与参数/响应 | `packages/shared/src/ipc/index.ts` |
| UI 命令 Zod 校验 | `packages/shared/src/schemas/ipc.ts` |
| 事件名与负载 | `packages/shared/src/events/index.ts` |
| 宿主 RPC 契约 | `packages/shared/src/types/host.ts` |
| C++ 命令路由 | `native/desktop_host/src/ipc_router.cpp`（`invoke` + `handle_host_request`） |
| 16 段 investigation 表结构 | `native/desktop_host/src/schema_migrations.hpp` |
| 旧 Rust 参考实现 | `git show 45fef20^:<path>` |
| 迁移总计划与状态 | `docs/rust-removal-plan.md`、`docs/rewrite-cpp-python.md` |
| 命令级现状 | `docs/cpp-desktop-handoff.md` |
| 交付/验收 | `docs/acceptance-handoff.md`、`docs/architecture-and-build-plan.md` |
