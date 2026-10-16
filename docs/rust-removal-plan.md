# C++ 接管与 Rust 移除计划

这份文档把 C++ 接管拆成可勾选的清单，并记录删除 Rust/Tauri 时的清理范围。

> **状态：Rust/Tauri 已删除。** `crates/`、`apps/desktop/src-tauri/`、`Cargo.toml`/`Cargo.lock`/`rust-toolchain.toml`/`.cargo/`，以及已退休的 legacy Node agent（`apps/agent/`）都已从仓库移除；`pnpm check` 已不再依赖 cargo，前端不再回退 Tauri。**本文件现在是“从零实现 C++ 宿主能力”的指导，而不是“何时删除”的门槛。** 删除时向 C++ 手工追回的参考实现在 `docs/rewrite-cpp-python.md` 和 git 历史里。

## 0. 已删除的内容（本文件原本的前置条件）

- 桌面 UI 原有 **81 个 Tauri 命令**（`crates/core` + `apps/desktop/src-tauri`），其中 C++ 宿主**已实现约 20 个**，其余仍返回 `NOT_IMPLEMENTED`。
- `pnpm package` 曾走 `tauri build`；现在 **打包/安装器尚未实现**。
- `pnpm check` 曾跑 `cargo fmt/clippy/check/test`；现在已移除。
- 原本由 Rust 承载的共享能力（36 段 SQLite 迁移权威副本、OS 凭据边界、出站代理、host-trust、MCP、Python Agent sidecar 启动与监管）需要按下面顺序在 C++/Python 中重新提供。
- Python Agent 到 C++ 宿主 RPC 的桥接仍未接入。

---

## 1. 迁移前的盘点（保留作参考）

- Rust 还承载 C++ 尚未接管的共享能力：36 段 SQLite 迁移的权威副本、OS 凭据边界、出站代理、host-trust、MCP、以及对 Python Agent sidecar 的**启动与监管**。
- Python Agent 到 C++ 宿主 RPC 的完整桥接仍未接入。

---

## 2. C++ 宿主已实现

`native/desktop_host/src/ipc_router.cpp`

`core_ping`、`shell.open_url`、`server_list`、`workspace_list`、`server_add`、`server_update`、`server_delete`、`activity_list`、`tool_execution_list`、`network_proxy_get`、`network_proxy_save`、`chat_session_list`、`chat_session_get`、`chat_session_create`、`chat_message_append`、`chat_session_archive`、`chat_session_rename`、`chat_session_delete`。

带 `desktop_host_*` 的 CTest 目标覆盖这些命令的数据/迁移/self-check。

### 2.1 C++ ↔ Python sidecar 桥（已接通首个里程碑）

`native/desktop_host/src/sidecar.cpp` 提供 `SidecarClient`：负责 Python Agent 子进程的启动/监管、24 MiB 上限的 NDJSON 分帧、请求/响应按 id 关联、`agent.stream` 通知回传，以及 Python 侧反向 `host.*` RPC 的分发。`native/desktop_host/src/ipc_router.cpp` 通过它实现：

- `agent_spawn`、`agent_status`、`agent_kill`、`agent_logs`（进程监管与 stderr 尾部）；
- `agent_run_start`、`agent_run_stop`、`agent_approval_respond`（转发到 Python `agent.run.*`/`agent.approval.respond`；payload 原样透传，由 Python Agent Node 负责持久受理、停止与一次性审批，同步 start 使用长超时，宿主的 `host.*` worker 继续应答）；
- `provider_list`、`provider_save`、`provider_activate`、`provider_delete`、`provider_models`、`provider_test`（转发到 Python `provider.*`）；
- `host.credentials.set`、`host.credentials.get`、`host.credentials.reclaim`（写/读/回收 OS 凭据库）；
- `host.mcp.catalog`（当前返回空目录）、`host.tool.cancel`（确认）。

`desktop_host_sidecar_tests` 用真实 Python Agent 验证启动、握手、ping、provider 凭据轮换与回收，以及 router 转发（含 run 受理、未知 run 停止、过期审批应答，以及缺少 provider 时的错误映射）。开发用 `pnpm desktop:dev:cpp` 会探测 `agent/.venv` 并把 `YUKINAL_AGENT_PYTHON`/`YUKINAL_AGENT_SOURCE` 传给宿主。MCP 目录、investigation 与其余 `host.tool.execute` 仍是下一步。

