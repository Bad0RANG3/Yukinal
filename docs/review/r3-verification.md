# R3 — 对抗式证伪报告（对 R1 全部 Critical / High 发现的独立复核）

- **复核范围**：`docs/review/r1-native.md`、`docs/review/r1-desktop.md`、`docs/review/r1-python.md` 中的全部 11 条 Critical / High 判定（native H-1/H-2、desktop C-1/C-2/H-1/H-2/H-3/H-4、python H-1/H-2/H-3）。
- **复核基线**：`HEAD = 736f06a58d07e595fa79ac620d8ce116779bfe4c`（分支 `codex/ui-layout-direction`），另对**当前工作区**做对照，以区分「原报告所述代码状态」与「当前已修复状态」。
- **复核立场**：R1 的三份报告是**待检验的断言**，不是事实。每一条都先假定为假，直到我亲自读到 `文件:行` 证据、并用**自己写的命令**重算全部数值后才接受。
- **环境**：Windows / pwsh；`cmake`/`ctest`/`node`/`pnpm`/`git` 可用；已构建树 `native/build-nmake`（含 `yukinal-ssh-service.exe`，5 656 576 字节）；`agent/.venv`、`services/control-plane/.venv` 均存在。
- **只读约束**：本次复核**未修改任何被跟踪文件**。`git status --porcelain native/src native/desktop_host packages/shared/src agent/src apps/desktop/src` 输出为空。所有临时脚本写在 `%TEMP%`。

---

## 一、判定汇总表

| 条目 | 判定 | 一句话依据 |
| --- | --- | --- |
| native **H-1** `exit_code` 无法区分「0」与「未收到」 | **部分成立** | 主体（假成功）我端到端复现：mock 服务器不发 `exit-status` ⇒ 服务返回 `status=succeeded, exit_code=0`；但 R1「守卫是死代码」的措辞**不准确**，我用 `exit-status=0x80000000` 触发了该守卫（`status=failed/result_unknown`），它可达但语义错误。 |
| native **H-2** OpenSSL TLS 写路径未屏蔽 SIGPIPE | **成立**（触发面**无法判定**） | 静态链完整可查（OpenSSL `writesocket`→`write`、`crypto/` 内 `MSG_NOSIGNAL` 计数为 0、native 全树无 `signal`/`sigaction`）；但本机无 WSL 发行版与 docker，**未能**在 Linux 上实际触发 SIGPIPE 终止。 |
| desktop **C-1** keyboard-interactive 链路端到端不可达 | **成立** | 生产者确实存在并被我跑通（`ssh_mock_integration` 通过，驱动真实 `yukinal-ssh-service.exe` 走完 `auth_prompt`）；而 `native/desktop_host` 内 `auth_prompt`/`auth_response` 字面量计数为 **0**，`server_auth_respond` 无任何分支可返回 `true`。 |
| desktop **C-2** 6 个 A 类死通道 + 3 个 B 类声明名 | **成立** | 我对 9 个名字在 `native/`+`agent/` 逐字面量重扫，全部为 0；对 13 个「有生产者」通道复扫命中位置与 R1 表一致；`terminal.opened` 确为第三类状态。 |
| desktop **H-1** `agent_status.restart` 无生产者、无 supervisor | **成立** | `SidecarStatus` 8 个字段中没有 `restart`；`monitor_process` 退出分支只置 `running=false/last_exit`；宿主全树 `restart` 字面量只出现在无关的 `docker.restart`/`systemd.restart` 工具名里。 |
| desktop **H-2** verbatim 转发撞 `z.strictObject` | **部分成立** | 逐字转发与 `strictObject` 拒绝未知键都真实存在（我用 20 组生产端真实形状复测：17 通过 / 4 拒绝）；但被拒的 3 例是我**故意加字段**的漂移用例，当前生产端全部真实形状都能通过，风险是「未来加字段即崩」而非「现状已崩」。 |
| desktop **H-3** 审批 decision 无白名单 / `approve_session` 契约与后端不一致 | **部分成立** | 契约不一致与「未知 approvalId 静默 no-op」成立（`begin_response` 返回 `nullopt`、`finish` 分支即空操作），但 R1 暗示的「可越权拿到会话级授权」**不成立**：宿主与 sidecar 双双拒绝 `approve_session`。 |
| desktop **H-4** `mcp_server_list` 报 `running:false` 且状态对象有两个形状 | **成立** | `mcp_command` 的 `view_for` 硬编码 `{"running", false},{"toolCount", 0}` 且从不咨询 `mcp_`；`McpSupervisor::stop` 另手写一份 5 字段 status；`McpSupervisor` 公开 API 没有只读 status 查询。 |
| python **H-1** Agent 测试 100/153 被门禁覆盖 | **原报告成立，当前已修复** | 在 HEAD 语义下重算成立（`test_agent_*.py`→100，`test_*.py`→153，差 18 = 4 个未注册模块）；当前工作区 `scripts/check.mjs` 新增第 8 步 + `native/CMakeLists.txt` 改为 `test_*.py` 已封堵。 |
| python **H-2** 控制平面 129 测试在 CI 中从不运行 | **原报告成立，当前已修复** | HEAD 的 `check.yml` 中 `control-plane` 字面量计数为 **0**；当前工作区 `check.yml` 安装 `services/control-plane[test]` 并设 `YUKINAL_CONTROL_PLANE_REQUIRED=1`（计数 5）。 |
| python **H-3** `policyId` 在 API 边界被拒 | **成立** | 我用 `_parse_run_request` 直接复现：无 `policyId`/`permissionMode` 通过，带任何 `policyId` 一律 `-32602 policy selection is not supported by this Python Agent Node yet`；而 UI 对四个内建策略都会发送它。 |

**计数**：成立 **6**（native H-2、desktop C-1、desktop C-2、desktop H-1、desktop H-4、python H-3）；部分成立 **3**（native H-1、desktop H-2、desktop H-3）；原报告成立但当前已修复 **2**（python H-1、python H-2）；不成立 **0**；无法判定 **0**（仅 native H-2 的**触发**子项无法判定，静态判定仍为成立）。

> 关于 python H-1/H-2 的计数口径：这 2 条的核心事实（覆盖率缺口）在 HEAD 上**成立**，但工作区已有对应修复。按 Lead 的口径要求，凡被修复覆盖者读作「原报告成立，当前已修复」。因此 6 + 3 + 2 = **11 条，无漏项、无不成立、无无法判定**。

> 关于 native H-2 的「无法判定」：它**不计入**「无法判定」类，因为静态判定（成立）已经完成；无法判定的只是「Linux 上现场触发」这一步，已单列在第四节。

---

## 二、逐条复核详情

### 模板条目：native H-1 —— `exit_code` 无法区分「退出码 0」与「未收到 exit-status」

**R1 原断言**（`docs/review/r1-native.md:23-106`）：

1. 目标服务器关闭 channel 而不发 `exit-status` 时，`result.completed = true` 且 `exit_code = 0`（假成功）。
2. `result.exit_code < 0` 守卫（`native/src/ssh.cpp:2133`）是**死代码**。
3. 终端路径（`ssh.cpp:1708-1711`）同样无法表达「未收到」，只是退化为 `0` 而不是 `null`。

#### 我的独立证据

**(a) 静态链（我逐段读源码确认，非引用 R1）**

```cpp
// native/src/ssh.cpp:2131-2140
  if (result.error_code.empty()) {
    result.exit_code = libssh2_channel_get_exit_status(resources.channel_);
    if (result.exit_code < 0) {
      fail(&result, "result_unknown", "The SSH connection closed before an exit status was received");
    } else if (result.output_truncated && !preserve_truncated_output) {
      fail(&result, "output_limit", "SSH output exceeded the configured 64 KiB per-stream limit");
    } else {
      result.completed = true;
    }
  }
```

`libssh2_channel_get_exit_status` 的实现（`native/build-nmake/_deps/libssh2-src/src/channel.c:1648-1653`）：

