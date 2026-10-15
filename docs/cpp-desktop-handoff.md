# C++ 桌面宿主交接手册

本文用于接手 Yukinal 的桌面宿主重写。当前方向是保留 React UI 的外观与交互，把桌面窗口、IPC、SQLite、凭据管理和桌面能力逐步迁到 C++；不要再为桌面功能补 Rust 实现。Python Agent 仍按开发任务书负责 LangGraph、模型和工具编排，之后由 C++ 桌面宿主承接它需要的本地授权与数据能力。

## 当前检查点

截至 2026-10-15，上一检查点为 `d2594ff`（`Move schema migrations to C++ desktop host`）。当前 C++ 桌面宿主可打开 React UI、提供本机 IPC 桥、初始化 SQLite 数据库并执行版本 1 到 36 的迁移，已实现服务器增删改、OS 凭据库读写、活动记录、聊天历史命令，以及本轮迁入的网络代理设置命令（`network_proxy_get`、`network_proxy_save`）。网络代理的入参校验、入参中写入凭据的擦除、凭据轮换/保留/清除与失败回收都落在 C++；解析结果区分环境变量、Windows/macOS 系统代理与 PAC 失败关闭。

C++ 桌面宿主核心、Windows 可执行文件及桌面宿主测试（IPC router、数据库迁移、self-check）已在本机构建并通过。数据库测试已改为用真实的前 8 段迁移构造 v8 库，再验证迁移到 36 与旧数据可读；上一检查点遗留的“迁移后仍为 v8”断言已失效并被替换。

Rust/Tauri 宿主与 legacy Node agent 已按 `rust-removal-plan.md` 从仓库删除；C++ WebView 宿主是唯一桌面宿主，`pnpm check` 不再依赖 cargo。因此 C++ 宿主目前是**唯一载体**：尚未迁移的命令返回 `NOT_IMPLEMENTED`，前端也不再回退 Tauri。新功能实现应落在 C++ 桌面宿主或 Python Agent 中，跨层命令和数据形状以 `packages/shared` 为准。命令级缺口、依赖顺序和实现指导见 `docs/rust-removal-plan.md`。

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

当前 IPC router 已处理 `core_ping`、`shell.open_url`、`server_list`、`workspace_list`、`server_add`、`server_update`、`server_delete`、`activity_list`、`tool_execution_list`、`network_proxy_get`、`network_proxy_save`、`server_host_key_status`、`server_host_key_forget`，以及聊天会话的 list/get/create、消息追加、归档、重命名和删除。事件订阅桥已提供通用 plumbing；未实现的命令返回 `NOT_IMPLEMENTED`，不会回退调用 Tauri。

Provider、MCP、服务器连接与主机信任（探针/确认）、终端、文件浏览/传输、桌面任务恢复和 Python Agent 桥接还没有全部迁入 C++ 桌面宿主。独立 SSH 服务中已有的能力不等于 UI 到 C++ 宿主的命令路径已经完成。

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

## 数据和凭据边界

数据库位于数据目录的 `yukinal.db`。C++ 迁移与 Rust 数据库 crate 的 36 段 SQL 完全一致，每个版本单独事务提交，并通过 `PRAGMA user_version` 记录版本。新数据库从空库初始化；较旧的非空库升级前会产生一致性备份和 JSON 清单，备份名为 `yukinal.db.pre-migration.bak` 和 `yukinal.db.pre-migration.json`。迁移失败时会关闭 SQLite 连接、恢复备份并移除 `-wal`/`-shm` 文件。更高版本的数据库会被拒绝打开。

秘密只存 OS 凭据库；SQLite 只保留 `keychain://服务/账户` 引用。修改凭据时先暂存新凭据，再提交数据库记录；失败路径须清理暂存项或把清理工作写入 `credential_cleanup_queue`。日志、IPC 响应和清单中不能包含密码、密钥或令牌。

## 下一步施工顺序

1. 迁移 Provider 配置读写与模型目录，保证 API key 轮换、删除和失败清理遵循同一套凭据约束。
2. 迁移 MCP 配置、风险注解信任、服务器生命周期、OAuth 与进程监督；区分“设置已保存”和“服务已运行”。
3. 将服务器连接、Host Key 确认、终端、文件浏览、传输和审批流程接入 C++ 桌面宿主；复用现有 C++ SSH/SFTP/PTY 服务，不保留 Rust SSH 回退。
4. 将 Python Agent 连接到 C++ 宿主 RPC，迁移剩余任务、审计、计划、恢复与事件命令，并逐项对照 shared schema 和 UI 使用点。
5. 完成 Windows、macOS、Linux 桌面构建和安装后验收，再移除 Tauri/Rust 桌面宿主及只服务于旧宿主的代码。

每一轮只纳入已完成且可复核的文件，更新相应文档，构建受影响的 C++ 目标，再按用户要求推送。不要把 `ssh_agent_development_brief.md` 加入提交；它是工作区中的任务输入。Git author 和 committer 必须设为 `Bad0RANG3 <badorangeovo@Outlook.com>`。

## 完成标准

命令级盘点、依赖顺序与删除清理清单见 [`rust-removal-plan.md`](./rust-removal-plan.md)；在它的完成标准全部勾上之前不要删除 Rust/Tauri。

- React 的现有外观和交互由 C++ WebView 宿主承载；所有 UI 命令都由 C++ 实现或明确标记为尚未迁移，不再依赖 Tauri/Rust 执行。
- 现有数据库能从新安装和旧版本安全迁移；凭据、失败恢复、审计和数据兼容有明确实现。
- Python Agent 只负责开发任务书指定的 Agent 与工具编排，通过 C++ 宿主执行本地权限和持久化操作。
- SSH、MCP、Provider、终端和文件操作在授权边界内工作；不支持的配置明确失败，不静默走旧 Rust 路径。
- 三平台 C++ 宿主和安装器完成构建、升级、用户数据保留及实际 UI 冒烟验收后，才清理 Rust/Tauri 旧实现。