---

## 3. 剩余命令盘点（按依赖分组）

### A. 纯本机数据 / 本地文件（不触网，最容易先迁）

- ✅ `server_host_key_status`、`server_host_key_forget`（已知主机存储已在 `native/desktop_host/src/host_trust.cpp` 实现；`v1:host:port:SHA256:...` 格式与 `crates/host-trust` 一致）
- ✅ `local_path_pick`、`local_file_preview`（`native/desktop_host/src/local_paths.cpp` 提供短时句柄表：15 分钟 TTL、2048 句柄上限、256 选取上限、拒绝符号链接与变更类型；预览复用共享的 `FilePreviewResponse`，1 MiB 上限并按文本/图片/二进制分类；Windows 用原生文件/目录对话框，其他平台暂时返回明确错误）
- `file_transfer_list`、`file_transfer_get`（`native/desktop_host/src/database.cpp` 读取 `file_transfers` 账本；本宿主没有传输管理器，因此持久化的 `queued`/`running`/`waitingConflict` 行在读取时对账为 `interrupted` 并写回，已完成/失败等终态保持不变）
- `emit_local_drop`（OS 拖入事件）

### B. 需要 C++ SSH / SFTP / PTY 核心（`native/src` 已有能力）

- ✅ `server_host_key_probe`、`server_host_key_trust`（C++ SSH 探针 + 单次有效、5 分钟过期的 probe ticket，绑定 serverId/host/port/fingerprint；已有不同 pin 时拒绝，要求先 forget）
- ✅ `remote_file_list`、`remote_file_read`、`remote_file_preview`（in-process SFTP core：有界目录列表（≤4096 项，名称归一化成绝对路径）、1 MiB 有界读取（有损 UTF-8）、preview 先 stat 确认常规文件再读，复用共享 `make_file_preview`；浏览器路径策略不套用 Agent 凭据黑名单）
- `file_prepare_remote_drag`、`file_drag_out_start`
- `file_transfer_upload`、`file_transfer_download`、`file_transfer_cancel`、`file_transfer_resolve_conflict`、`transfer`
- ✅ `terminal_open`、`terminal_write`、`terminal_resize`、`terminal_close`（`native/desktop_host/src/desktop_terminal.cpp` 生成单目标临时配置并启动 `yukinal-ssh-service --terminal --cols --rows` 子进程（凭据走子进程环境），按 NDJSON 帧转发 `terminal.opened`/`terminal.data`/`terminal.closed` 事件；真实 PTY 验收待靶机放通）
- ✅ `server_connect`、`server_disconnect`（in-process `connection_check`，成功/失败写 server status + 活动记录；disconnect 幂等）；`server_auth_respond`、`server_auth_cancel`（有界校验；键盘交互挑战由 C++ PTY worker 持有，暂未接 PTY 时正确返回 `accepted:false`）
- ✅ `server_logs`（`native/desktop_host/src/desktop_ssh.cpp` 用 in-process C++ SSH core 调 `server_logs`，再把原始 stdout 交给 Python `server.logs.normalize` 归一化）
- ✅ `server_services`（同上，跑 `service_inventory` + Python `server.services.normalize`）
- ✅ `server_snapshot`（跑固定 collector 命令集 + nonce 分隔解析，交给 Python `server.snapshot.normalize`，校验 health/capabilities/collectors 后写入 `snapshots` 表再返回）

### C. 走 Python sidecar（Rust 现在就是转发）

✅ Provider 六个命令（`provider_list`、`provider_save`、`provider_activate`、`provider_delete`、`provider_models`、`provider_test`）与 `agent_run_start`、`agent_run_stop`、`agent_approval_respond` 已由 C++ 宿主经 sidecar 桥转发到 Python，凭据经 `host.credentials.*` 落在 C++ OS 凭据边界。

