# SSH + Agent 验收交接手册

本文档面向**接手验收的人**，目标是在一台干净机器上复现构建与测试，并完成本机（开发机）无法完成的真实环境验收。任务输入是工作区里的 `ssh_agent_development_brief.md`；**它不在仓库内**，也**不要提交它**（仓库的构建和门禁都不依赖它的存在）。

> **变更提示（Rust/Tauri 已删除）**：`crates/`、`apps/desktop/src-tauri/`、`Cargo.*`、`rust-toolchain.toml`、`.cargo/` 和 legacy Node agent（`apps/agent/`）已从仓库移除；`pnpm check` 不再跑 cargo，桌面宿主只有 C++。本文中失效的命令（`cargo test`/`pnpm --filter @yukinal/agent test:live`/`yukinal-desktop --lib`）已在下文标注；后续实现指导见 [`rust-removal-plan.md`](./rust-removal-plan.md)。

本文只写可复核的事实：命令、预期结果、真实结果，以及还没闭环的部分。不要因为本文说“通过”就跳过重新运行。

---

## 1. 当前检查点

| 项 | 值 |
| --- | --- |
| 分支 | `codex/ui-layout-direction` |
| HEAD | `736f06a`（`chore(ci): drop the temporary native crash diagnostics`） |
| 本地领先 `origin/main` | 30 个提交（**未推送**） |
| 基线检查点 | `d2594ff`（`Move schema migrations to C++ desktop host`） |
| 未跟踪文件 | `ssh_agent_development_brief.md`（工作区任务输入，不在仓库内，禁止提交） |

这轮的验收对象是任务书 `ssh_agent_development_brief.md` 的四阶段。仓库同时并行进行 **C++ 桌面宿主迁移**（见 `docs/cpp-desktop-handoff.md`、`docs/native-desktop-host.md`），它是另一条工作线，不属本任务书的验收范围，但改动会影响仓库整体门禁，本文一并说明。

### 本机真实结果（Windows 11 / VS 2022 / Python 3.14.8）

| 套件 | 结果 |
| --- | --- |
| CTest（`native/build-nmake`，Release） | **10/10 passed** |
| Python Agent 全量 unittest | **132 passed** |
| Rust workspace `cargo test`（除 `yukinal-desktop --lib`） | **all passed**，含真实 sidecar + 回环 Provider 端到端 |
| `cargo fmt --all --check` | passed |
| `cargo clippy --workspace --all-targets -- -D warnings` | passed |
| 真实 OpenSSH 容器验收 | **未跑**（本机无 Docker） |
| 真实云 Provider live | **未跑**（本机无 API key；Python 侧也没有 live harness） |
| `yukinal-desktop --lib` 单元测试 | **无法启动**（`STATUS_ENTRYPOINT_NOT_FOUND`，环境问题，见 第 7 节） |

> 2026-10-18 复核：上表是那一轮的记录。当前实测 CTest 为 **13/13**（`ctest --test-dir native/build-nmake -C Release --output-on-failure`），Python Agent 为 **153 passed / 4 skipped**；Rust 相关行随宿主删除已无对应目标。
> 2026-10-18 二次复核：CTest 由 15 降为 **13**，原因是删除了 `test_client.py` / `test_service_integration.py` 的**重复注册**（详见 第 4.1 节的修正说明），覆盖不降反升 —— 原 15 个目标里有 2 个是重复计数，同时有 4 个测试模块不在任何目标里。

---

## 2. 仓库结构与职责

```text
native/                    C++：独立 SSH service + C++ 桌面宿主（WebView）
  src/                     C++ SSH service（libssh2 + cpp-httplib）
  desktop_host/            C++ 桌面宿主（窗口、IPC、SQLite、凭据、迁移）
agent/                     Python：Agent Runtime（LangGraph/MCP/工具/策略/provider）
apps/agent/                旧 Node Agent（已删除）
apps/desktop/src/          React UI
apps/desktop/src-tauri/    Tauri/Rust 旧宿主（已删除）
crates/                    Rust：core / database / net / credentials / host-trust 等（已删除）
packages/shared/           UI 与宿主的 IPC/schema 契约
docs/                      设计与状态文档
```