```c
int libssh2_channel_get_exit_status(LIBSSH2_CHANNEL *channel)
{
    if(!channel)
        return 0;          /* 无 channel 也返回 0 */
    return channel->exit_status;
}
```

`exit_status` 的写入点（`.../src/packet.c:1129-1136`）—— 我确认**赋值是无条件的**，这决定了子项 2 的真假：

```c
uint32_t status = 0;
if(ssh2_get_u32(&buf, &status))
    rc = ssh2_err(session, LIBSSH2_ERROR_PROTO, "exit-signal status error");
else
    channelp->exit_status_received = 1;

channelp->exit_status = (int)status;    /* ← 无条件，且是 32 位截断为 int */
```

`ssh2_calloc` 确实零初始化（`.../src/misc.c:784-790`：`memset(p, 0, size)`），因此「从未收到」与「收到 0」在 `exit_status` 上不可区分 —— **子项 1 的机制成立**。

我另外确认：`libssh2_channel_has_exit_status` 在 `native/**` 的调用点计数为 **0**（唯一能表达「是否收到」的访问器未被使用），且 `native/tests/` 中没有任何断言触及 `exit_code` / `result_unknown`（对 `exit_code|exitCode|result_unknown|completed` 的搜索只命中 `test_network.cpp:38,54` 的 `YUKINAL_REQUIRE(!result.completed)`）。

**(b) 端到端复现（自己写的 mock，非复用 R1 的任何脚本）**

脚本（可运行，见本文件附录 A）：起一个 paramiko mock SSH 服务器，接受 exec 请求、发送输出、`shutdown_write()` 关掉 channel，**从不发送 `exit-status`**；再把真实 `native/build-nmake/yukinal-ssh-service.exe` 指到它，通过仓库自带的 `yukinal_ssh.SshServiceClient` 调 `server_exec`。

实测输出：

```
=== probe result ===
exit_code=0 stdout='probe-output\n'
VERDICT: the service reported a successful command with exit_code=0
         even though the server never sent an exit-status request.
```

⇒ **子项 1 成立，且有活体复现**。同一路径的 HTTP 响应体（我另用 `/v1/ssh/exec` 直连接复核）为
`{"status":"succeeded","exit_code":0,...}`，即「假成功」会一路冒泡到服务契约层。

**(c) 对子项 2（「死代码」）的证伪 —— R1 这里说错了**

`packet.c:1136` 的 `(int)status` 是无条件强转，所以**合法但 ≥ `0x80000000` 的 32 位退出码会变成负数**，从而让 `< 0` 守卫可达。我构造了 `exit-status = 0x80000000` 直接验证（附录 A 同款 mock，只改发送的状态值）：

```
requested exit-status = 0x80000000 (2147483648)
{
  "http_status": 502,
  "body": "{\"duration_ms\":4,\"error\":{\"code\":\"result_unknown\",
            \"message\":\"The SSH connection closed before an exit status was received\"},
            \"exit_code\":null,\"operation\":\"server_exec\",\"status\":\"failed\",...}"
}
```

⇒ 守卫**可达**，且触发时给出的是**错误诊断**（服务器明明发了状态，服务却说「连接在收到退出状态前关闭了」）。因此准确表述是「**可达但语义错误，且无法表达『未收到』**」，而不是「死代码」。
R1 自己在 `:87` 也写了「这是该守卫唯一可达的触发路径」，却在标题与 `:86` 主结论中用了「死代码」——**这是标题与正文的自相矛盾**，也是本次复核中 R1 唯一一处需要修正的定性。

**(d) 对子项 3（终端路径）的独立复现**

代码事实（我读的原文）：`ssh.cpp:1622` `int exit_code = -1;`；`:1708-1711` `if (libssh2_channel_eof(...)) { exit_code = libssh2_channel_get_exit_status(...); break; }`；`:1722-1724` 发 `{"type":"closed"},{"exit_code", exit_code < 0 ? nullptr : exit_code}`。

我起了 PTY/shell mock（同样不发 `exit-status`），让**服务器侧**主动关掉 channel，然后逐行读 worker 的 stdout NDJSON：

```
  frame: {"type": "ready"}
  frame: {"data": "dGVybWluYWwtYmFubmVyCg==", "type": "output"}
  frame: {"exit_code": 0, "type": "closed"}
```

⇒ **子项 3 成立**：`-1` 哨兵被 `:1709` 覆盖成 `0`，「未收到」退化为 `0` 而非 `null`。

补一个我在复核过程中**纠正自己**的细节（对下游有价值）：如果**客户端先关**（worker 在进入 `:1708` 的 EOF 判断前就 `break`），`exit_code` 仍保持 `-1`，此时确实会发 `exit_code: null` —— 这解释了两条不同代码路径的输出差异，也说明「null」只在客户端主动断开时出现，**不能**作为「服务器未发 exit-status」的信号。

**(e) 影响链（我追到 UI）**

`exit_code: 0` 会被下游当作成功：`native/desktop_host/src/desktop_terminal.cpp:365` 把 `exitCode` 原样转发（`exit_code < 0 ? nullptr : exit_code`），而 `apps/desktop/src/features/terminal/TerminalPane.tsx:161` 只在 `payload.exitCode === null` 时省略后缀，`0` 会被渲染成普通的「(exit 0)」。

#### 判定

**部分成立。** 主体后果（假成功、无法表达「未收到」、终端路径退化）**成立且我已端到端复现**；但「`< 0` 守卫是死代码」**不成立**（我用 `0x80000000` 触发成功），R1 作者在标题/`:86` 处把子项 2 的结论写反了，其正文 `:87` 反而写对了。

#### 最小复现

```pwsh
# 静态：守卫可达性的代码依据
Select-String -Path native\src\ssh.cpp -Pattern 'libssh2_channel_get_exit_status|result_unknown'

# 动态（需要 agent\.venv，约 20 秒）：假成功
$env:PYTHONPATH="D:\yukinal\agent\src"
& .\agent\.venv\Scripts\python.exe $env:TEMP\r3-exit-status-repro.py `
    --service-binary "D:\yukinal\native\build-nmake\yukinal-ssh-service.exe"
# 期望：exit_code=0（服务器从未发送 exit-status）

# 动态：守卫确实可达（证明「死代码」不成立）
& .\agent\.venv\Scripts\python.exe $env:TEMP\r3-large-exit-status-probe.py `
    "D:\yukinal\native\build-nmake\yukinal-ssh-service.exe" 2147483648
# 期望：502 / result_unknown「The SSH connection closed before an exit status was received」

# 动态：终端路径退化为 0 而非 null
& .\agent\.venv\Scripts\python.exe $env:TEMP\r3-terminal-exit-status-probe.py `
    "D:\yukinal\native\build-nmake\yukinal-ssh-service.exe"
