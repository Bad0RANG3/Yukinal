# C++ 桌面宿主交接手册

本文用于接手 Yukinal 的桌面宿主重写。当前方向是保留 React UI 的外观与交互，把桌面窗口、IPC、SQLite、凭据管理和桌面能力逐步迁到 C++；不要再为桌面功能补 Rust 实现。Python Agent 仍按开发任务书负责 LangGraph、模型和工具编排，之后由 C++ 桌面宿主承接它需要的本地授权与数据能力。

## 当前检查点

截至 2026-10-15，上一检查点为 `d2594ff`（`Move schema migrations to C++ desktop host`）。当前 C++ 桌面宿主可打开 React UI、提供本机 IPC 桥、初始化 SQLite 数据库并执行版本 1 到 36 的迁移，已实现服务器增删改、OS 凭据库读写、活动记录、聊天历史命令，以及本轮迁入的网络代理设置命令（`network_proxy_get`、`network_proxy_save`）。网络代理的入参校验、入参中写入凭据的擦除、凭据轮换/保留/清除与失败回收都落在 C++；解析结果区分环境变量、Windows/macOS 系统代理与 PAC 失败关闭。

C++ 桌面宿主核心、Windows 可执行文件及桌面宿主测试（IPC router、数据库迁移、self-check、sidecar 桥）已在本机构建并通过。数据库测试已改为用真实的前 8 段迁移构造 v8 库，再验证迁移到 36 与旧数据可读；上一检查点遗留的“迁移后仍为 v8”断言已失效并被替换。

本轮接入了 **C++ ↔ Python sidecar 桥的首个里程碑**（`native/desktop_host/src/sidecar.cpp` + `include/yukinal/desktop_host/sidecar.hpp`）：宿主可以启动/监管 Python Agent 子进程，按其 NDJSON/JSON-RPC 协议完成 `initialize` 与 `tools.list` 握手，把 `agent.stream` 通知回传为 WebView 事件，并处理 Python 侧发出的 `host.credentials.set/get/reclaim`、`host.mcp.catalog`（当前为空目录）与 `host.tool.cancel`。在此基础上迁入了 `agent_spawn`、`agent_status`、`agent_kill`、`agent_logs` 与 Provider 六个命令（`provider_list/save/activate/delete/models/test`）。Provider API key 仍只进 OS 凭据库，由 `host.credentials.*` 读写。`desktop_host_sidecar_tests` 用真实 Python Agent 验证了启动、ping、API key 轮换/回收与 router 转发；`pnpm desktop:dev:cpp` 会自动探测 `agent/.venv` 并传入 `YUKINAL_AGENT_PYTHON`/`YUKINAL_AGENT_SOURCE`。

Rust/Tauri 宿主与 legacy Node agent 已按 `rust-removal-plan.md` 从仓库删除；C++ WebView 宿主是唯一桌面宿主，`pnpm check` 不再依赖 cargo。因此 C++ 宿主目前是**唯一载体**：尚未迁移的命令返回 `NOT_IMPLEMENTED`，前端也不再回退 Tauri。新功能实现应落在 C++ 桌面宿主或 Python Agent 中，跨层命令和数据形状以 `packages/shared` 为准。命令级缺口、依赖顺序和实现指导见 `docs/rust-removal-plan.md`。

打包已改为 C++ 宿主的分发布局与完整性清单（见下文“打包（C++ 宿主）”）；安装器（NSIS/MSI/DMG/deb）、签名与三平台真机验收仍未实现，所以分发布局还不是可发布的安装包。

## 架构与代码位置

```text
React UI ── shared IPC contract ── C++ desktop host
                                      ├── SQLite / OS credential store
                                      └── C++ SSH service
Python Agent ── LangGraph / models / tools / MCP
```

目标是让 Python Agent 通过 C++ 宿主的本机 RPC 使用已授权的目标和持久化能力。当前独立 C++ SSH 服务和 Python caller 已存在，但 C++ 桌面窗口到 Python Agent 的完整宿主桥接仍待接入。