任务书关注的是 `native/src`（C++ SSH）、`agent/`（Python Agent）、`packages/shared`（契约）。

---

## 3. 环境准备

### 3.1 Windows 必备

- Visual Studio 2022（Community 或 BuildTools）+ “使用 C++ 的桌面开发” + Windows SDK
- CMake ≥ 3.24
- Python ≥ 3.11（本机 3.14.8）
- Node ≥ 24、pnpm 11.8
- Git
- Rust stable（rustfmt、clippy）。**注意**：本机 `.cargo/bin` 的 shim 缺失，`cargo` 不在 `PATH`，但工具链在
  `C:\Users\<user>\.rustup\toolchains\stable-x86_64-pc-windows-msvc\bin`，可直接把该目录加进 `PATH`。
- Docker Desktop（仅真实 OpenSSH 容器验收需要）

### 3.2 Python 虚拟环境

```powershell
python -m venv agent/.venv
agent/.venv/Scripts/python.exe -m pip install -e "agent[agent,models,test]"
```

`agent`/`models`/`test` 三个 extra 都装齐，CTest 才会注册 `python_agent_tests`、`ssh_mock_integration`、`agent_ssh_mock_integration`。若缺 `jsonschema/langgraph/mcp`，这些测试会被 CMake **静默跳过**，门禁会假装通过——验收时必须确认测试数量。

---

## 4. 复现本机结果

### 4.1 配置并构建 C++（含 SSH service + 桌面宿主 + 测试）

在 **VS Developer PowerShell / vcvars64** 里执行（NMake 需要 MSVC 环境）：

```powershell
cmake -S native -B native/build-nmake -G "NMake Makefiles" `
  -DCMAKE_BUILD_TYPE=Release `
  -DYUKINAL_FETCH_DEPS=ON `
  -DYUKINAL_BUILD_TESTS=ON `
  -DYUKINAL_BUILD_SSH_SERVICE=ON `
  -DYUKINAL_BUILD_DESKTOP_HOST=ON `
  -DYUKINAL_BUNDLE_WINDOWS_OPENSSL=ON `
  -DYUKINAL_ENABLE_SSH_MOCK_TEST=ON `
  -DPython3_EXECUTABLE=<repo>/agent/.venv/Scripts/python.exe
cmake --build native/build-nmake --config Release --parallel
ctest --test-dir native/build-nmake -C Release --output-on-failure
```

预期 13 个测试全过（下面这份清单与 `ctest -N` 的实际顺序一致，即 `native/CMakeLists.txt` 的注册顺序）：

1. `yukinal_ssh_tests`（C++ 配置/策略/host key/网络）
2. `ssh_container_harness_tests`（容器 harness 自身的解析测试）
3. `python_agent_tests`（**Python Agent 全量**：`agent/tests/test_*.py`，153 tests）
4. `ssh_mock_integration`（Paramiko mock：成功/鉴权失败/超时/输出上限/host key）
5. `agent_ssh_mock_integration`（Agent→C++ mock：选中目标绑定、越权拒绝）
6. `window_context_tests`
7. `caption_hit_test_tests`
8. `desktop_host_tests`
9. `transfer_tests`
10. `transfer_manager_tests`
11. `desktop_host_database_tests`
12. `desktop_host_self_check`
13. `desktop_host_sidecar_tests`

> 修正（2026-10-18）：此清单原先写「10 个测试」，并把 `python_client_tests` /
> `service_http_integration` 列为独立目标。这两个目标是 `test_client.py` /
> `test_service_integration.py` 的重复注册 —— 它们与 `test_agent_*.py` glob 一起造成了
> 「同一批测试被计两次、而 `test_network_snapshot.py` / `test_provider_store.py` /
> `test_reasoning_effort.py` / `test_system_snapshot.py` 四个模块不被任何门禁执行」。
> 现在 `native/CMakeLists.txt` 只保留一个 glob 为 `test_*.py` 的 `python_agent_tests`
> 目标来覆盖全部 Agent 测试（`test_client.py` 与 `test_service_integration.py` 在内），
> 并给该目标注入了 `YUKINAL_SSH_SERVICE_BINARY`，使 `test_service_integration` 的 4 个
> 用例真正执行而不再静默 skip。