# 期望：{"exit_code": 0, "type": "closed"}
```

脚本源码见**附录 A**（`%TEMP%` 中的脚本不持久，附录内是可直接重建的完整版本）。

---

### native H-2 —— Linux 上内置 OpenSSL 的 TLS 写路径未屏蔽 SIGPIPE，对端 RST 可终止进程

**R1 原断言**（`r1-native.md:110-198`）：`native/` 全树没有任何 SIGPIPE 处理；内置 OpenSSL 的 TLS 写走 `write(2)`，对端 RST 会以 SIGPIPE 终止进程。

#### 我的独立证据

| 检查项 | 我的命令 | 结果 |
| --- | --- | --- |
| native 全树有无信号处理 | 在 `native/src`、`native/include`、`native/desktop_host`、`native/tests` 搜索 `signal(` / `sigaction` | **0 处** |
| `prevent_sigpipe` 定义/调用 | 读 `ssh.cpp:72-81` | 定义 1 处；**唯一调用点** `:730-735`，且被 `if ((use_tls_proxy \|\| use_socks_proxy \|\| set_nonblocking(...)) && prevent_sigpipe(candidate))` 收窄 —— 只在走代理或非阻塞时才尝试，直连 TLS 不经过 |
| `prevent_sigpipe` 的 Linux 行为 | 读 `ssh.cpp:72-81` | `#ifdef SO_NOSIGPIPE` … `#else (void)socket; return true;` —— Linux 下是**空操作** |
| `MSG_NOSIGNAL` 使用点 | 在 `native/` 搜索 | 仅 `ssh.cpp:452-457`、`ssh.cpp:507-511`、`host_certificate.cpp:767-782`，且都写在 `#ifdef MSG_NOSIGNAL` … `#else 0` 里 |
| OpenSSL 写路径 | 读内置源码 `.../yukinal_openssl/include/internal/sockets.h:198-203` | 通用 POSIX 分支 `writesocket(s,b,n)` → `write((s),(b),(n))` |
| OpenSSL 调用点 | 在 `crypto/` 搜索 `writesocket` | `bss_sock.c:156`、`bss_conn.c:433`、`bss_dgram.c:462` |
| OpenSSL 内有无 `MSG_NOSIGNAL` | 在 `crypto/**` 搜索 | **0 处** |
| libssh2 是否受影响 | 读 `libssh2_priv.h:993-1003` | 不受影响：`SSH2_SOCKET_SEND_FLAGS(session) = (flag.sigpipe ? 0 : MSG_NOSIGNAL)`，libssh2 自己传 `MSG_NOSIGNAL` |
| httplib 的兜底 | 读 `httplib.h:6083-6089` | 仅 `Server::Server()` 里 `signal(SIGPIPE, SIG_IGN)`（`:6087`），且 `CPPHTTPLIB_SEND_FLAGS` 默认 `0`（`:136-137`），`:5939` 用它发送 |
| httplib 兜底覆盖范围 | 读 `native/src/http_service.cpp:108` 与 `native/src/main.cpp:95-133` | ssh-service 里只有 1 个 `httplib::Server`；worker 模式在 `:111-114` **早于** `serve()` 返回 ⇒ worker 进程没有 httplib 兜底 |
| 桌面宿主 | 读 `native/desktop_host/src/main.cpp:196`、`:374-375`、`:393` | `UiServer`（内含 `httplib::Server server_`）在**所有**路径上都被构造 ⇒ 桌面宿主全程忽略 SIGPIPE |

**R1 正确**：内置 OpenSSL 的 TLS 写没有任何 `MSG_NOSIGNAL`，也不受 libssh2 的 `flag.sigpipe` 保护；而 `prevent_sigpipe` 在 Linux 上是空操作、调用点还被收窄，不能当作覆盖。

#### 无法判定的部分

我在本机**无法实际触发** `SIGPIPE` 终止：

- `python %TEMP%\r3-sigpipe-probe.py` → `AttributeError: module 'signal' has no attribute 'SIGPIPE'`（Windows 无 `SIGPIPE`）；
- `wsl -e bash` 失败（存在 `wsl` 但没有已安装的发行版）；
- `docker` 不在 `PATH`。

因此「对端 RST 时进程真的会死」这一步**未能**在本环境实测。R1 自己在 `:198` 也把「现场触发」标为待证实。

#### 判定

**成立**（静态链完整、每一环我都独立读到源码）；但**触发面无法判定** —— 缺少 Linux 运行时（无 WSL 发行版、无 docker）来证明 `write(2)` 收到 RST 后进程确实被 `SIGPIPE` 终止。

#### 最小复现（静态链，标记「静态」）

```pwsh
# 1) native 全树无信号处理
git grep -nE "signal\(|sigaction" -- native/src native/include native/desktop_host native/tests   # 0 命中
# 2) prevent_sigpipe 的 Linux 分支是空操作 + 调用点被收窄
Select-String -Path native\src\ssh.cpp -Pattern 'prevent_sigpipe' -Context 2,2
# 3) OpenSSL 写路径落到 write(2)
Select-String -Path native\build-nmake\yukinal_openssl-prefix\src\yukinal_openssl\include\internal\sockets.h -Pattern 'define writesocket'
Select-String -Path native\build-nmake\yukinal_openssl-prefix\src\yukinal_openssl\crypto\bio\bss_sock.c -Pattern 'writesocket'
# 4) OpenSSL crypto/ 内 MSG_NOSIGNAL 为 0
#    （在 native\build-nmake\yukinal_openssl-prefix\src\yukinal_openssl\crypto 下搜索 MSG_NOSIGNAL）
# 5) httplib 只在 Server::Server() 忽略 SIGPIPE
Select-String -Path native\build-nmake\_deps\httplib-src\httplib.h -Pattern 'SIGPIPE|CPPHTTPLIB_SEND_FLAGS'
```

---

### desktop C-1 —— keyboard-interactive 认证链路端到端不可达

**R1 原断言**（`r1-desktop.md:26-99`）：PTY worker 会发 `auth_prompt`，但桌面宿主的帧解析器没有该分支而静默丢弃；`server_auth_respond`/`server_auth_cancel` 无条件返回 `accepted:false`；UI 订阅的 `server.auth_challenge` 无生产者且把 `accepted:false` 渲染成「已过期」。

#### 我的独立证据

| 检查项 | 我的命令 | 结果 |
| --- | --- | --- |
| 生产者是否存在 | `git grep -n '"auth_prompt"' -- native` | 命中 `native/src/ssh.cpp:1494`（`{"type", "auth_prompt"}`）—— **生产者真实存在** |
| 桌面宿主有无消费 | `git grep -n -E "auth_prompt\|auth_response" -- native/desktop_host` | **0 命中** |
| 终端帧解析分支 | 搜 `desktop_terminal.cpp` 的 `type == "` | 只有 `:390 ready`、`:397 output`、`:402 error`，**无 else、无 auth_prompt 分支** |
| 回执是否可能为 true | 读 `ipc_router.cpp:8760-8793` | `:8767-8769` 注释称「Keyboard-interactive prompts are owned by the C++ PTY worker」；`:8792` `return {true, {{"accepted", false}}};` —— 该函数内**无任何路径**返回 `true` |
| UI 订阅是否有生产者 | 对 `server.auth_challenge` 在 `native/`+`agent/` 逐字面量扫描 | **0 命中** |
| UI 的文案 | 读 `ServerAuthChallengeModal.tsx:26`、`:76` | 订阅 `server.auth_challenge`；`accepted === false` 分支写「本次认证挑战已过期或已被处理」 |

**关键正面证据（R1 未提供，我补上的）**：生产者的确**能在真实二进制上跑通**。仓库自带的 `scripts/test_ssh_mock.py:806-890`（`exercise_cpp_terminal`）用键盘交互 mock（`:86-94` 查询 "Second factor"/"Enter the one-time code"、`:96-101` 接受 `["654321"]`）驱动真实 `yukinal-ssh-service.exe`，断言首个事件是 `auth_prompt`（`:845-853`）、写入 `{"type":"auth_response","responses":["654321"]}`（`:854`）、随后拿到 `ready` 与 `keyboard-interactive-success`（`:884-885`）。

我实际运行：

```
Test #4: ssh_mock_integration .............   Passed    8.88 sec
Test #5: agent_ssh_mock_integration .......  Passed   12.79 sec
100% tests passed out of 2
```

以及直接跑脚本本身：`PASS SSH exec, HTTP/HTTPS CONNECT with certificate verification, SFTP listing/stat/read/write/exclusive-create/remove/link-count, auth rejection, timeout, output cap, and host-key rejection`。

⇒ 结论比 R1 更强：**不是「认证方式不支持」，而是「服务层已实现并验证、桌面宿主这一段把帧丢掉了」**。C-1 因此是纯宿主侧接线缺口，修复成本评估应当据此下调（不需要在服务层新增能力）。

#### 判定

**成立。** 生产者在服务层可用且已被 CTest 覆盖；桌面宿主侧消费为 0；`accepted` 永不为 true；UI 通道无生产者且把失败误译为「过期」。

#### 最小复现

```pwsh
# 正面：服务层 keyboard-interactive 生产路径可用（端到端走真实 exe）
ctest --test-dir native/build-nmake -C Release -R "ssh_mock_integration|agent_ssh_mock_integration" --output-on-failure
# 期望：2/2 Passed

# 反面：宿主侧消费缺失 + 回执恒为 false
git grep -n -E "auth_prompt|auth_response" -- native/desktop_host    # 0 命中
Select-String -Path native\desktop_host\src\ipc_router.cpp -Pattern 'accepted' -Context 3,3
```

---

### desktop C-2 —— 6 个 A 类死通道 + 3 个 B 类声明名 + 1 个第三类状态

**R1 原断言**（`r1-desktop.md:101-201`）：`SUBSCRIBABLE` 19 项中 6 项无生产者；`DECLARED_WITHOUT_A_SUBSCRIBER` 4 项中 3 项无生产者、`terminal.opened` 有生产者；13 项有生产者。

#### 我的独立证据

我对每一个名字在 `native/` + `agent/` 里按**字面量**重扫（不依赖 R1 的扫描脚本）：

| 通道 | 类别 | native+agent 字面量命中 | 我的判定 |
| --- | --- | --- | --- |
| `activity.created` | A | **0** | 无生产者 |
| `agent.usage` | A | **0** | 无生产者 |
| `agent.thinking` | A | **0** | 无生产者 |
| `server.auth_challenge` | A | **0** | 无生产者（＝C-1） |
| `terminal.output_lost` | A | **0** | 无生产者 |
| `investigation.schedule_notification` | A | **0** | 无生产者 |
| `server.connected` | B | **0** | 无生产者 |
| `server.disconnected` | B | **0** | 无生产者 |
| `server.updated` | B | **0** | 无生产者 |
| `terminal.opened` | 第三类 | 有（`desktop_terminal.cpp:391`） | **有生产者、无 UI 订阅者** |

对照侧（有生产者的 13 项，我逐条复核命中位置）：`agent.started` / `agent.text` / `agent.tool_call` / `agent.tool_result` / `agent.waiting_approval` / `agent.approval_expired` / `agent.completed` / `agent.failed`（Python 侧 8 项）+ `terminal.data` / `terminal.closed`（C++ 2 项）+ `mcp.oauth_device_code`（`ipc_router.cpp:9576`）/ `file.local_dropped`（`:6003`）/ `file.transfer_updated`（`:5948`）（C++ 3 项）= **13**。19 − 13 = **6**，与 A 类计数自洽。

UI 订阅侧我逐个确认存在：`ActivityFeed.tsx:70,139`、`InfrastructureDashboard.tsx:35,55`、`ServerOverview.tsx:397,419`、`useWorkpageOperations.ts:48,95`、`useAgentRun.ts:205,214`、`ServerAuthChallengeModal.tsx:26`、`TerminalPane.tsx:168`、`InvestigationsPane.tsx:102,106`。

我还独立确认了 R1 引用的两处「自证」文本确实存在且确实为假：
- `packages/shared/src/schemas/event-vocabulary.test.ts:5-13` 断言「Rust emits `server.updated` and `terminal.opened`」以及「Every agent member has a producer … `agent.text` and `agent.usage` complete the provider-to-UI path」——但 `server.updated` 生产者为 0、`agent.usage` 生产者为 0；
- `:90-112` 的注释把生产者清单标注为「transcribed from `apps/agent/src/agent-loop.ts`」——该文件不存在（仓库已无 Rust/agent-loop 路径）。

#### 判定

**成立。** 9 个「无生产者」名字全部复扫为 0，13 个「有生产者」名字全部复核到具体行号，`terminal.opened` 确为独立的第三类状态。R1 把 A/B 两类分开计数的口径也是对的。

#### 最小复现

```pwsh
foreach ($n in @("activity.created","agent.usage","agent.thinking","server.auth_challenge",
                 "terminal.output_lost","investigation.schedule_notification",
                 "server.connected","server.disconnected","server.updated")) {
  "$n -> " + (& git grep -n --fixed-strings "`"$n`"" -- native agent 2>$null | Measure-Object).Count
}
# 期望：全部 0

