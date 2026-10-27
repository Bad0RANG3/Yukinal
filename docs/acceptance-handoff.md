# SSH + Agent 验收交接手册

> **2026-10-28 / 2.0.3 状态：** Computer Use 验证真实只读 Provider/SSH 工具循环、`server.info` 计划步骤成功结算并在桌面重开后恢复、待选择任务和计划重开恢复、未批准命令自然过期且从未执行，以及 host denylist 与桌面 autoconfig 联合工作。结构化回复组件、复制、历史回开和任务范围绑定有原生窗口/本地回归证据。Windows 宿主现嵌入任务栏图标。完整本地 `pnpm check` 通过。真实多目标并行/长时压力、安装版 GUI、签名和公证仍未完成。当前发行与证据范围以[交付与发布](./release.md)为准；当前界面以[工作区交互说明](./workspace-tabs-spec.md)为准。下文第 1 节是 2026-10-20 的历史快照，不代表本次实机证据；未单独记录的平台 GUI、拖放与升级组合仍不作已验收声明。

本文档面向**接手验收的人**，目标是在一台干净机器上复现构建与测试，并完成本机（开发机）无法完成的真实环境验收。任务输入是工作区里的 `ssh_agent_development_brief.md`；**它不在仓库内**，也**不要提交它**（仓库的构建和门禁都不依赖它的存在）。

> **变更提示（Rust/Tauri 已删除）**：`crates/`、`apps/desktop/src-tauri/`、`Cargo.*`、`rust-toolchain.toml`、`.cargo/` 和 legacy Node agent（`apps/agent/`）已从仓库移除；`pnpm check` 不再跑 cargo，桌面宿主只有 C++。本文中失效的 Rust 命令是历史信息；旧 Node `test:live` 入口已由第 6.2 节的 Python live Provider 探针替代。后续实现指导见 [`rust-removal-plan.md`](./rust-removal-plan.md)。

本文只写可复核的事实：命令、预期结果、真实结果，以及还没闭环的部分。不要因为本文说“通过”就跳过重新运行。

---

## 1. 历史验收检查点（main，2026-10-20）