> 若第 3–5 个没出现，说明 Python extra 没装全，不要当作通过。

### 4.2 Python Agent 全量测试

```bash
export PYTHONPATH=agent/src
export YUKINAL_SSH_SERVICE_BINARY="D:/yukinal/native/build-nmake/yukinal-ssh-service.exe"
agent/.venv/Scripts/python.exe -m unittest discover -s agent/tests -p "test_*.py"
```

预期：`Ran 153 tests ... OK (skipped=4)`（2026-10-18 实测；此前的 132 是旧快照）。
`skipped=4` 是本机缺少 Docker/真实云 Provider 时才出现的用例；`test_service_integration`
的 4 个用例在设好 `YUKINAL_SSH_SERVICE_BINARY` 后会真正执行，不再是 skip。

### 4.3 Rust 工作区

```bash
export PATH="/c/Users/<user>/.rustup/toolchains/stable-x86_64-pc-windows-msvc/bin:$PATH"
export YUKINAL_TEST_PYTHON="D:/yukinal/agent/.venv/Scripts/python.exe"
export YUKINAL_TEST_AGENT_SOURCE="D:/yukinal/agent/src"
export YUKINAL_TEST_NODE="C:/Program Files/nodejs/node.exe"
export YUKINAL_TEST_REQUIRED=1

cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace -- --test-threads=1
```

- `YUKINAL_TEST_REQUIRED=1` 很关键：没有它，缺少 Python/Node 的测试会静默 skip，绿色不代表真跑过。
- 唯一预期失败是 `yukinal-desktop --lib` 的**启动**失败（不是断言失败），见 第 7 节。
- 任务书最关键的端到端测试是：
  `crates/core/tests/sidecar_agent.rs::real_sidecar_runs_a_durable_read_through_provider_and_host`
  —— 真 Python sidecar + 回环 OpenAI 兼容 Provider + 真 host RPC + LangGraph 持久计划 + host-backed 只读工具。可单独跑：

```bash
cargo test -p yukinal-core --test sidecar_agent -- --test-threads=1
```

---

## 5. 任务书验收矩阵

| 任务书要求 | 实现位置 | 验收证据 | 状态 |
| --- | --- | --- | --- |
| C++ SSH service，仅监听 127.0.0.1，host key 必须验证 | `native/src` | `yukinal_ssh_tests`、`ssh_mock_integration` | 通过 |
| Python 通过本机 HTTP/JSON 调用 C++ | `agent/src/yukinal_ssh` | `python_agent_tests` 内的 `test_client`、`test_service_integration` | 通过 |
| 结构化只读操作（`service_status`/`disk_usage` 等）、白名单目标 | `native/src/policy.cpp`、`agent/src/yukinal_ssh` | `test_client`、`test_policy` | 通过 |
| 参数校验 / 超时 / 输出上限 / 错误分类 / 脱敏 | 同上 | `test_config`、`test_policy`、mock | 通过 |
| Tool Registry：全局唯一、schema 校验、目标授权、审批绑定、审计脱敏 | `agent/src/yukinal_agent/tools.py`、`audit.py` | `test_agent_tools` | 通过 |
| MCP stdio + Streamable HTTP、断连、重复名、schema 错误 | `agent/src/yukinal_agent/mcp_manager.py` | `test_agent_workflow`（含真实 MCP 进程） | 通过 |
| LangGraph 工作流、步数/时限/预算、Checkpointer 恢复 | `agent/src/yukinal_agent/model_agent.py`、`checkpoints.py` | `test_agent_workflow`、`test_agent_model_loop` | 通过 |
| ModelFactory 多 provider 配置切换 | `agent/src/yukinal_agent/models.py`、`provider_store.py` | `test_agent_models_policy`、`test_provider_store` | 通过 |
| provider 鉴权失败/超时/限流分类 | `model_agent.py::_model_error_code` | `test_agent_model_loop` | 通过 |
| Provider 端到端 | Python Agent + 回环 OpenAI 兼容 | `sidecar_agent::real_sidecar_runs_a_durable_read_through_provider_and_host` | 通过（回环；真实云见 第 6.2 节） |
| 策略引擎、高风险拦截、审批参数变化重审 | `agent/src/yukinal_agent/policy.py`、host 审批 | `test_agent_models_policy`、`test_agent_sidecar`、`test_agent_model_loop` | 通过 |
| 工具名全局唯一/冲突检测 | `tools.py` + provider 边界映射（`model_agent.py`） | `test_agent_tools`、`test_agent_model_loop` | 通过 |
| 任务取消、重复请求、进程重启 | `sidecar.py`、`supervisor` | `test_agent_sidecar`、Rust `supervisor` 测试 | 通过 |
| 完整调查任务/计划恢复（证据/产物持久化） | — | — | **未实现**（属产品 A/O/F/R 线，不在任务书内；见 第 6.3 节） |