git grep -n "terminal.opened" -- native     # 期望：desktop_terminal.cpp:391
Select-String -Path packages\shared\src\schemas\event-vocabulary.test.ts -Pattern 'Rust emits|agent-loop'
```

---

### desktop H-1 —— `agent_status.restart` 与自动恢复：契约/UI/fixture 齐全，宿主无生产者

**R1 原断言**（`r1-desktop.md:207-260`）：契约与 UI 有 `restart`，`native/` 里 0 个 `"restart"` 字面量，`monitor_process` 没有重启循环。

#### 我的独立证据

| 检查项 | 我的命令 | 结果 |
| --- | --- | --- |
| 宿主有无 `restart` 生产 | `git grep -n '\brestart' -- native/desktop_host` | 命中全部是**无关**的 MCP host-tool 名 `docker.restart`/`systemd.restart`（`desktop_approval.cpp:27,28`、`ipc_router.cpp:935,937,6294`）与注释；**没有任何 sidecar 重启语义** |
| `agent_status` 响应字段 | 读 `ipc_router.cpp:9006-9037` | 显式构造 `{running,pid,protocolVersion,agentVersion,toolCount,entry,startedAt,lastExit}` —— **无 `restart`** |
| 结构体是否有该字段 | 读 `sidecar.hpp:63-72` | `SidecarStatus` = `running,pid,protocol_version,agent_version,tool_count,entry,started_at,last_exit` —— **无 restart** |
| 是否有 supervisor 逻辑 | 读 `sidecar.cpp:383-415` `monitor_process` | 退出分支只有 `running=false; last_exit=exit_info; exited=true; notify_pending_of_exit();` —— **无重启循环、无退避、无尝试计数** |
| 契约侧是否存在 | 读 `packages/shared/src/schemas/ipc.ts:144-159`、`types/lifecycle.ts:8-13` | `AgentStatusSchema` 有 `restart: RestartRecordSchema.optional()`；`RestartRecord {attempt;maxAttempts;exhausted;at}` —— **契约确实承诺了** |
| UI 是否消费 | 读 `useAgentRun.ts:312-335` | 有专门的 restart-record effect —— **UI 确实在等** |

#### 判定

**成立。** 契约（`optional` 字段）、类型（`RestartRecord`）、UI（effect）、fixture 都齐备，宿主侧既无字段也无逻辑：`restart` 是一个「只能被别处填充、宿主永不填充」的字段。

#### 最小复现

```pwsh
git grep -n '\brestart' -- native/desktop_host          # 只有 docker./systemd.restart 工具名
Select-String -Path native\desktop_host\src\ipc_router.cpp -Pattern 'agent_status' -Context 0,30
Select-String -Path native\desktop_host\src\sidecar.cpp -Pattern 'monitor_process' -Context 0,35
# 静态：契约有、宿主无
Select-String -Path packages\shared\src\schemas\ipc.ts -Pattern 'restart'
```

---

### desktop H-2 —— verbatim 转发撞 `z.strictObject` 边界

**R1 原断言**（`r1-desktop.md:262-296`）：宿主把 payload 逐字转发给 sidecar，而 UI 侧响应 schema 是 `z.strictObject`，sidecar 多一个字段就会让 UI 抛 `ZodError`。

#### 我的独立证据

**(a) 逐字转发属实**：`ipc_router.cpp:9119-9122` 对除 `agent_approval_respond` 外的所有方法 `return {true, client.request(found->second, params, timeout)}`（注释 `:9112-9115`「the host forwards the payload unchanged」）；`provider_command`（`:9643-9664`）同样是单次 `client.request(..., params, ...)`。

**(b) `strictObject` 会拒绝未知键属实**：`apps/desktop/src/lib/ipc.ts:109-134` 的 `callDesktop` 做 `IPC_SCHEMAS[command].response.parse(raw)`，且**不包 try/catch** ⇒ `ZodError` 会向上抛。

**(c) 我用生产端真实形状重放（关键：不能自己编 payload）**。我先从 C++/Python 生产端抄下每种响应的**逐字字段集**，再喂给编译后的 `packages/shared/dist/schemas/ipc.js`：

```
结果：17 accepted / 4 rejected
```

4 个被拒中，**只有 1 个**不是故意漂移：`agent_context_reset` 传 `{"deleted": true}` 被拒 —— 但这是**我抄错了生产者**：`{"deleted": ...}` 属于 `agent.context.delete`，该名字不在 `IPC_SCHEMAS` 里；`chat_session_delete` 的契约才是 `{deleted}`（`ipc.ts:517-519`）。其余 3 个被拒用例是我**故意**加的漂移字段（`+lastProbeAt`、`+queued`、`+skipped`），被 `z.strictObject` 按预期拒绝。

⇒ **纠正后的净结论：当前所有真实生产端形状都能通过**，不存在「现在就已经崩」的实例。

#### 判定

**部分成立。** 「逐字转发 + `strictObject` 零容错」这对组合是真的，且我用 3 个漂移用例证明「sidecar 多一个字段 UI 就崩」的**机制**成立；但 R1 的措辞暗示现状已经有 break，**我未能找到任何一个当前生产的、会被拒的真实响应**。准确表述是「**加字段即崩的脆弱边界**（结构性风险），而非现存故障」。

#### 最小复现

```pwsh
# 17 accepted / 4 rejected；4 个被拒中 3 个是我故意加的漂移字段，
# 1 个是把 agent.context.delete 的 {deleted} 误当成 agent_context_reset
node $env:TEMP\r3-strict-probe4.mjs
Select-String -Path apps\desktop\src\lib\ipc.ts -Pattern 'IPC_SCHEMAS\[command\]\.response\.parse'
```

---

### desktop H-3 —— 审批 decision 在宿主入口没有白名单；未知 approvalId 静默 no-op；`approve_session` 契约允许但所有后端拒绝

**R1 原断言**（`r1-desktop.md:298-338`）：宿主 `begin_response` 对未知 approvalId 处理不当（可被当作成功），`approve_session` 在契约里允许但后端全部拒绝。

#### 我的独立证据

**(a) 宿主账本（我读了全部三个相关函数）**

```cpp
// native/desktop_host/src/desktop_approval.cpp:304-334
  // :309  未知 approvalId → return std::nullopt;
  // :311  过期 → ...
  // :315  run_id 不匹配 → ...
  // :318  非 Pending → ...
  // :322  approve_once → ...
  // :324  reject → ...
  // :326  approve_session → throw "host action approvals are per call and cannot be session grants"
  // :330  else → throw "approval decision is invalid"