| 项 | 值 |
| --- | --- |
| 分支 / 当前源码 | `main` / `a30ed3952dcc560995f265001bf403748fac8c69` |
| Provider 冷启动修复 | [PR #11](https://github.com/Bad0RANG3/Yukinal/pull/11) 已合并；Provider RPC 会先启动 Python Agent sidecar |
| 主分支跨平台 CI | [Run 39361586532](https://github.com/Bad0RANG3/Yukinal/actions/runs/39361586532)：Windows、Ubuntu、macOS 和依赖审计全部成功 |
| 候选安装包 workflow | [Run 39363920255，attempt 2](https://github.com/Bad0RANG3/Yukinal/actions/runs/39363920255)：Windows NSIS、Ubuntu DEB、macOS DMG 构建与平台安装 smoke 全部成功；首次 Windows NSIS 依赖安装遇 Chocolatey HTTP 504，重跑后通过 |
| 候选工件 | [Windows](https://github.com/Bad0RANG3/Yukinal/actions/runs/39363920255/artifacts/12240505218)（95,467,198 bytes）、[Ubuntu](https://github.com/Bad0RANG3/Yukinal/actions/runs/39363920255/artifacts/12239280971)（212,564,859 bytes）、[macOS](https://github.com/Bad0RANG3/Yukinal/actions/runs/39363920255/artifacts/12238453827)（126,038,000 bytes）；保留 30 天。它们来自 `a30ed39`，尚不是稳定版 release 资产 |
| Windows 候选包本地复核 | `package.mjs --verify` 校验 **716/716** 个随包文件；NSIS 安装版 Agent **0.1.0** provider/MCP imports 与 JSON-RPC stdio smoke 通过；安装、host self-check、卸载和用户数据保留通过。安装器 SHA-256：`BA0B94087B57F823DE83045713B284CC1C39CF100271020761A9D89CCC683EA8` |
| Windows 安装版 GUI | 2026-10-20 将上述 NSIS 安装器安装到默认 `%LOCALAPPDATA%\Programs\Yukinal`（退出码 0），通过随包 `yukinal.cmd` 启动；WebView2 Runtime 为 `155.0.4283.62`。可见窗口 `Yukinal | 动态` 正常打开，首次连接设置与空服务器列表加载，未出现配置读取错误；界面状态为“已就绪”。Alt+F4 关闭后宿主退出，NSIS 卸载退出码 0；安装目录、卸载注册项、开始菜单和桌面快捷方式均已清理。安装器 SHA-256 见上一行。 |
| D 盘隔离路径诊断 | 早先将 `WEBVIEW2_USER_DATA_FOLDER` 放在 `D:\yukinal\target\...` 时宿主退出码 3。Chromium 日志确认 profile 单实例锁创建返回 Win32 `ERROR_ACCESS_DENIED`（5）；同一候选载荷在 LocalAppData 的一次性 profile 下能启动。Windows GUI 通过结论以上述默认 LocalAppData 安装路径的真实安装、启动和卸载复验为准。 |
| Live Provider 凭据 | `YUKINAL_LIVE_API_KEY` 未配置；仅检查了 Actions secret 名称，没有读取秘密值 |
| MCP / SSH 真实目标 | 未提供可用 MCP endpoint/tool、可重置 SSH 主机、凭据 secret 或 host-key 指纹 |
| 交互式安装验收 | Windows 候选包在默认 per-user 安装路径已完成安装、首次 GUI 启动、关闭和卸载验收；仍未测试真实 Provider、MCP、SSH 工作流或完整文件传输。当前没有可用 macOS/Linux 交互安装环境。CI smoke 不代替这些手动操作 |
| 稳定版状态 | `v2.0.0` 仍公开，但其安装包没有包含 PR #11 修复；候选工件未发布。真实 A/O/F/R 业务链路、三平台安装版 GUI/文件传输、签名与公证仍待验收 |

Run 39363920255 的 attempt 1 仅 Windows job 因 Chocolatey 社区源返回 HTTP 504 而失败；attempt 2 重跑该 job 后三平台全部成功。`publish stable GitHub release` 在此 `workflow_dispatch` 运行中按设计跳过，因此此次结果只生成候选工件。

## 2. 历史任务书检查点（2026-10-18）

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

## 3. 仓库结构与职责

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

## 6. 尚需完成、需要真实凭据或交互环境的验收

### 6.1 真实 OpenSSH 容器（任务书 第 11 节/第 12 节 要求）

**远端自动验收已通过一部分**：RC.6 的 [Ubuntu 24.04 job](https://github.com/Bad0RANG3/Yukinal/actions/runs/39309839659) 配置 `YUKINAL_ENABLE_SSH_CONTAINER_TEST=ON`，CTest 的 `openssh_container_integration` 通过（13.59 秒，Ubuntu job 共 9/9 CTest 通过）。`scripts/test_ssh_container.py` 构建临时 OpenSSH 容器并实际验证服务状态、磁盘只读查询、错误凭据分类、超时、输出上限、未知/变更 host key，以及 `hardlink@openssh.com` no-replace 发布与目标冲突不覆盖。

这证明了当前 C++ SSH/SFTP 服务与隔离 OpenSSH 实现的协议兼容子集；它不替代用户指定发行版/配置的真实主机验收，也不覆盖安装版 GUI 和三平台文件拖放。

本次 RC.6 已在 Ubuntu CI 的隔离容器通过上述协议子集验收；若要在开发机复现，前置是 Docker 可用，且 `ssh-keygen`、`ssh-keyscan` 在 PATH 中。

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

要覆盖的用例：只读操作成功、错误凭据、命令超时、输出上限、未知 host key、变更 host key。RC.6 的容器结果已记入 `docs/rewrite-cpp-python.md`；开发机复现后再补充本机镜像与 OpenSSH 版本。

### 6.2 真实云 Provider

当前 Python Agent 已有显式 opt-in 的 live 工具调用验收脚本 `scripts/accept_live_python_provider.py`。它经实际 LangChain provider adapter 和 `ModelDrivenAgent` 发起一次模型请求，只暴露一个本地只读探针工具，验证模型确实调用工具并准确返回工具生成的随机 token；不写远端，也不打印 API key、完整回答或原始异常。

可在 GitHub Actions 手动运行 `.github/workflows/provider-live-acceptance.yml`：先用 repository Variables 配置 `YUKINAL_LIVE_PROVIDER`、`YUKINAL_LIVE_MODEL` 和需要的 endpoint 参数，用 repository Secret 配置 `YUKINAL_LIVE_API_KEY`，然后显式勾选 `enable_billable_provider_request`。未勾选时 Provider job 跳过；勾选后只发出一条真实工具调用请求。探针本身也要求 `YUKINAL_LIVE_PROVIDER_TESTS=1`，两道 opt-in 都保留。该 workflow 只验 Provider/tool loop，不代替真实 MCP/SSH 和安装版 GUI 任务。

PowerShell（先安装 `agent[agent,models,test]`）：

```powershell
$env:YUKINAL_LIVE_PROVIDER_TESTS = "1"
$env:YUKINAL_LIVE_PROVIDER = "openai"
$env:YUKINAL_LIVE_MODEL = "<tool-calling-model>"
$env:YUKINAL_LIVE_API_KEY = "<key>"
& agent/.venv/Scripts/python.exe scripts/accept_live_python_provider.py
```

支持 provider：`openai`、`openai-compatible`、`anthropic`、`gemini`。可选变量：
`YUKINAL_LIVE_BASE_URL`（OpenAI-compatible 必填，远程端点必须 HTTPS）、
`YUKINAL_LIVE_API_VERSION`（Anthropic）、`YUKINAL_LIVE_WIRE_API`（OpenAI-compatible 的 `chat` 或 `responses`）。远程 provider 必须配置 `YUKINAL_LIVE_API_KEY`；只有 loopback 的 OpenAI-compatible endpoint 可以不带 key。该命令会产生一次真实、可能计费的模型请求，只有显式设置 `YUKINAL_LIVE_PROVIDER_TESTS=1` 才会运行。

这条探针证明真实 provider 连通与单工具调用，不证明真实目标任务。A1–A4 仍需在安装版中配合实际 MCP endpoint 和可重置 SSH 主机完成；需记录厂商/模型/版本、认证方式、时间、测试目标与脱敏证据。鉴权失败、超时、429 和 5xx 的分类已有模拟回归，但尚无真实厂商负向验收记录。

`agent/tests/test_live_provider_acceptance.py` 会启动 loopback OpenAI-compatible stub，离线验证显式 opt-in、远端 HTTPS 限制、密钥配置边界及完整工具循环；这些测试不会访问云端。远程 endpoint 使用 API key 时必须是 HTTPS，唯一允许 HTTP 的是 loopback 测试服务。

### 6.3 明确未实现/不属任务书

- **调查证据/产物记录**（`host.investigation.evidence.record`、`host.investigation.artifact.record`）在 Python 运行时仍未实现。计划步骤结算 `host.investigation.plan.step_result` 已接入 Python Agent：成功工具结果会调用宿主推进持久计划，宿主按 call ID 防止重放重复推进；Python Agent 与 C++ 宿主处理器都有确定性回归。安装版 UI 的实时闭环仍需单独验收。
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

## 9. 当前门禁与复现清单

```powershell
# 首次安装 workspace 与 Python 测试依赖
pnpm install --frozen-lockfile
python -m venv agent/.venv
agent/.venv/Scripts/python.exe -m pip install -e "agent[agent,models,test]"
python -m venv services/control-plane/.venv
services/control-plane/.venv/Scripts/python.exe -m pip install -e "services/control-plane[test]"

# 完整当前仓库门禁；需先按 native/README.md 配置 C++ 测试构建
$env:YUKINAL_SSH_CMAKE_BUILD_DIR = "native/build-check"
$env:YUKINAL_CONTROL_PLANE_REQUIRED = "1"
pnpm check

# 离线验证 live Provider 探针（只访问本机 loopback stub）
agent/.venv/Scripts/python.exe -m unittest agent.tests.test_live_provider_acceptance -v

# 真实 OpenSSH 隔离容器（需 Docker；RC.6 Ubuntu CI 已通过）
python scripts/test_ssh_container.py --service-binary native/build-check/yukinal-ssh-service.exe
```

macOS/Linux 使用 `agent/.venv/bin/python` 和 `services/control-plane/.venv/bin/python`；容器测试的 service 路径按构建结果填写。release-candidate workflow 在 Windows、Ubuntu 24.04 与 macOS 15 Intel 上运行完整 gate 并构建、安装 smoke NSIS/DEB/DMG。

真实 Provider 验收会发出一次可能计费的请求，只在明确配置并启用时运行：

```powershell
$env:YUKINAL_LIVE_PROVIDER_TESTS = "1"
$env:YUKINAL_LIVE_PROVIDER = "openai" # openai-compatible / anthropic / gemini
$env:YUKINAL_LIVE_MODEL = "<tool-calling-model>"
$env:YUKINAL_LIVE_API_KEY = "<key>"
$env:YUKINAL_LIVE_BASE_URL = "<optional-https-endpoint>"
& agent/.venv/Scripts/python.exe scripts/accept_live_python_provider.py
```

---

## 10. 并行工作线：C++ 桌面宿主（不属本任务书）

状态：C++ 宿主已能打开 React UI、提供本机 IPC、初始化并迁移 SQLite（1→36）、服务器 CRUD、OS 凭据、活动/聊天记录、网络代理设置（`network_proxy_get`/`network_proxy_save`，凭据只进 OS 库，PAC/内嵌凭据失败关闭），以及终端、文件传输与拖拽、MCP（含 OAuth 与进程监督）和 Agent↔host 的 `host.*` RPC（含 `host.tool.execute` 两条路由）。CTest 目标依平台与配置而异：Windows 当前注册 13 个；Ubuntu RC.6 job 注册 9 个，其中包含真实 OpenSSH 容器集成。

未迁入 C++ 的命令返回 `NOT_IMPLEMENTED`，不会回退 Tauri（Tauri 宿主已删除）。剩余迁移顺序见 `docs/cpp-desktop-handoff.md`。

桌面宿主开发启动：`pnpm desktop:dev:cpp`（Tauri + Python Agent 的 `desktop:dev:python` 已随 Rust/Tauri 删除而移除）。

---

## 11. 接手后的建议顺序

1. 按第 9 节复现当前 `pnpm check`、Python Agent 与 CTest 结果；Rust/Tauri 目标已删除，不属于当前门禁。
2. 跑 第 6.1 节 真实 OpenSSH，补齐真实协议兼容性证据。
3. 跑第 6.2 节真实 Python Agent Provider 探针，并记录实际厂商、模型和脱敏结果。
4. 把结果写回 `docs/rewrite-cpp-python.md`（带日期、版本、通过/失败组合），再更新 `docs/changelog.md`。
5. 按 A1–A4、O1–O2、F1–F4、R1 完成真实目标任务与 Windows/macOS/Linux 安装版 GUI 验收，再决定稳定版发布。
6. Git 身份使用 `Bad0RANG3 <badorangeovo@Outlook.com>`，且**不要**提交 `ssh_agent_development_brief.md`。