---

## 6. 本机无法完成、需要接手者执行的验收

### 6.1 真实 OpenSSH 容器（任务书 第 11 节/第 12 节 要求）

前置：Docker Desktop 正在运行，`ssh-keygen`、`ssh-keyscan` 可用。

方式 A（推荐，隔离临时容器；测试结束后自动清理镜像/容器）：

```bash
export PYTHONPATH=agent/src
python scripts/test_ssh_container.py --service-binary native/build-nmake/yukinal-ssh-service.exe
```

方式 B（注册进 CTest）：

```powershell
cmake -S native -B native/build-check -G "NMake Makefiles" `
  -DCMAKE_BUILD_TYPE=Release -DYUKINAL_FETCH_DEPS=ON -DYUKINAL_BUILD_TESTS=ON `
  -DYUKINAL_ENABLE_SSH_CONTAINER_TEST=ON `
  -DPython3_EXECUTABLE=<repo>/agent/.venv/Scripts/python.exe
cmake --build native/build-check --config Release --parallel
ctest --test-dir native/build-check -C Release -R openssh_container_integration --output-on-failure
```

要覆盖的用例：只读操作成功、错误凭据、命令超时、输出上限、未知 host key、变更 host key。请把容器镜像、OpenSSH 版本、通过/失败的输入组合记进 `docs/rewrite-cpp-python.md`。

### 6.2 真实云 Provider

现状：
- Python Agent 的 provider 路径已在**回环** OpenAI 兼容服务上端到端验证（第 4.3 节）。这验证了 wire、工具调用、LangGraph、持久计划，但不证明某家真实云厂商。
- 仓库里现成的 live 门禁是**旧 Node provider SDK** 的：
  `apps/agent/live/provider-live.test.ts`。它测的不是 Python LangChain 路径。

方式 A：验证旧 Node provider 适配器（可与真实 OpenAI/Anthropic/Gemini 通话）：

```bash
YUKINAL_LIVE_PROVIDER_TESTS=1 \
YUKINAL_LIVE_PROVIDERS=openai-chat \
YUKINAL_OPENAI_MODEL=gpt-4o-mini \
YUKINAL_OPENAI_API_KEY=sk-... \
pnpm --filter @yukinal/agent test:live
```

可选 provider 名：`openai-chat`、`openai-responses`、`anthropic`、`gemini`；对应变量：
`YUKINAL_OPENAI_MODEL` + `YUKINAL_OPENAI_API_KEY`（`YUKINAL_OPENAI_BASE_URL` 可选）、
`ANTHROPIC_API_KEY` + `YUKINAL_ANTHROPIC_MODEL`、
`GEMINI_API_KEY` + `YUKINAL_GEMINI_MODEL`。

方式 B：验证 Python Agent 到真实云。目前**没有现成 live 脚本**，需要二选一：
- 起桌面应用并在设置里配置真实 provider（`pnpm desktop:dev:python`），手动发一条只读任务；
- 或照 `crates/core/tests/sidecar_agent.rs` 的回环测试，新增一个用真实 `baseUrl`/key 的 gated 测试。

要覆盖：鉴权失败（401/403）、超时、限流（429）、模型不可用（5xx）、工具调用能力不足时的清晰报错。把厂商、模型、时间、输入组合记入文档。

### 6.3 明确未实现/不属任务书

- **调查任务的证据/产物持久化**（`host.investigation.evidence.record`、`plan.step_result`、`artifact.record`）在 Python 运行时没有实现；旧 Node Agent 有。`crates/core/tests/sidecar_agent.rs` 已**不再**把它计为通过，只在 host fixture 里显式拒绝该调用。
- **第四阶段控制平面**（FastAPI + PostgreSQL + 多租户）：任务书写“可选、按实际需要”，且警告不要过早引入微服务。当前是单机桌面产品，未实现。

---

## 7. 已知环境问题与坑

1. **`yukinal-desktop --lib` 测试二进制无法启动**：`exit code 0xc0000139 (STATUS_ENTRYPOINT_NOT_FOUND)`。这是 Windows 下测试二进制缺少嵌入 manifest、加载到 comctl32 v5 的问题，与本次任务书无关，改动前就存在。表现是 `cargo test --workspace` 在 “Running unittests src\lib.rs (yukinal_desktop_lib-*.exe)” 处报启动失败（不是断言失败）。其余 Rust 目标全部通过。
2. **`cargo` 不在 PATH**：`.cargo/bin` 缺失；用 `~/.rustup/toolchains/stable-x86_64-pc-windows-msvc/bin`。
3. **NMake 需要 MSVC 环境**：不先 `vcvars64.bat` 会报找不到 `nmake`/`cl`。
4. **CMake 依赖下载**：首次 configure 会拉取 libssh2 / nlohmann_json / cpp-httplib / webview / sqlite / bundled OpenSSL，需要网络；Windows 上 bundled OpenSSL 还需要完整 Perl（Strawberry Perl）。
5. **测试静默跳过**：缺 Python extra 或 Paramiko 时 CMake 会不注册测试。核对 `ctest -N` 数量与预期一致（2026-10-18 实测 **13 个**；见 第 4.1 节的修正说明）。
6. **`ssh_agent_development_brief.md`** 是工作区里的任务输入，不在仓库内；保持未跟踪、不提交。
7. **Git 身份**：提交 author/committer 必须是 `Bad0RANG3 <badorangeovo@Outlook.com>`。

---

## 8. 安全与数据边界（验收时不要破坏）

- 秘密只进 OS 凭据库（Windows Credential Manager / macOS Keychain / Linux Secret Service + keyutils）；SQLite 只存 `keychain://服务/账户` 引用。
- IPC 响应、日志、审计、迁移清单中不得出现密码、私钥、令牌或完整敏感环境变量。
- 默认拒绝：未配置的目标/用户/工具/操作一律拒绝。
- host key 必须校验；未知或变更必须失败。
- 破坏性操作必须人工审批；审批绑定具体目标、工具与参数，参数变化重审。
- 目标必须由 `target_id` 映射到受控配置，不能相信模型给出的任意地址。
- MCP/模型返回的文本视为不可信数据，不能覆盖系统策略。

