# C++ 接管与 Rust 移除计划

这份文档把“什么时候可以删掉 Rust/Tauri”从原则变成可勾选的清单。删除动作本身只有一行，但它的前置条件很多；本文给出命令级盘点、依赖顺序和删除时的清理清单。

**硬门槛：在本文“完成标准”逐条勾上之前，不要删除任何 Rust/Tauri 代码。** 这也是 `docs/cpp-desktop-handoff.md` 的既定规则。

---

## 1. 为什么现在不能删

- 桌面 UI 有 **81 个 Tauri 命令**（`apps/desktop/src-tauri/src/**`，`#[tauri::command]`）。
- C++ 桌面宿主**已实现约 18 个**；其余返回 `NOT_IMPLEMENTED`，并且**不回退 Tauri**。
- `pnpm package` 走的是 `tauri build`（Rust 宿主）——安装包的实际宿主是 Rust。
- `pnpm check` 跑 `cargo fmt/clippy/check/test`。
- Rust 还承载 C++ 尚未接管的共享能力：36 段 SQLite 迁移的权威副本、OS 凭据边界、出站代理、host-trust、MCP、以及对 Python Agent sidecar 的**启动与监管**。
- Python Agent 到 C++ 宿主 RPC 的完整桥接仍未接入。

直接删除的后果：桌面应用不可用、`pnpm check`/`pnpm package` 失败、迁移与凭据的权威实现消失。

---

## 2. C++ 宿主已实现（18）

`native/desktop_host/src/ipc_router.cpp`

`core_ping`、`shell.open_url`、`server_list`、`workspace_list`、`server_add`、`server_update`、`server_delete`、`activity_list`、`tool_execution_list`、`network_proxy_get`、`network_proxy_save`、`chat_session_list`、`chat_session_get`、`chat_session_create`、`chat_message_append`、`chat_session_archive`、`chat_session_rename`、`chat_session_delete`。

带 `desktop_host_*` 的 CTest 目标覆盖这些命令的数据/迁移/self-check。

---

## 3. 剩余命令盘点（按依赖分组）

### A. 纯本机数据 / 本地文件（不触网，最容易先迁）

- ✅ `server_host_key_status`、`server_host_key_forget`（已知主机存储已在 `native/desktop_host/src/host_trust.cpp` 实现；`v1:host:port:SHA256:...` 格式与 `crates/host-trust` 一致）
- `file_transfer_list`、`file_transfer_get`（任务/账本查询）
- `local_path_pick`、`local_file_preview`、`emit_local_drop`（本机对话框/预览/拖入事件）

### B. 需要 C++ SSH / SFTP / PTY 核心（`native/src` 已有能力）

- `server_host_key_probe`、`server_host_key_trust`（探针 + 一次性 probe ticket）
- `remote_file_list`、`remote_file_read`、`remote_file_preview`
- `file_prepare_remote_drag`、`file_drag_out_start`
- `file_transfer_upload`、`file_transfer_download`、`file_transfer_cancel`、`file_transfer_resolve_conflict`、`transfer`
- `terminal_open`、`terminal_write`、`terminal_resize`、`terminal_close`（C++ PTY）
- `server_connect`、`server_disconnect`、`server_auth_respond`、`server_auth_cancel`（连接生命周期 / 键盘交互认证）
- `server_snapshot`、`server_services`、`server_logs`（读操作，Python 做归一化）

### C. 走 Python sidecar（Rust 现在就是转发）

`apps/desktop/src-tauri/src/commands/provider.rs`、`commands/server/mod.rs`、`commands/agent_run.rs`、`commands/investigation.rs` 里的若干命令，实际逻辑在 `agent/src/yukinal_agent/`：

- `provider_list`、`provider_save`、`provider_activate`、`provider_delete`、`provider_models`、`provider_test`
- `agent_run_start`、`agent_run_stop`、`agent_approval_respond`
- `server_snapshot`、`server_services`、`server_logs` 的归一化部分
- `investigation_*`（15）中的持久化/恢复部分