```

`finish_response`（`:336-353`）要求 `accepted && token.approve_once && entry.binding.expires_at > now`；`consume`（`:355-380`）有 1 秒等待循环与 `kMaxApprovalTtlSeconds = 180`（`:24`）。

**(b) 「未知 approvalId 被当成成功」—— R1 说重了，但确有真实缺陷**

`begin_response` 返回 `std::nullopt` 时，宿主侧 `ipc_router.cpp:9152-9158` 的 `finish` lambda 在 `!token.has_value()` 时**是空操作**，函数照常返回 `{{"accepted", ...}}`（`:9171`）。所以实际行为是：**未知 ID → 不做事、但回一个看起来正常的 `accepted`**。这不是「被当成成功授权」（没有授权发生），而是**静默无操作 + 误导性回执**。R1 的定性偏重。

**(c) 越权路径不成立 —— 我对 R1 的核心反证**

R1 的标题暗示 `approve_session` 是「契约允许但后端拒绝」的不一致；我确认**两端都拒绝，因此不存在越权面**：

- 宿主：`:326-328` 直接 `throw`；
- sidecar：`agent/src/yukinal_agent/sidecar.py:774-792` 的 `_approval_respond` 白名单是 `("approve_once","reject")`（`:783`）。

我用 `PythonSidecar.__new__` 绕过构造直接调 `_approval_respond`：

```
  RESULT 'approve_once'    -> {"accepted": false}
  RESULT 'reject'          -> {"accepted": false}
  FAULT  'approve_session' -> [-32602] invalid approval response
  FAULT  'approve'         -> [-32602] invalid approval response
  FAULT  ''                -> [-32602] invalid approval response
```

（`{"accepted": false}` 是因为我传的 `apr_probe` 不存在；关键是 `approve_session` 走的是**参数校验失败**而不是被接受。）

**(d) 但「契约与 UI 承诺了不可用的能力」成立**

`APPROVAL_DECISIONS = ["approve_once","approve_session","reject"]`（`packages/shared/src/types/chat.ts:411`）与 `ApprovalResponseSchema`（`permission.ts:140-145`）都包含 `approve_session`；`AgentEntryView.tsx:164-168` 的「本次运行批准」按钮仅在 `entry.approval.sessionGrantable !== false` 时渲染，而 sidecar **恒定**发 `"sessionGrantable": False`（`sidecar.py:1076`）⇒ 该按钮在通路上永不渲染（R1 的 M-3 也说了这点，与 H-3 有重叠）。

**(e) UI 误译**

`useAgentRun.ts:468-475`：`accepted === false` → `:469` 显示「审批已失效」。结合 (b)，未知/失效 ID 与「已过期」混在同一个文案里，用户无法区分「票不在」（可能是接线错误）与「票过期」。

#### 判定

**部分成立。** 成立的部分：契约/UI 与后端能力不一致（`approve_session`）、未知 approvalId 静默 no-op、UI 文案误译。**被夸大的部分**：不存在「未知 approvalId 被当成成功授权」，也不存在「`approve_session` 可越权」——宿主（`:326-328`）与 sidecar（`:783`）**双双硬拒**，且 `finish_response` 仍要求 `approve_once` + 未过期，双重前提下没有授权面。

#### 最小复现

```pwsh
# sidecar 侧：approve_session 被参数校验拒绝
& .\agent\.venv\Scripts\python.exe $env:TEMP\r3-python-boundary-probe.py
# 期望：approve_session -> [-32602] invalid approval response