---

## 9. 一键复现清单（Windows Developer PowerShell）

```powershell
# 1. Python venv（首次）
python -m venv agent/.venv
agent/.venv/Scripts/python.exe -m pip install -e "agent[agent,models,test]"

# 2. C++ + CTest
cmake -S native -B native/build-nmake -G "NMake Makefiles" `
  -DCMAKE_BUILD_TYPE=Release -DYUKINAL_FETCH_DEPS=ON -DYUKINAL_BUILD_TESTS=ON `
  -DYUKINAL_BUILD_SSH_SERVICE=ON -DYUKINAL_BUILD_DESKTOP_HOST=ON `
  -DYUKINAL_BUNDLE_WINDOWS_OPENSSL=ON -DYUKINAL_ENABLE_SSH_MOCK_TEST=ON `
  -DPython3_EXECUTABLE="$PWD/agent/.venv/Scripts/python.exe"
cmake --build native/build-nmake --config Release --parallel
ctest --test-dir native/build-nmake -C Release --output-on-failure      # 期望 15/15

# 3. Python Agent 全量
$env:PYTHONPATH = "$PWD/agent/src"
$env:YUKINAL_SSH_SERVICE_BINARY = "$PWD/native/build-nmake/yukinal-ssh-service.exe"
& "$PWD/agent/.venv/Scripts/python.exe" -m unittest discover -s agent/tests -p "test_*.py"   # 期望 153 OK（4 skipped）

# 4. Rust（cargo 不在 PATH 时）
$env:PATH = "$env:USERPROFILE\.rustup\toolchains\stable-x86_64-pc-windows-msvc\bin;$env:PATH"
$env:YUKINAL_TEST_PYTHON = "$PWD/agent/.venv/Scripts/python.exe"
$env:YUKINAL_TEST_AGENT_SOURCE = "$PWD/agent/src"
$env:YUKINAL_TEST_NODE = "C:/Program Files/nodejs/node.exe"
$env:YUKINAL_TEST_REQUIRED = "1"
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace -- --test-threads=1

# 5. 真实 OpenSSH（需 Docker）
python scripts/test_ssh_container.py --service-binary native/build-nmake/yukinal-ssh-service.exe

# 6. 真实 Provider（旧 Node 门禁；Python 路径见 第 6.2 节）
$env:YUKINAL_LIVE_PROVIDER_TESTS = "1"
$env:YUKINAL_LIVE_PROVIDERS = "openai-chat"
$env:YUKINAL_OPENAI_MODEL = "<model>"
$env:YUKINAL_OPENAI_API_KEY = "<key>"
pnpm --filter @yukinal/agent test:live
```