| 目录 | 责任 |
| --- | --- |
| `apps/desktop/src/` | React 界面、路由、查询和表单；`src/lib/ipc.ts` 是唯一 UI IPC 入口 |
| `packages/shared/src/ipc/`、`packages/shared/src/schemas/` | 命令名、参数、响应和事件契约；新增命令先核对这里 |
| `native/desktop_host/src/main.cpp` | C++ WebView 窗口、受限静态文件服务和命令行入口 |
| `native/desktop_host/src/ipc_router.cpp` | C++ 命令验证、路由、事件订阅和 OS URL 打开 |
| `native/desktop_host/src/database.cpp` | SQLite 打开、迁移、服务器与其他本地记录操作 |
| `native/desktop_host/src/schema_migrations.hpp` | 按顺序执行的 36 个 SQLite 迁移 SQL；版本只能追加 |
| `native/desktop_host/src/credential_store.cpp` | Windows Credential Manager、macOS Keychain、Linux Secret Service 与 keyutils |
| `native/desktop_host/src/host_trust.cpp` | 本机 `known_hosts` v1 存储（host-key status/forget） |
| `native/src/` | 独立 C++ SSH 服务、传输和策略 |
| `agent/src/yukinal_agent/` | Python Agent Runtime、工具注册、LangGraph 和 MCP |
| `apps/desktop/src-tauri/` | **已删除**（Rust/Tauri 旧宿主） |

## C++ 宿主已有命令

当前 IPC router 已处理 `core_ping`、`shell.open_url`、`server_list`、`workspace_list`、`server_add`、`server_update`、`server_delete`、`activity_list`、`tool_execution_list`、`network_proxy_get`、`network_proxy_save`、`server_host_key_status`、`server_host_key_forget`，聊天会话的 list/get/create、消息追加、归档、重命名和删除，本机文件选择与预览 `local_path_pick`、`local_file_preview`、文件传输 `file_transfer_list`/`get`/`upload`/`download`/`cancel`/`resolve_conflict` 与远端拖拽 `file_prepare_remote_drag`/`file_drag_out_start`、主机指纹 `server_host_key_probe`、`server_host_key_trust`、日志/服务/快照读取 `server_logs`、`server_services`、`server_snapshot`、远端文件 `remote_file_list`/`read`/`preview`、终端 `terminal_open`/`write`/`resize`/`close`、连接生命周期 `server_connect`/`disconnect`/`auth_respond`/`auth_cancel`、MCP 审核与启停 `mcp_server_review`/`mcp_server_start`/`mcp_server_stop`（server 目录走 `host.mcp.catalog`，工具调用 `host.tool.execute` 分发 `mcp.*` 与九类非 MCP 只读 SSH 工具）、排查任务列表 `investigation_task_list`，以及经 sidecar 桥的 `agent_spawn`、`agent_status`、`agent_kill`、`agent_logs`、`agent_run_start`、`agent_run_stop`、`agent_approval_respond` 与 Provider 六个命令。事件订阅桥已提供通用 plumbing；未实现的命令返回 `NOT_IMPLEMENTED`，不会回退调用 Tauri。

Provider 的命令转发、凭据边界、Agent 进程监管/运行命令转发、本机文件选择/预览，以及 `host.tool.execute` 的非 MCP 只读工具分发（`server.info`/`logs`/`services`、`docker.ps`/`logs`/`inspect`、`systemd.inspect`、`package.inspect`、`filesystem.read`，带 durable plan、guardrail、目标匹配与调用幂等账本门禁，effectful 工具在此分支 fail-closed 由 Agent 经 `host.ssh.effectful_action_target` 驱动）已接通；`host.tool.execute` 的 `mcp.*` 分支现已与非 MCP 分支共用 durable plan/task guardrail/目标匹配/幂等账本门禁，MCP 风险由宿主按 running+reviewed catalog 解析（无法解析即 fail-closed）；investigation 的 UI 转发、服务器连接与主机信任（探针/确认）、终端、桌面任务恢复仍未全部迁入 C++ 桌面宿主。文件传输命令已接入 C++ 传输管理器，但平台原生拖出（`IDropSource`/`DoDragDrop`）与 OS 投放目标（`IDropTarget`/`WM_DROPFILES`）仍待实现，真实目标验证尚未完成；`host.tool.execute` 的只读 SSH 执行同样只能在真实目标上验证（`43.240.221.2:22` 目前被过滤）。独立 SSH 服务中已有的能力不等于 UI 到 C++ 宿主的命令路径已经完成。