`apps/desktop/src-tauri/src/commands/provider.rs`、`commands/server/mod.rs`、`commands/agent_run.rs`、`commands/investigation.rs` 里的其余命令，实际逻辑在 `agent/src/yukinal_agent/`：

- ✅ provider 六个命令（见上）；
- ✅ `agent_run_start`、`agent_run_stop`、`agent_approval_respond`（原样转发，Python 拥有持久受理/停止/审批状态）；
- `server_snapshot`、`server_services`、`server_logs` 的归一化部分；
- `investigation_*`（15）中的持久化/恢复部分。

这些剩余命令已具备 C++ ↔ Python sidecar 桥，server 归一化与 `investigation_*` 仍需补 `host.tool.execute` 等 RPC。

### D. 进程监管与 MCP

- ✅ `agent_spawn`、`agent_status`、`agent_kill`、`agent_logs`（Python sidecar 生命周期）
- ✅ `mcp_server_list`、`mcp_server_save`、`mcp_server_delete`（`mcp_servers` 表 CRUD + 静态 HTTP 认证头 secret 进 OS 凭据库；OAuth 尚未支持，保存时明确拒绝）
- `mcp_server_start`、`mcp_server_stop`、`mcp_server_review`、`mcp_oauth_connect`、`mcp_oauth_cancel`（依赖 MCP 进程监管/McpSupervisor 与 OAuth 流程）

---

## 4. 关键路径：C++ ↔ Python sidecar 桥

**已接通首个里程碑。** 参考实现是 `crates/core/src/sidecar/`（约 1100 行）：

- `crates/core/src/sidecar/config.rs`：解析 Python/PyInstaller 入口、环境白名单、`YUKINAL_DATA_DIR`。
- `crates/core/src/sidecar/mod.rs`：`Command` 启动、`env_clear` + 最小环境、stdout/stderr 读取任务、NDJSON 帧（上限 24 MiB）、请求/响应按 `id` 关联、`initialize` 握手、退出与重启监管。
- `apps/desktop/src-tauri/src/commands/host/`：host RPC 服务端（`host.credentials.set/get/reclaim`、`host.mcp.catalog`、`host.tool.execute`、`host.investigation.*`、审批 ticket 等）。
- Python 侧协议见 `agent/src/yukinal_agent/sidecar.py`（`initialize`、`handle`/`handle_async`、方法表）与 `docs/rewrite-cpp-python.md` 的 “Processes and trust boundaries”。

C++ 侧需要一个等价物：进程启动 + 双向 NDJSON + 请求关联 + host RPC 回调。建议先在开发宿主上以 `--python-agent <python> --agent-source <dir>`（或复用 `YUKINAL_*` 环境）接通，再逐命令转发。

---

## 5. 建议施工顺序

1. **纯本机数据组（第 3 节A）**：host-trust 移植 + transfer 查询 + 本机路径。
2. **C++ ↔ Python 桥（第 4 节）**：先通 `initialize` + `provider_*`，再 `host.credentials.*`、`host.mcp.catalog`、`host.tool.execute`。
3. **Provider / Agent / Investigation 转发（第 3 节C）**：把 Rust 的转发命令改由 C++ 宿主转发。
4. **MCP 生命周期与 OAuth（第 3 节D）** + `agent_*` 监管。
5. **SSH/SFTP/PTY 组（第 3 节B）**：连接、host key、终端、文件、传输、审批。
6. **三平台构建与安装验收**：Windows/macOS/Linux 桌面构建、升级、用户数据保留、真实 UI 冒烟。
7. **删除 Rust/Tauri**（见 第 7 节），并同步清理门禁/打包/CI/文档。

每一轮只纳入已完成且可复核的文件，更新文档，构建受影响的 C++ 目标并跑测试。

---

## 6. Rust 组件去向