# 宿主侧：approve_session 被 throw；未知 ID 走 nullopt
Select-String -Path native\desktop_host\src\desktop_approval.cpp -Pattern 'std::nullopt|approve_session|per call' -Context 2,2
Select-String -Path native\desktop_host\src\ipc_router.cpp -Pattern 'token.has_value\(\)' -Context 3,3
```

---

### desktop H-4 —— `mcp_server_list` 对运行中的服务器报 `running:false`，且 MCP 状态对象有两个形状

#### 我的独立证据

```cpp
// native/desktop_host/src/ipc_router.cpp:9186-9204
  const auto view_for = [](const nlohmann::json& config) {
    nlohmann::json view = {
        {"config", config},
        {"status",
         {{"serverId", config.at("id")},
          {"running", false},                   // ← 硬编码
          {"toolCount", 0},                     // ← 硬编码
          {"stderrTail", nlohmann::json::array()},
          {"diagnostics", nlohmann::json::array()}}},
        {"tools", nlohmann::json::array()},
    };
```

`mcp_server_list`（`:9206-9218`）遍历 `database_->list_mcp_servers()` 后对每一项调用 `view_for` —— **全程不咨询 `mcp_`**（`McpSupervisor` 实例）。

**第二个形状**：`McpSupervisor::stop`（`native/desktop_host/src/desktop_mcp.cpp:1174-1192`）另手写一份 status：

```cpp
  nlohmann::json status = {{"serverId", server_id},
                           {"running", false},
                           {"toolCount", 0},
                           {"stderrTail", nlohmann::json::array()},
                           {"diagnostics", nlohmann::json::array()}};
```

而 `McpSupervisor::start`（`:1167-1172`）走的是 `impl_->status_for(session)`，其实现（`:1138-1156`）里 `pid`/`program`/`startedAt` **硬编码为 null**、`protocolVersion`/`serverName`/`serverVersion` 取自 session、`toolCount` 用 `session->tools.size()`、**无 `lastExit`、无 `restart`**。

我还确认 `McpSupervisor` 的公开 API **没有只读 status 查询**（只有 `start`/`stop`），因此 `view_for` 想查也查不到 —— 这点支持 R1 的「结构性」判断：不是漏写一个字段，而是缺少可用的查询接口。

契约侧（`packages/shared/src/schemas/mcp.ts:113-127`）的 `McpServerStatusSchema` 允许 `lastExit`/`restart`，`:106-111` 有 `McpExitRecordSchema{code?,signal?,at,reason}` ⇒ **契约承诺了过期信息，宿主两个形状都不产出**。

#### 判定

**成立。** `mcp_server_list` 恒报 `running:false`/`toolCount:0` 且不查 `mcp_`；`stop` 与 `start` 产出两个不同形状的 status；契约要求的 `lastExit`/`restart` 均无生产者；`McpSupervisor` 缺少只读 status 接口。

#### 最小复现

```pwsh
Select-String -Path native\desktop_host\src\ipc_router.cpp -Pattern 'const auto view_for' -Context 0,20
Select-String -Path native\desktop_host\src\desktop_mcp.cpp -Pattern 'status_for|nlohmann::json status' -Context 0,20
Select-String -Path native\desktop_host\include\yukinal\desktop_host\desktop_mcp.hpp -Pattern 'nlohmann::json (start|stop|status)'
# 期望：只有 start/stop，没有只读 status
```

---

### python H-1 —— Agent 测试只有 100/153 被门禁覆盖，18 个测试在任何路径下都不执行

**R1 原断言**（`r1-python.md:37-97`）：CMake 只注册 `test_agent_*.py` 与两个一次性目标，其余 4 个模块的 18 个测试既不被 CTest 也不被 `check.mjs` 覆盖；`check.mjs` 只有 12 步。

#### 我的独立证据（全部我自己的命令重算）

| 度量 | 命令 | 结果 |
| --- | --- | --- |
| 全量 | `-m unittest discover -s agent\tests -p "test_*.py"` | **Ran 153 tests in 25.649s … OK** |
| 门禁口径 | `-m unittest discover -s agent\tests -p "test_agent_*.py"` | **Ran 100 tests in 8.588s … OK** |
| 差额 | 153 − 100 | **53** |
| HEAD 另行注册的 2 个目标 | `git show HEAD:native/CMakeLists.txt` | `:218-219 python_client_tests`（`test_client.py`）、`:226-227 service_http_integration`（`test_service_integration.py`） |
| 这两个模块的规模 | 分别 discover | `test_client` **31**、`test_service_integration` **4**（`OK (skipped=4)`） |
| 未被任何路径覆盖 | 153 − 100 − 31 − 4 | **18** = `test_network_snapshot`(5) + `test_provider_store`(5) + `test_reasoning_effort`(3) + `test_system_snapshot`(5) |
| `check.mjs` 步数 | `node %TEMP%\r3-count-steps2.cjs scripts\check.mjs` | **工作区 13 步 / HEAD 12 步** |

⇒ R1 的 100/153、18、12 步三个数字**全部被我独立重算复现**。

#### 修复对照（工作区，非 HEAD）

- `native/CMakeLists.txt`：`:217` 注释改成「Every `agent/tests/test_*.py` module is covered by the single `python_agent_tests`」，`:236-237` 的 glob 由 `test_agent_*.py` 改为 **`test_*.py`**，`:240` 注入 `YUKINAL_SSH_SERVICE_BINARY=$<TARGET_FILE:yukinal-ssh-service>`；HEAD 的 `python_client_tests`/`service_http_integration` 两个一次性注册被删除。等效于 CTest 口径从 135 → **153**。
- `scripts/check.mjs`：工作区新增第 8 步 `"Python Agent tests (full unittest discovery)"`（`:183-196`），命令为 `-m unittest discover -s <agentTestsPath> -p test_*.py -v`，并把 `YUKINAL_SSH_SERVICE_BINARY` 传进环境。

#### 判定

**原报告成立，当前已修复。** 在 HEAD 语义下 H-1 的每一项数值都成立（我复算一致）；当前工作区的 CMake glob + `check.mjs` 第 8 步已经封堵这条缺口。按 Lead 的口径记为「原报告成立，当前已修复」。

#### 最小复现

```pwsh
# HEAD 语义下的缺口
& .\agent\.venv\Scripts\python.exe -m unittest discover -s agent\tests -p "test_agent_*.py"   # 100
& .\agent\.venv\Scripts\python.exe -m unittest discover -s agent\tests -p "test_*.py"         # 153
node $env:TEMP\r3-count-steps2.cjs scripts\check.mjs                                          # 工作区 13 / HEAD 12
# 修复证据
Select-String -Path native\CMakeLists.txt -Pattern "test_\*\.py"        # :237
Select-String -Path scripts\check.mjs -Pattern "full unittest discovery"  # :188
```

---

### python H-2 —— 控制平面 129 个测试在 CI 中从不运行

**R1 原断言**（`r1-python.md:101-267`）：`services/control-plane` 的测试在 CI 中没有任何执行路径，venv 不存在即静默跳过。

#### 我的独立证据

| 度量 | 命令 | 结果 |
| --- | --- | --- |
| 控制平面测试规模 | `& services\control-plane\.venv\Scripts\python.exe -m unittest discover -s services\control-plane\tests` | **Ran 129 tests in 11.871s … OK** |
| HEAD CI 是否涉及 | `git show HEAD:.github/workflows/check.yml` 中 `control-plane\|CONTROL_PLANE` 计数 | **0** |
| HEAD 的安装行 | `git show HEAD:.github/workflows/check.yml` | `:56 pip install -e "agent[agent,models,test]"`，`:89-90 pnpm install + pnpm check` —— **没有** control-plane 的安装 |
| HEAD 的 check.mjs 步骤 | 同上 | `:57-64 control plane tests (SQLite)`，`skip: !controlPlanePython`（`:62`）⇒ 无解释器时**跳过而非失败** |
| 工作区 CI | `Select-String .github\workflows\check.yml -Pattern 'control-plane\|CONTROL_PLANE'` | **5 处**：`:58-62` 注释 + `pip install -e "services/control-plane[test]"`，`:96-100 YUKINAL_CONTROL_PLANE_REQUIRED: "1"` |
| 工作区 check.mjs 的加固 | 读 `:108-152`、`:172-182` | 两级探针（core/full）；`:118 controlPlaneRequired`；`:125-138` 显式指定的解释器坏了就 `failGate`；`:142-152`「装了 core 但没有 test extra」或 `REQUIRED=1` ⇒ `failGate`；`:180` 只有「全都没有依赖」才允许 skip |

#### 判定

**原报告成立，当前已修复。** HEAD 上 129 个测试确实无任何执行路径（CI 0 命中、check.mjs 静默 skip）；当前工作区的 `check.yml` 安装 + `REQUIRED=1` + `check.mjs` 的两级探针与 failGate 逻辑已把「静默跳过」改成「显式失败」。
补充一点 R1 未展开、但对定级重要的观察：`services/control-plane/.venv` **在本机存在**，所以本地跑 `pnpm check` 是绿的 —— 正是这种「本机绿、CI 从不跑」的组合让该缺口能长期存活。

#### 最小复现

```pwsh
& .\services\control-plane\.venv\Scripts\python.exe -m unittest discover -s services\control-plane\tests
# 期望：Ran 129 tests … OK
(git show HEAD:.github/workflows/check.yml | Select-String -Pattern 'control-plane' | Measure-Object).Count   # 0
(Select-String -Path .github\workflows\check.yml -Pattern 'control-plane|CONTROL_PLANE' | Measure-Object).Count # 5
```

---

### python H-3 —— `policyId` 契约在 API 边界被拒绝

**R1 原断言**（`r1-python.md:268-331`）：`policyId` 被写入契约与 UI，但 sidecar 一律拒绝，选择任何内建策略都会让 run 启动失败。

#### 我的独立证据

**(a) sidecar 侧直接复现**（`_parse_run_request`，不经过宿主）：

```
== r1-python H-3: _parse_run_request (sidecar.py:1612-1615) ==
  OK    baseline (no policyId)
  OK    permissionMode=auto
  FAULT policyId=policy.local      -> [-32602] policy selection is not supported by this Python Agent Node yet
  FAULT policyId=policy.production -> [-32602] policy selection is not supported by this Python Agent Node yet
```

对应代码 `agent/src/yukinal_agent/sidecar.py:1612-1615`：`policyId` 出现即 `raise _RpcFault(RPC_INVALID_PARAMS, "policy selection is not supported by this Python Agent Node yet")`。

**(b) UI 会不会真的发送它 —— 我追了完整调用链**

- `apps/desktop/src/features/agent/AgentComposer.tsx:634-647`（策略选择器 UI，四个内建选项 + auto）→ `:132-134,156-157,669`；
- `AgentPanel.tsx:290 policyId: requestPolicyId(runPolicy)`，而 `run-policy.ts:81-83 requestPolicyId(policy) { return policy ?? undefined; }`，`:44-65` 定义四个内建 id；
- `AgentPanel.tsx:154 useState<RunPolicyChoice>(null)` ⇒ 默认不选时 `policyId = undefined`，**此时不发送**，run 正常；
- 一旦用户选了任一内建策略：`useAgentRun.ts:402 policyId: input.policyId ?? undefined` → 宿主 `ipc_router.cpp:9119-9122` 逐字转发 → sidecar `-32602`。

**(c) 契约层是否承诺**：`packages/shared/src/schemas/ipc.ts:394-441` 的 `agent_run_start` 有可选 `policyId`（`:418`，注释 `:417`「Forwarded verbatim to the sidecar」）；`permission.ts:104 policyId`；`agent.ts:121,147` 也带可选 `policyId`。

**(d) 反向一致性**：`sidecar.py:216` 的 `system.describe` 报 `"permissionPolicyIds": []`，即 sidecar 自己声明「我没有任何策略」，与拒绝行为自洽 —— 但与 UI 展示四个可选策略不自洽。

#### 判定

**成立。** 边界拒绝可复现（`-32602`），UI 路径可达（选任一内建策略即触发），契约承诺存在，且 `system.describe` 自报空策略集。影响范围要精确：**默认（不选策略）不触发**，只有用户主动选策略才失败 —— 这一点 R1 的表述（「选择任何内建策略都会让 run 启动失败」）是准确的。

#### 最小复现

```pwsh
& .\agent\.venv\Scripts\python.exe $env:TEMP\r3-python-boundary-probe.py
# 期望：policyId=policy.local / policy.production -> [-32602] policy selection is not supported …

# UI 可达性（静态链）
Select-String -Path apps\desktop\src\features\agent\AgentComposer.tsx -Pattern 'RUN_POLICY|policy' | Select-Object -First 8
Select-String -Path apps\desktop\src\features\agent\AgentPanel.tsx -Pattern 'requestPolicyId'
Select-String -Path apps\desktop\src\features\agent\useAgentRun.ts -Pattern 'policyId'
Select-String -Path agent\src\yukinal_agent\sidecar.py -Pattern 'policy selection is not supported'
```

---

## 三、本次验证中被推翻的断言

1. **native H-1 子项 2「`exit_code < 0` 守卫是死代码」—— 被推翻。**
   反证：`packet.c:1136` 的 `(int)status` 是无条件 32 位强转，`0x80000000` 会变成负数。我用 `exit-status = 0x80000000` 实测得到 `502 / result_unknown`，守卫可达。正确表述是「可达但语义错误、且无法表达『未收到』」。R1 的正文 `:87` 与标题自相矛盾。

2. **desktop H-3「未知 approvalId 被当成成功（可越权）」—— 被推翻（降级为「静默 no-op + 误导性回执」）。**
   反证：`begin_response` 对未知 ID 返回 `std::nullopt`，`finish` 分支在 `!token.has_value()` 时是空操作，**没有授权发生**；且 `finish_response:336-353` 仍要求 `accepted && approve_once && 未过期`。另外 `approve_session` 在宿主（`desktop_approval.cpp:326-328` throw）与 sidecar（`sidecar.py:783` 白名单只有 `approve_once`/`reject`）**双双被拒**，不存在会话级越权面。

3. **desktop H-2「sidecar 多一个字段就把 UI 打崩」（现状已崩的暗示）—— 部分推翻。**
   反证：我按生产端**逐字字段集**重放 20 组真实形状，17 通过 / 4 拒绝，其中 3 个拒绝是我故意注入的漂移字段；唯一非漂移的 1 例（`agent_context_reset` + `{"deleted":true}`）是我自己抄错了生产者（`{deleted}` 属于 `agent_context_delete`/`chat_session_delete` 而非 `agent_context_reset`）。**当前不存在会被拒的真实响应**，风险是「加字段即崩」。

4. **desktop C-2 中「这些通道的能力缺失等同于后端没实现」的隐含定性 —— 被修正（针对 C-1）。**
   反证：`ctest -R ssh_mock_integration` **2/2 通过**，`scripts/test_ssh_mock.py:806-890` 用真实 `yukinal-ssh-service.exe` 走完了 `auth_prompt` → `auth_response` → `ready` → `keyboard-interactive-success` 全链。即 keyboard-interactive **在服务层已实现且有测试**，缺口纯在桌面宿主的接线。这降低了 C-1 的修复成本估计。

5. **我在复核过程中被自己推翻的第一个结论（记录在案，避免误导下游）**：我最初的终端 mock 让**客户端先关** channel，得到 `{"exit_code": null}`，据此一度以为 R1 的子项 3 是错的；改成**服务器侧关** channel 后得到 `{"exit_code": 0}`。R1 的子项 3 成立。`null` 只在客户端主动断开（`exit_code` 保持 `-1` 哨兵）的路径上出现，不能作为「服务器未发 exit-status」的信号。

---

## 四、未能验证的断言

1. **native H-2 的「现场触发」：对端 RST 时进程确实被 SIGPIPE 终止。**
   缺少证据：本机 Windows 无 `signal.SIGPIPE`（`AttributeError`）；`wsl` 已安装但**没有可用的发行版**（`wsl -e bash` 失败）；`docker` 不在 `PATH`。因此只能完成静态链验证（已全部完成并确认），运行期触发**无法判定**。建议在 Linux CI 上加一个 RST 注入用例来闭环。

2. **desktop C-2 中「`activity.created` 的天然发射点是 `ipc_router.cpp:8838 record_connection_activity`」的可行性。**
   我确认了该函数存在，但**没有验证**它被执行时是否处于可安全 `sink(...)` 的线程/时机（宿主事件投递是 `webview_eval`，每帧一次）。这属于修复设计，不在本次证伪范围，标为未验证。

3. **python H-1 修复后的「等效覆盖」是否真的达到 153。**
   我验证了工作区 `native/CMakeLists.txt:237` 的 glob 已是 `test_*.py`、`check.mjs:188-196` 已加第 8 步，但**未重建 CMake 并重跑 13/13 CTest** 来实测「CTest 口径 = 153」。这是一次会改动 `native/build-nmake` 的行为，超出只读复核范围。静态证据充分（glob 语义直接），运行期确认为**未验证**。

4. **desktop H-3 中 `HostApprovalLedger::consume` 的 1 秒等待循环在真实负载下的实际阻塞时长。**
   代码事实已读（`:355-380`，`kMaxApprovalTtlSeconds = 180`），但未做并发/压测。R1 把它列为 Medium（M-1），不在本次 11 条之列，故未深挖。

5. **native H-1 的「恶意目标可稳定压制 exit-status」在真实 SSH 服务器上的概率。**
   我用 paramiko mock **证明了可能性**（真实 `yukinal-ssh-service.exe` 被成功欺骗），但**没有**在 OpenSSH/真实发行版 sshd 上复现 —— 标准 sshd 通常会发送 `exit-status`，实际可达性取决于对端是否为攻击者控制。R1 自己在 `:106` 也把这一条标为「待证实」。

---

## 附录 A —— 可重建的最小复现脚本（native H-1 模板条目）

`%TEMP%` 下的脚本不持久。下面是 **exec 路径假成功** 的完整可重建版本（终端路径把 `check_channel_exec_request` 换成 `check_channel_shell_request` + `--terminal`，发送 `exit-status` 的开关换成「删掉 `channel.send_exit_status(STATUS)`」即可得到三种变体）。

```python
# 保存为 %TEMP%\r3-exit-status-repro.py，用 agent\.venv 运行：
#   $env:PYTHONPATH="D:\yukinal\agent\src"
#   & .\agent\.venv\Scripts\python.exe %TEMP%\r3-exit-status-repro.py `
#       --service-binary "D:\yukinal\native\build-nmake\yukinal-ssh-service.exe"
from __future__ import annotations
import argparse, base64, hashlib, io, json, os, socket, subprocess, sys, tempfile, threading, time
from pathlib import Path
import paramiko
from cryptography.hazmat.primitives import serialization
from cryptography.hazmat.primitives.asymmetric import ed25519

REPO = Path(os.environ.get("YUKINAL_REPO", r"D:\yukinal"))
TOKEN = "r3-exit-status-probe-token-long-enough-for-service"
SUPPRESS_EXIT_STATUS = True   # 三种变体：True / False / 改发 0x80000000

class Server(paramiko.ServerInterface):
    def __init__(self, user_key): self.user_key, self.ready = user_key, threading.Event()
    def get_allowed_auths(self, username): return "publickey"
    def check_auth_publickey(self, username, key):
        return paramiko.AUTH_SUCCESSFUL if key == self.user_key else paramiko.AUTH_FAILED
    def check_channel_request(self, kind, chanid): return paramiko.OPEN_SUCCEEDED
    def check_channel_exec_request(self, channel, command):
        self.ready.set(); return True

def serve(listener, stop, user_key, host_key):
    while not stop.is_set():
        try: connection, _ = listener.accept()
        except (socket.timeout, OSError): continue
        try:
            transport = paramiko.Transport(connection)
            transport.add_server_key(host_key)
            server = Server(user_key); transport.start_server(server=server)
            channel = transport.accept(10)
            if channel is None: transport.close(); continue
            server.ready.wait(5)
            channel.sendall(b"probe-output\n")
            if not SUPPRESS_EXIT_STATUS: channel.send_exit_status(0)
            channel.shutdown_write(); channel.close()
            time.sleep(0.2); transport.close()
        except Exception: pass
    listener.close()

def main() -> int:
    parser = argparse.ArgumentParser(); parser.add_argument("--service-binary", required=True)
    args = parser.parse_args()
    sys.path.insert(0, str(REPO / "agent" / "src"))
    from yukinal_ssh import SshServiceClient

    def new_key():
        pem = ed25519.Ed25519PrivateKey.generate().private_bytes(
            serialization.Encoding.PEM, serialization.PrivateFormat.OpenSSH,
            serialization.NoEncryption()).decode("ascii")
        return paramiko.Ed25519Key.from_private_key(io.StringIO(pem)), pem

    user_key, user_pem = new_key(); host_key, _ = new_key()
    fingerprint = "SHA256:" + base64.b64encode(hashlib.sha256(host_key.asbytes()).digest()).decode().rstrip("=")

    listener = socket.socket(); listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    listener.bind(("127.0.0.1", 0)); listener.listen(4); listener.settimeout(0.2)
    ssh_port = listener.getsockname()[1]
    probe = socket.socket(); probe.bind(("127.0.0.1", 0)); service_port = probe.getsockname()[1]; probe.close()
    stop = threading.Event()
    threading.Thread(target=serve, args=(listener, stop, user_key, host_key), daemon=True).start()

    with tempfile.TemporaryDirectory(prefix="r3-exit-status-") as temp:
        config = Path(temp) / "service.json"
        config.write_text(json.dumps({
            "listen_port": service_port, "api_token_env": "R3_API_TOKEN",
            "targets": [{"target_id": "srv_no_exit_status", "host": "127.0.0.1", "port": ssh_port,
                         "username": "test-user", "host_key_fingerprint": fingerprint,
                         "auth": {"type": "private_key", "private_key_env": "R3_USER_KEY"},
                         "allow_server_exec": True}]}), encoding="utf-8")
        env = dict(os.environ); env["R3_API_TOKEN"] = TOKEN; env["R3_USER_KEY"] = user_pem
        service = subprocess.Popen([args.service_binary, "--config", str(config)],
                                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, env=env)
        try:
            client = SshServiceClient(TOKEN, f"http://127.0.0.1:{service_port}")
            deadline = time.monotonic() + 10; last = None
            while time.monotonic() < deadline:
                try: last = client.server_exec("srv_no_exit_status", "probe-command", 5000, 4096); break
                except Exception as error: last = error; time.sleep(0.2)
            print("=== probe result ===")
            if isinstance(last, Exception):
                print(f"client raised: {type(last).__name__}: {last}"); return 1
            print(f"exit_code={last.exit_code!r} stdout={last.stdout!r}")
            print("VERDICT: reported a successful command with exit_code=0 although the server never sent exit-status")
            return 0
        finally:
            service.terminate()
            try: service.wait(timeout=5)
            except subprocess.TimeoutExpired: service.kill()
            stop.set()
    return 0

if __name__ == "__main__": raise SystemExit(main())
```

---

## 附录 B —— 本次复核使用的其它命令（均已在正文标注期望输出）

| 用途 | 命令 |
| --- | --- |
| 门禁步数 | `node %TEMP%\r3-count-steps2.cjs scripts\check.mjs`（工作区 13 / HEAD 12） |
| 严格契约重放 | `node %TEMP%\r3-strict-probe4.mjs`（17 accepted / 4 rejected） |
| sidecar 边界 | `& .\agent\.venv\Scripts\python.exe %TEMP%\r3-python-boundary-probe.py`（H-3 `-32602`；H-3 审批决策） |
| 大退出码守卫 | `& .\agent\.venv\Scripts\python.exe %TEMP%\r3-large-exit-status-probe.py <exe> 2147483648`（502 / result_unknown） |
| 终端退出码 | `& .\agent\.venv\Scripts\python.exe %TEMP%\r3-terminal-exit-status-probe.py <exe> [with-status]`（`exit_code: 0`） |
| 键盘交互端到端 | `ctest --test-dir native/build-nmake -C Release -R "ssh_mock_integration\|agent_ssh_mock_integration" --output-on-failure`（2/2 Passed） |
| Agent 全量 | `& .\agent\.venv\Scripts\python.exe -m unittest discover -s agent\tests -p "test_*.py"`（153 OK） |
| 控制平面 | `& .\services\control-plane\.venv\Scripts\python.exe -m unittest discover -s services\control-plane\tests`（129 OK） |
| 只读性自证 | `git status --porcelain native/src native/desktop_host packages/shared/src agent/src apps/desktop/src`（空） |

**复核完成时间**：2026-10-18（Asia/Shanghai）。本报告未修改任何被跟踪文件。