## 构建和本机启动

仓库根目录安装前端依赖后，可用 `pnpm desktop:dev:cpp` 构建共享契约、配置并编译 C++ 宿主，再启动 Vite 和桌面窗口。该命令默认使用 `native/build-cpp-host`，通过 `YUKINAL_DESKTOP_HOST_BUILD_DIR` 可指定其他目录；Windows 需要 Visual Studio 2022 C++ 工具链、Windows SDK、CMake 和依赖下载所需的网络连接。

也可以直接构建宿主：

```powershell
cmake -S native -B native/build-cpp-host `
  -DYUKINAL_BUILD_DESKTOP_HOST=ON `
  -DYUKINAL_BUILD_SSH_SERVICE=OFF `
  -DYUKINAL_BUILD_TESTS=OFF `
  -DYUKINAL_FETCH_DEPS=ON
cmake --build native/build-cpp-host --target yukinal-desktop-host --config Debug
```

在 Windows PowerShell 中运行上述命令前，先从 Visual Studio Developer PowerShell 启动，或在 `cmd.exe` 中调用 `VsDevCmd.bat -arch=x64` 后再运行 CMake。Linux/macOS 还需要 WebKitGTK 或 WebKit、SQLite、libsecret 及对应平台的 CMake 依赖；目前未记录 C++ 桌面宿主在这两个平台的构建验收。

开发宿主接受 `--dev-url http://127.0.0.1:1420`；发布模式接受包含 `index.html` 的 `--ui-dir <目录>`。`--data-dir <绝对路径>` 可为开发实例隔离数据库。也可以设置 `YUKINAL_DATA_DIR`；默认数据位置为 Windows `%APPDATA%/dev.yukinal.workspace`、macOS `~/Library/Application Support/dev.yukinal.workspace`、Linux `$XDG_DATA_HOME/dev.yukinal.workspace`（未设置时使用 `~/.local/share/dev.yukinal.workspace`）。

## 打包（C++ 宿主）

`pnpm package`（`scripts/package.mjs`）不再走 `tauri build`，而是把仓库已经产出的四件套暂存成可分发布局：

- `pnpm build:python-agent` 产出冻结 Python Agent onedir（需要 `agent[package]` 与 PyInstaller）；
- `pnpm build:ssh-service` 产出 C++ SSH 服务；
- `pnpm --filter @yukinal/desktop build` 产出桌面 UI（`apps/desktop/dist`，其中必须有 `index.html`）；
- 用 CMake 构建 C++ 桌面宿主（`native/build-nmake` 等 Release 目录）。

四者就绪后 `pnpm package` 把它们（连同 `NOTICE`、`LICENSE`、`OPENSSL-LICENSE.txt`、`THIRD_PARTY_LICENSES.txt`）复制到 `target/release/bundle/yukinal-<version>-<platform>-<arch>/`，UI 落在 `ui/`，并写出 `SHA256SUMS.txt` 与 `release-manifest.json`。清单记录应用版本（`packages/shared/src/version.ts` 的 `APP_VERSION`）、Agent 版本（`agent/pyproject.toml`）、平台/架构、源码修订、工作区是否脏、每个文件的 SHA-256/字节数，以及启动约定（含 `launch.uiDirectory`）。`target/` 已被忽略，不纳入版本控制。UI 目录可用 `--ui-dir <目录>` 或 `YUKINAL_DESKTOP_UI_DIR` 覆盖默认的 `apps/desktop/dist`。

- `pnpm release:manifest` 在已暂存的载荷上重建清单与校验和；
- `pnpm package -- --verify` 逐字节复核载荷、校验和与清单，并反向枚举磁盘：出现未声明文件、符号链接/NTFS 联接、或清单声明了不存在的 UI 入口时都非零退出；
- 缺少宿主、UI、Agent 或 SSH 服务产物时脚本 fail-closed（UI 目录缺少 `index.html` 同样失败），不会产出部分载荷。