| Rust 组件 | 去向 |
| --- | --- |
| `apps/desktop/src-tauri` | 命令迁完后整体删除；React UI 由 C++ WebView 宿主承载 |
| `crates/database` | C++ `desktop_host/src/database.cpp` + `schema_migrations.hpp`（已镜像 36 段迁移）；删前做一次迁移兼容核对 |
| `crates/credentials` | C++ `desktop_host/src/credential_store.cpp`（已实现接口 + OS 后端） |
| `crates/net` | C++ `desktop_host/src/network_proxy.cpp` + Python `provider_store.py`/`models.py`（代理解析已有） |
| `crates/host-trust` | 待移植进 C++ 宿主（known_hosts v1 格式） |
| `crates/core`（sidecar/MCP/工具/supervisor） | sidecar 监管与 host RPC 由 C++ 宿主接管；MCP 配置/生命周期由 C++/Python 分工 |
| `crates/filesystem`、`crates/terminal` | 由 C++ SFTP/PTY 服务承接 |
| `crates/time` | 进入 C++/Python 各自需要处 |

---

## 7. 删除时的清理清单

删除前确认“完成标准”全勾，然后：

1. 删除：`crates/`、`apps/desktop/src-tauri/`、`Cargo.toml`、`Cargo.lock`、`rust-toolchain.toml`、`.cargo/`。
2. `package.json`：移除 `test` 里的 tauri 测试引用；`package` 改为 C++ 宿主打包。
3. `scripts/check.mjs`：移除 cargo fmt/clippy/check/test 步骤；CTest 仍需保留。
4. `scripts/package.mjs`、`scripts/build-*`、NSIS/MSI/DMG/deb 相关脚本与 smoke：改为 C++ 宿主。
5. `scripts/lib/tauri-agent-command.mjs` 及其测试：删除或替换。
6. `.github/workflows/check.yml`、`package.yml`：移除 Rust 工具链、cargo 缓存、`cargo audit`。
7. `docs/`：更新 `native-desktop-host.md`、`rewrite-cpp-python.md`、`cpp-desktop-handoff.md`、`acceptance-handoff.md`、`release.md`、`changelog.md`、`README.md`。
8. 跑 `pnpm check` 与三平台安装验收，确认没有对 `cargo`/`src-tauri` 的悬挂引用。

---

## 8. 完成标准（逐条勾）

- [ ] 81 个 UI 命令在 C++ 宿主全部有实现，或明确标注为“尚未迁移”且 UI 不依赖。
  - 已实现（56）：`core_ping`、`shell.open_url`、`server_list`、`workspace_list`、`server_add`、`server_update`、`server_delete`、`server_connect`、`server_disconnect`、`server_auth_respond`、`server_auth_cancel`、`terminal_open`、`terminal_write`、`terminal_resize`、`terminal_close`、`activity_list`、`tool_execution_list`、`network_proxy_get`、`network_proxy_save`、`server_host_key_status`、`server_host_key_forget`、`server_host_key_probe`、`server_host_key_trust`、`server_logs`、`server_services`、`server_snapshot`、`remote_file_list`、`remote_file_read`、`remote_file_preview`、`chat_session_*`（7）、`local_path_pick`、`local_file_preview`、`file_transfer_list`、`file_transfer_get`、`mcp_server_list`、`mcp_server_save`、`mcp_server_delete`、`agent_spawn`、`agent_status`、`agent_kill`、`agent_logs`、`agent_run_start`、`agent_run_stop`、`agent_approval_respond`、`provider_*`（6）。
- [ ] C++ ↔ Python sidecar 桥接通，provider/agent/investigation/MCP 走该桥。
  - Provider 六个命令、`agent_spawn/status/kill/logs`、`agent_run_*`/`agent_approval_respond` 与 MCP 设置 CRUD 已经走 C++；`host.ssh.service_target` 与文件读/写/编辑目标已实现，MCP 进程目录、investigation 与其余 `host.tool.execute` 仍待补。
- [ ] 36 段迁移与新安装/旧库升级在 C++ 侧通过，凭据仍只进 OS 库。
- [ ] SSH/SFTP/PTY/审批在 C++ 侧覆盖现有 UI 业务路径，无 Rust SSH 回退。
- [ ] `pnpm check` 不再依赖 cargo；`pnpm package` 用 C++ 宿主产出安装包。
- [ ] Windows/macOS/Linux 安装、升级、用户数据保留、真实 UI 冒烟通过。
- [ ] 删除清单（第 7 节）全部执行并复验。