---

## 10. 并行工作线：C++ 桌面宿主（不属本任务书）

状态：C++ 宿主已能打开 React UI、提供本机 IPC、初始化并迁移 SQLite（1→36）、服务器 CRUD、OS 凭据、活动/聊天记录、网络代理设置（`network_proxy_get`/`network_proxy_save`，凭据只进 OS 库，PAC/内嵌凭据失败关闭），以及终端、文件传输与拖拽、MCP（含 OAuth 与进程监督）和 Agent↔host 的 `host.*` RPC（含 `host.tool.execute` 两条路由）。`ctest` 当前有 15 个目标。

未迁入 C++ 的命令返回 `NOT_IMPLEMENTED`，不会回退 Tauri（Tauri 宿主已删除）。剩余迁移顺序见 `docs/cpp-desktop-handoff.md`。

桌面宿主开发启动：`pnpm desktop:dev:cpp`（Tauri + Python Agent 的 `desktop:dev:python` 已随 Rust/Tauri 删除而移除）。

---

## 11. 接手后的建议顺序

1. 按 第 9 节 复现本机结果，确认 CTest 全部通过（2026-10-18 实测 **13 个目标**）、Python Agent 测试全绿；Rust 目标已随宿主删除而移除。
2. 跑 第 6.1 节 真实 OpenSSH，补齐真实协议兼容性证据。
3. 跑 第 6.2 节 真实 Provider（明确区分 Node SDK 与 Python LangChain 两条路径）。
4. 把结果写回 `docs/rewrite-cpp-python.md`（带日期、版本、通过/失败组合），再更新 `docs/changelog.md`。
5. 第 7.1 节的 `yukinal-desktop --lib` launch 问题已随 Rust 宿主删除而消失，无需处理。
6. 若要推送：确认只提交已完成且可复核的文件，Git 身份用 `Bad0RANG3 <badorangeovo@Outlook.com>`，且**不要**提交 `ssh_agent_development_brief.md`。