启动器需要用 `--ui-dir <载荷>/ui` 指向暂存的 UI（宿主要求 `--dev-url` 与 `--ui-dir` 恰好给一个，且该目录含可读 `index.html`，否则以退出码 2 静默退出），用 `YUKINAL_AGENT_ENTRY` 指向 `python-agent/yukinal-agent[.exe]`（冻结 onedir，目标机无需安装 Python），用 `YUKINAL_SSH_SERVICE_BINARY` 指向 `ssh-service/yukinal-ssh-service[.exe]`；这两个环境变量名是 C++ 宿主 `configure_sidecar()` 与终端/SFTP 后端已经在读取的名字。

**未实现**：安装器（NSIS/MSI/DMG/deb）、升级与用户数据保留、代码签名与公证、三平台真实 UI 冒烟。清单的 `notes` 会如实记录“这是分发布局而非已安装发布”；完整性校验只证明载荷未被改动，不证明发布者身份。

## 数据和凭据边界

数据库位于数据目录的 `yukinal.db`。C++ 迁移与 Rust 数据库 crate 的 36 段 SQL 完全一致，每个版本单独事务提交，并通过 `PRAGMA user_version` 记录版本。新数据库从空库初始化；较旧的非空库升级前会产生一致性备份和 JSON 清单，备份名为 `yukinal.db.pre-migration.bak` 和 `yukinal.db.pre-migration.json`。迁移失败时会关闭 SQLite 连接、恢复备份并移除 `-wal`/`-shm` 文件。更高版本的数据库会被拒绝打开。

秘密只存 OS 凭据库；SQLite 只保留 `keychain://服务/账户` 引用。修改凭据时先暂存新凭据，再提交数据库记录；失败路径须清理暂存项或把清理工作写入 `credential_cleanup_queue`。日志、IPC 响应和清单中不能包含密码、密钥或令牌。

## 下一步施工顺序

1. ✅ Provider 配置读写、模型目录、Agent 进程监管与运行转发（经 sidecar 桥），以及 `host.tool.execute` 的非 MCP 只读工具分发与 `mcp.*` durable plan/task 门禁对齐都已迁入 C++ 宿主；Agent↔host 的 RPC 块至此完成。
2. ✅ MCP 配置、风险注解信任、服务器生命周期、OAuth 与进程监督已迁入 C++ 宿主（`host.mcp.catalog` + `mcp_server_*`），并区分“设置已保存”和“服务已运行”。
3. 将服务器连接、Host Key 确认、终端、文件浏览、传输和审批流程接入 C++ 桌面宿主；复用现有 C++ SSH/SFTP/PTY 服务，不保留 Rust SSH 回退。
4. 迁移剩余任务、审计、计划、恢复与事件命令，并逐项对照 shared schema 和 UI 使用点。
5. 在分发布局（`pnpm package`）之上补齐安装器与签名，完成 Windows、macOS、Linux 桌面构建和安装后验收。

每一轮只纳入已完成且可复核的文件，更新相应文档，构建受影响的 C++ 目标，再按用户要求推送。工作区里的 `ssh_agent_development_brief.md` 是外部的任务输入说明，**不在仓库内**，也不要加入提交；仓库的构建和门禁都不依赖它的存在。Git author 和 committer 必须设为 `Bad0RANG3 <badorangeovo@Outlook.com>`。

## 完成标准

命令级盘点、依赖顺序与删除清理清单见 [`rust-removal-plan.md`](./rust-removal-plan.md)；Rust/Tauri 已按该计划删除，本文件剩下的是从零实现 C++ 宿主能力的收尾项。

- React 的现有外观和交互由 C++ WebView 宿主承载；所有 UI 命令都由 C++ 实现或明确标记为尚未迁移，不再依赖 Tauri/Rust 执行。
- 现有数据库能从新安装和旧版本安全迁移；凭据、失败恢复、审计和数据兼容有明确实现。
- Python Agent 只负责开发任务书指定的 Agent 与工具编排，通过 C++ 宿主执行本地权限和持久化操作。
- SSH、MCP、Provider、终端和文件操作在授权边界内工作；不支持的配置明确失败，不静默走旧 Rust 路径。
- 三平台 C++ 宿主和安装器完成构建、升级、用户数据保留及实际 UI 冒烟验收；Rust/Tauri 旧实现已在 `45fef20` 删除，参考实现留在 git 历史里。