要把这些迁进 C++ 宿主，**必须先有 C++ ↔ Python sidecar 桥**（见 §4）。

### D. 进程监管与 MCP

- `agent_spawn`、`agent_status`、`agent_kill`、`agent_logs`（Python sidecar 生命周期）
- `mcp_server_list`、`mcp_server_save`、`mcp_server_delete`、`mcp_server_start`、`mcp_server_stop`、`mcp_server_review`、`mcp_oauth_connect`、`mcp_oauth_cancel`

---

## 4. 关键路径：C++ ↔ Python sidecar 桥

这是 provider / MCP / agent / investigation 的前置，也是整个迁移的瓶颈。参考实现是 `crates/core/src/sidecar/`（约 1100 行）：

- `crates/core/src/sidecar/config.rs`：解析 Python/PyInstaller 入口、环境白名单、`YUKINAL_DATA_DIR`。
- `crates/core/src/sidecar/mod.rs`：`Command` 启动、`env_clear` + 最小环境、stdout/stderr 读取任务、NDJSON 帧（上限 24 MiB）、请求/响应按 `id` 关联、`initialize` 握手、退出与重启监管。
- `apps/desktop/src-tauri/src/commands/host/`：host RPC 服务端（`host.credentials.set/get/reclaim`、`host.mcp.catalog`、`host.tool.execute`、`host.investigation.*`、审批 ticket 等）。
- Python 侧协议见 `agent/src/yukinal_agent/sidecar.py`（`initialize`、`handle`/`handle_async`、方法表）与 `docs/rewrite-cpp-python.md` 的 “Processes and trust boundaries”。

C++ 侧需要一个等价物：进程启动 + 双向 NDJSON + 请求关联 + host RPC 回调。建议先在开发宿主上以 `--python-agent <python> --agent-source <dir>`（或复用 `YUKINAL_*` 环境）接通，再逐命令转发。

---

## 5. 建议施工顺序

1. **纯本机数据组（§3A）**：host-trust 移植 + transfer 查询 + 本机路径。
2. **C++ ↔ Python 桥（§4）**：先通 `initialize` + `provider_*`，再 `host.credentials.*`、`host.mcp.catalog`、`host.tool.execute`。
3. **Provider / Agent / Investigation 转发（§3C）**：把 Rust 的转发命令改由 C++ 宿主转发。
4. **MCP 生命周期与 OAuth（§3D）** + `agent_*` 监管。
5. **SSH/SFTP/PTY 组（§3B）**：连接、host key、终端、文件、传输、审批。
6. **三平台构建与安装验收**：Windows/macOS/Linux 桌面构建、升级、用户数据保留、真实 UI 冒烟。
7. **删除 Rust/Tauri**（见 §7），并同步清理门禁/打包/CI/文档。

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
  - 已实现（19）：`core_ping`、`shell.open_url`、`server_list`、`workspace_list`、`server_add`、`server_update`、`server_delete`、`activity_list`、`tool_execution_list`、`network_proxy_get`、`network_proxy_save`、`server_host_key_status`、`server_host_key_forget`、`chat_session_*`（7）。
- [ ] C++ ↔ Python sidecar 桥接通，provider/agent/investigation/MCP 走该桥。
- [ ] 36 段迁移与新安装/旧库升级在 C++ 侧通过，凭据仍只进 OS 库。
- [ ] SSH/SFTP/PTY/审批在 C++ 侧覆盖现有 UI 业务路径，无 Rust SSH 回退。
- [ ] `pnpm check` 不再依赖 cargo；`pnpm package` 用 C++ 宿主产出安装包。
- [ ] Windows/macOS/Linux 安装、升级、用户数据保留、真实 UI 冒烟通过。
- [ ] 删除清单（§7）全部执行并复验。
