# R1-A：native C++ SSH 服务与公共头文件 广度审查

## 1. 审查范围、方法与总体结论

**审查范围**（只读）：`native/src/` 下的 `ssh.cpp`、`http_service.cpp`、`main.cpp`、`config.cpp`、`policy.cpp`、`host_key.cpp`、`host_certificate.cpp` 及其本地头文件 `native/src/ssh_network.hpp`、`native/src/host_key.hpp`；`native/include/yukinal/*.hpp`；`native/tests/*`、`native/CMakeLists.txt`、`native/cmake/**`。`native/desktop_host/**` 属于 task-2 范围，本审查未涉及。

**方法**：先通读 `native/CMakeLists.txt` 与 `native/include/yukinal/*.hpp` 建立全局依赖与接口约定（静态库 `yukinal_ssh_core` 由 6 个 .cpp 组成，`yukinal-ssh-service` 仅含 `main.cpp`），再逐文件完整阅读实现（不做 grep-only 结论）；对第三方依赖的关键语义（libssh2 `f216bbe3`、cpp-httplib v0.18.3、内置 OpenSSL 3.5.9）直接阅读其源码确认行为，而不是依赖文档或记忆。所有结论均给出 `file:line` 与可复核的代码片段；无法在本地复现或无法完全证明的条目显式标注「待证实」并给出置信度。

**总体结论**：代码整体成熟度显著高于同类项目——主机密钥信任（known_hosts / SHA256 指纹 / OpenSSH CA + KRL）为「三选一且必须匹配」的 fail-closed 设计，`probe_only` 与 `allow_server_exec` 双层授权、SFTP 分阶段发布（stage → fsync → 校验 → rename）、严格的 base64/整数边界检查、以及全链路 deadline 传播都做得扎实，**未发现 Critical 级缺陷**。发现 2 个 High：`result.exit_code` 语义缺陷使「未收到 exit-status」与「退出码 0」不可区分（`exit_code < 0` 守卫实际为死代码，且对超大合法退出码会误报失败），以及 Linux 上内置 OpenSSL 的 TLS 写路径缺少 `MSG_NOSIGNAL`/SIGPIPE 屏蔽导致进程可被对端 RST 杀死。Medium 包括 `service_status` 未加引号拼接服务名、`SshResources` 析构阶段的 EAGAIN 半释放问题等。大部分 Medium/Low 属于纵深防御一致性问题（引号风格不统一、缺截断标志），在当前校验器约束下不构成可直接利用的漏洞，但削弱了后续修改的安全冗余。

---

## 2. 发现清单

### Critical

无。

---

### High

#### H-1 `exit_code` 无法区分「退出码 0」与「未收到 exit-status」，`exit_code < 0` 守卫是死代码

**位置**：`native/src/ssh.cpp:2131-2140`（同型问题在 `native/src/ssh.cpp:1708-1711`）

**证据**：

```cpp
// native/src/ssh.cpp:2131
  if (result.error_code.empty()) {
    result.exit_code = libssh2_channel_get_exit_status(resources.channel_);
    if (result.exit_code < 0) {
      fail(&result, "result_unknown", "The SSH connection closed before an exit status was received");
    } else if (result.output_truncated && !preserve_truncated_output) {
```

libssh2 的实现表明该守卫的前提不成立：

```c
/* libssh2/src/channel.c:1642 */
/*
 * Return the channel's program exit status. Note that the actual protocol
 * provides the full 32-bit this function returns.  We cannot abuse it to
 * return error values in case of errors so we return a zero if channel is
 * NULL.
 */
int libssh2_channel_get_exit_status(LIBSSH2_CHANNEL *channel)
{
    if(!channel)
        return 0;

    return channel->exit_status;
}

/* libssh2/src/channel.c:1656 */
/*
 * Return 1 if the channel received an exit-status request, otherwise 0.
 * libssh2_channel_get_exit_status() cannot report this.  A server that closes
 * a channel without sending the request leaves the status at 0, the same
 * value a successful command reports.  We return a zero if channel is NULL.
 */
int libssh2_channel_has_exit_status(LIBSSH2_CHANNEL *channel)
```

`exit_status` 由 `SSH2_CALLOC` 分配并零初始化，仅在收到请求时被写入，且**无条件下发**：

```c
/* libssh2/src/packet.c:1121-1136 */
                if(SSH2_IS_LITERAL(request, len, "exit-status")) {
                    ...
                        uint32_t status = 0;
                        if(ssh2_get_u32(&buf, &status))
                            rc = ssh2_err(session, LIBSSH2_ERROR_PROTO,
                                          "exit-signal status error");
                        else
                            channelp->exit_status_received = 1;

                        channelp->exit_status = (int)status;
```

`exit_status_received`（`libssh2_priv.h:470`）是全仓库唯一能表达「是否收到」的字段，其公开访问器 `libssh2_channel_has_exit_status`（`libssh2.h:999`）在 `native/**/*.{cpp,hpp}` 中**调用次数为 0**。

**影响**：

1. **假成功（主要）**：目标服务器关闭 channel 而不发送 `exit-status` 时，`result.completed = true` 且 `exit_code = 0`。代码作者在 2134 行的错误文案（"The SSH connection closed before an exit status was received"）证明此处**本意**是要失败，但该分支永远不可达。在威胁模型包含「目标主机被入侵 / 目标是攻击者控制的 SSH 服务器」的前提下（主机密钥仅保证身份，不保证行为），恶意目标可稳定地压制 `exit-status`，使一次失败的提权/安装/重启操作在 UI 上显示为成功。
2. **假失败（次要）**：`(int)status` 的强制转换使合法的超大退出码（`status >= 0x80000000`）变为负数，这是该守卫**唯一**可达的触发路径，于是把「命令以 2147483648 退出」误报为 "The SSH connection closed before an exit status was received"，错误码为 `result_unknown`。
3. 同一缺陷的第二处：`native/src/ssh.cpp:1708-1711` 的终端会话用 `exit_code = -1` 初始化（1622 行），因此终端路径能表达「未知」（1723 行 `exit_code < 0 ? nullptr : exit_code`），但它同样用 `libssh2_channel_get_exit_status`，无法区分「未收到」与「退出码 0」，只是把未知值退化为 `0` 而不是 `null`。

**建议修复**：在两处以 `libssh2_channel_has_exit_status()` 作为前置门控：

```cpp
if (result.error_code.empty()) {
  if (!libssh2_channel_has_exit_status(resources.channel_)) {
    fail(&result, "result_unknown",
         "The SSH connection closed before an exit status was received");
  } else {
    result.exit_code = libssh2_channel_get_exit_status(resources.channel_);
    ...
  }
}
```

终端路径同理改为 `exit_code = libssh2_channel_has_exit_status(...) ? libssh2_channel_get_exit_status(...) : -1;`。同时建议在 `native/include/yukinal/ssh.hpp` 的 `ExecResult` 上增加显式契约注释，说明 `exit_code` 仅在 `completed && error_code.empty()` 时有意义。

**置信度**：高（对 libssh2 语义与死代码判定的置信度）。触发「恶意目标压制 exit-status」在真实环境中的可复现性：待证实。

---

#### H-2 Linux 上内置 OpenSSL 的 TLS 写路径未屏蔽 SIGPIPE，对端 RST 可终止进程

**位置**：`native/src/ssh.cpp:72-81`（`prevent_sigpipe` 的 Linux 空实现）、`native/src/ssh.cpp:323-338`（`tls_write_all`）、`native/src/ssh.cpp:351-400`（HTTPS 代理 TLS）、`native/src/ssh.cpp:731-732`（唯一调用点）、`native/src/host_certificate.cpp:913-976`（KRL over HTTPS 客户端）

**证据**：

```cpp
// native/src/ssh.cpp:72
bool prevent_sigpipe(Socket socket) {
#ifdef SO_NOSIGPIPE
  int enabled = 1;
  return setsockopt(socket, SOL_SOCKET, SO_NOSIGPIPE,
                    reinterpret_cast<const char*>(&enabled), sizeof(enabled)) == 0;
#else
  (void)socket;
  return true;
#endif
}
```

`SO_NOSIGPIPE` 仅存在于 macOS/BSD；Linux 依赖 `MSG_NOSIGNAL`，而该函数在 Linux 上是**空操作并返回 true**。仓库对 `MSG_NOSIGNAL` 的使用仅覆盖三处手写 `send`（`ssh.cpp:452-457`、`ssh.cpp:507-511`、`host_certificate.cpp:767-782`），且都写成 `#else 0` 的降级形式：

```cpp
// native/src/host_certificate.cpp:772
#ifdef MSG_NOSIGNAL
    constexpr int send_flags = MSG_NOSIGNAL;
#else
    constexpr int send_flags = 0;
#endif
```

libssh2 自身是安全的（`libssh2_priv.h:993-1003`，`session->flag.sigpipe` 默认 0 ⇒ 使用 `MSG_NOSIGNAL`），但内置 OpenSSL 3.5.9 **不是**：

```c
/* openssl/include/internal/sockets.h:198-202  (通用 POSIX/Linux 分支) */
#else
#define ioctlsocket(a, b, c) ioctl((a), (b), (c))
#define closesocket(s) close(s)
#define readsocket(s, b, n) read((s), (b), (n))
#define writesocket(s, b, n) write((s), (b), (n))
#endif
```

```c
/* openssl/crypto/bio/bss_sock.c:128,156 */
static int sock_write(BIO *b, const char *in, int inl)
{
    ...
        ret = writesocket(b->num, in, inl);
```

对 OpenSSL 源码全量检索 `MSG_NOSIGNAL`：**0 处命中**；亦无 `signal(SIGPIPE, ...)` 或 `sigaction`。因此 TLS 记录层最终走 `write(2)`，默认语义下向已收到 RST 的套接字写入会同步投递 SIGPIPE，默认动作是**终止进程**。

受影响路径：
- `establish_verified_proxy_tls` / `tls_write_all`（HTTPS 代理 CONNECT 隧道），`ssh.cpp:329` `SSL_write_ex` → OpenSSL BIO → `write(2)`；
- 若 libssh2 会话被挂到该 TLS 上（`ssh.cpp:922-929` 注册 `LIBSSH2_CALLBACK_SEND` → `proxy_tls_write`，`ssh.cpp:883` `SSL_write_ex`），**整个 SSH 会话的数据都经 OpenSSL 写出**，SIGPIPE 暴露面覆盖全部后续交互，而不仅是握手；
- `SocksKrlHttpsClient`（`host_certificate.cpp:913-976`）与 `load_host_revocation_list` 中的 `httplib::SSLClient`（`host_certificate.cpp:1255`），最终也走 `send_socket` → `send(..., CPPHTTPLIB_SEND_FLAGS)`，而 `CPPHTTPLIB_SEND_FLAGS` 默认为 0（`httplib.h:133-138`）。

cpp-httplib 确实会忽略 SIGPIPE，但**仅在其构造 `httplib::Server` 时**：

```cpp
// httplib.h:6086
#ifndef _WIN32
    signal(SIGPIPE, SIG_IGN);
#endif
```

```cpp
// native/src/http_service.cpp:108
  httplib::Server server;
```

于是覆盖面是：HTTP 服务模式在 `serve()` 构造 `server` 后获得全局保护；而 `--terminal` / `--sftp-download` / `--sftp-upload-exclusive` 三种 worker 模式（`native/src/main.cpp:100-115`）以及 CLI 路径**从不构造 `httplib::Server`**，进程全程无 SIGPIPE 处理。仓库内除上述 `prevent_sigpipe` 外，全量检索 `SIGPIPE|sigaction|signal(` 于 `native/src`、`native/include`：**无任何安装信号处理器的代码**。

**影响**：一个已经断开的 HTTPS 代理、或一个恶意/中间设备在对端，只需在对端重置连接后触发一次 TLS 写，即可让 Yukinal 的 SSH worker 进程或 CLI 进程被 SIGPIPE 直接杀死（信号默认动作，非可捕获的退出码）。对 worker 模式而言表现为用户可见的任务「无故消失」；对使用 HTTPS 代理的会话而言是可用性问题。注意这不是内存破坏，而是**无诊断信息的进程终止**，排障成本高。

**建议修复**：在进程入口或 `initialize_libssh2()`（`ssh.cpp:1005-1010`）中一次性、幂等地忽略 SIGPIPE，使行为不再依赖「是否恰好构造过 httplib::Server」：

```cpp
#ifndef _WIN32
  struct sigaction sa{};
  sa.sa_handler = SIG_IGN;
  sigaction(SIGPIPE, &sa, nullptr);
#endif
```

（用 `SIG_IGN` 而非空处理器，避免 `SA_RESTART` 语义差异；写失败随后由 `SSL_write_ex` 返回 `SSL_ERROR_SYSCALL` 并转成已有的 `proxy_tls_failed` 错误码，错误路径已具备。）`prevent_sigpipe` 的 `#else` 分支至少在 Linux 上应保留调用但不再声称已保护。

**置信度**：高（对 OpenSSL 与 httplib 写路径缺 `MSG_NOSIGNAL`、以及 worker 模式无信号处理器的判定）。具体现场触发：待证实。

---

### Medium

#### M-1 `service_status` 未对服务名加引号，与所有同类函数不一致

**位置**：`native/src/ssh.cpp:2195-2203`

**证据**：

```cpp
ExecResult service_status(const TargetConfig& target, const std::string& service,
                          int timeout_seconds) {
  if (!valid_service_name(service)) {
    ExecResult result;
    fail(&result, "invalid_parameters", "The read-only SSH operation parameters are invalid");
    return result;
  }
  return run_ssh_command(target, "systemctl is-active -- " + service, timeout_seconds);
}
```

对比同类实现（均先经 `shell_single_quote` 或显式单引号包裹），例如 `ssh.cpp:2360-2362`：

```cpp
  const std::string command = "docker restart --time " +
                              std::to_string(grace_period_seconds) + " -- '" + container +
                              "' 2>&1";
```

**影响**：`valid_service_name`（`native/src/policy.cpp:103-106`）为 `^[A-Za-z0-9][A-Za-z0-9_.@:-]{0,127}$`，首字符禁止 `-`，因此**当前无法**构造出选项注入或参数拆分；`systemctl` 的参数解析也不做 shell 展开。此处不构成可触发的注入。但它是全仓库唯一的例外写法：一旦校验器将来放宽（例如允许首字符为 `-`、允许 `+`/`~` 以匹配 systemd instance 名），该处会立即变成选项注入点，而其它同级调用点仍受引号保护，形成隐蔽的「同等函数、不同安全级别」。

**建议修复**：改为 `"systemctl is-active -- " + shell_single_quote(service)`，与 `ssh.cpp:3239-3250` 保持单一实现；更好的做法是把所有命令构造统一收口到一个 `quote()` 辅助函数，消除手写 `"'"` 的重复。

**置信度**：高（当前不可利用）；风险评级为中（一致性/演进风险）。

---

#### M-2 `SshResources` 析构在 deadline 之后执行，可能留下部分释放的 session

**位置**：`native/src/ssh.cpp:896-917`

**证据**：

```cpp
  ~SshResources() {
    if (agent_) {
      libssh2_agent_disconnect(agent_);
      libssh2_agent_free(agent_);
    }
    if (sftp_handle_) libssh2_sftp_close_handle(sftp_handle_);
    if (sftp_) libssh2_sftp_shutdown(sftp_);
    if (channel_) {
      libssh2_channel_set_blocking(channel_, 0);
      libssh2_channel_close(channel_);
      libssh2_channel_free(channel_);
    }
    if (known_hosts_) libssh2_knownhost_free(known_hosts_);
    if (session_) {
      libssh2_session_set_blocking(session_, 0);
      libssh2_session_free(session_);
    }
    proxy_tls_.reset();
    close_socket(socket_);
  }
```

libssh2 的释放族函数都可能返回 `LIBSSH2_ERROR_EAGAIN` 并在中途退出，且这些返回值在此**全部被忽略**：

```c
/* libssh2/src/session.c:837-859 */
static int session_free(LIBSSH2_SESSION *session)
{
    ...
    if(session->free_state == ssh2_NB_state_created) {
        /* !checksrc! disable EQUALSNULL 1 */
        while((ch = ssh2_list_first(&session->channels)) != NULL) {
            rc = ssh2_channel_free(ch);
            if(rc == LIBSSH2_ERROR_EAGAIN)
                return rc;
        }
        session->free_state = ssh2_NB_state_sent;
    }
```

```c
/* libssh2/src/channel.c:2619-2628 */
    if(!channel->local.close &&
       (session->socket_state == SSH2_SOCKET_CONNECTED)) {
        rc = ssh2_channel_close(channel);

        if(rc == LIBSSH2_ERROR_EAGAIN)
            return rc;
```

而 `ssh.cpp:906-913` 在调用前把 blocking 关掉（`libssh2_channel_set_blocking(..., 0)` / `libssh2_session_set_blocking(..., 0)`），使 `BLOCK_ADJUST`（`libssh2/src/session.h:47-59`）在 `!session->api_block_mode` 时**不再等待 socket**，于是 EAGAIN 会更直接地向上返回。

同时，此时通常已经过了操作 deadline：`set_session_timeout` 只在 `remaining_ms(deadline) > 0` 时才刷新（`ssh.cpp:812-817`），操作超时路径下 `api_timeout_ms` 已耗尽。对照组是显式操作中的 1 秒清理宽限（`ssh.cpp:2810`、`ssh.cpp:2828`、`ssh.cpp:2945`、`ssh.cpp:3076`），析构路径**没有**对应的重新布防。

**影响**：连接异常或超时后，`libssh2_session_free` 可能只释放了部分资源就返回 EAGAIN，随后 `close_socket(socket_)` 关闭 fd。残留的 session/channel 分配在进程存活期内泄漏；在 HTTP 服务模式（`httplib::ThreadPool(4)`，`http_service.cpp:113`）下这是**可累积的**：反复触发超时或恶意目标的复位，worker 可缓慢泄漏内存与句柄。严重度受限于「每个请求最多泄漏一个 session」，但缺乏上界。真实泄漏量取决于 libssh2 在 EAGAIN 时的状态机位置，需要 instrument 才能定量。

**建议修复**：析构中在释放前重设一个小的截断超时（如 `set_session_timeout(resources.session_, Clock::now() + 1s)`）后再释放，并记录非零返回值；或至少在 `LIBSSH2_ERROR_EAGAIN` 时退化为「不尝试优雅关闭，直接关闭 socket 并由内核回收」。也可以保持 blocking=1 进行释放在 1 秒预算内完成，这与其它清理路径的做法一致。

**置信度**：高（机制与代码事实）；实际泄漏规模：待证实。

---

#### M-3 多个命令构造点手写 `"'" + value + "'"`，未对撇号转义

**位置**：`native/src/ssh.cpp:2304`、`2316`、`2330`、`2343`、`2361`、`2375`、`2391`、`2422-2423`

**证据**（以 `docker_logs` 与 `docker_restart` 为例）：

```cpp
// native/src/ssh.cpp:2303
  const std::string command = "docker logs --tail " + std::to_string(tail) +
                              " --timestamps -- '" + container + "' 2>&1";
```

```cpp
// native/src/ssh.cpp:2360
  const std::string command = "docker restart --time " +
                              std::to_string(grace_period_seconds) + " -- '" + container +
                              "' 2>&1";
```

与之并存的两个正确实现是 `shell_single_quote`（`ssh.cpp:3239-3250`）与 `server_exec_operation` 内的局部 `quote` lambda（`ssh.cpp:2165-2173`），二者都把 `'` 转义为 `'\''`。

**影响**：这些位置的安全性完全依赖被插值字段的校验器不含 `'`——`valid_container_ref`（`policy.cpp:108-120`）、`valid_service_name`（`policy.cpp:103-106`）、`valid_package_ref`（`policy.cpp:126-138`）、`valid_package_version`（`policy.cpp:140-153`）当前均只允许 `[A-Za-z0-9_.+:~^-]`，确实排除 `'`。因此**当前不可注入**。风险同样在于演进：这些命令以字符串拼接方式产生，任何未来放宽（例如为容器 ID 允许 digest 形式 `sha256:...` 已通过 `:` 覆盖，但为标签允许其它字符）或新增调用点遗漏校验器，都会直接变成远程命令注入。

**建议修复**：统一改用 `shell_single_quote`；或在 `run_ssh_command` 上层引入不可绕过的参数化构造辅助（`sh_join({...})`），使「拼接」在类型层面无法直接写字符串。

**置信度**：高（当前不可利用）。

---

### Low

#### L-1 `sftp_read` 读取 `max_bytes + 1` 但不返回截断标志

**位置**：`native/src/ssh.cpp:2717`、`2740`

**证据**：

```cpp
  const std::size_t limit = static_cast<std::size_t>(max_bytes) + 1;
  ...
  result.stdout_text = base64_encode(bytes);
  result.exit_code = 0;
  result.completed = true;
```

`limit` 多读 1 字节是为了探测「是否还有更多数据」，但结果里既没有截断布尔标志，也没有 `missing_metadata` 之类的字段（`ExecResult` 定义了 `output_truncated`/`stdout_truncated`/`stderr_truncated`，见 `native/include/yukinal/ssh.hpp`）。调用方必须自行用「解码后字节数 > max_bytes」判断，跨进程边界（`desktop_host`）尤其容易遗漏。

**影响**：信息性。不会造成数据损坏（返回内容确实是文件前若干字节），但契约靠约定而非字段表达。

**建议修复**：在 `sftp_read` 中对 `bytes.size() == limit` 的情形设置 `result.output_truncated = true`，或新增 `source_truncated` 字段并写入文档。

**置信度**：高。

---

#### L-2 `sftp_list` 在超限判断前先入队一个条目

**位置**：`native/src/ssh.cpp:2575-2579`

**证据**：

```cpp
    entries.push_back({{"name", entry_name}, {"file_type", file_type}, {"size", size}});
    if (entries.size() > 4096) {
      fail(&result, "output_limit", "The remote directory contains too many entries to list safely");
      return result;
    }
```

**影响**：4096 上限实际允许暂存 4097 个条目，随后请求整体失败。纯粹是判断顺序造成的 1 项偏差，无安全影响（响应依旧失败，不返回超量数据）。

**建议修复**：把上限判断移到 `push_back` 之前（`if (entries.size() >= 4096) { fail(...); return result; }`），或在文档中明确「超过 4096 项即整体失败」。

**置信度**：高。

---

#### L-3 `sftp_write_impl` 独占写失败时「先关闭句柄再 unlink」存在窄 TOCTOU 窗口

**位置**：`native/src/ssh.cpp:2826-2832`

**证据**：

```cpp
  if (!write_succeeded && exclusive) {
    if (!set_session_timeout(resources.session_, deadline)) {
      set_session_timeout(resources.session_, Clock::now() + std::chrono::milliseconds(1000));
    }
    libssh2_sftp_unlink_ex(resources.sftp_, path.data(), static_cast<unsigned int>(path.size()));
    return result;
  }
```

句柄已在 `2815` 行 `libssh2_sftp_close_handle` 关闭，之后才 `unlink`。中间若另一写入者以独占方式创建同名路径，本端的 `unlink` 会**删掉他人的文件**。

**影响**：需要并发的第三方写入者精确落在该窗口内，且本端已判定独占写失败。实际可达性极低，但语义上「清理自己创建的文件」与「删除当前该路径的文件」并不等价。仓库其它分阶段路径（`StagingFileCleanup`，`ssh.cpp:2939-2957`）同样依赖路径而非句柄。

**建议修复**：保留句柄直到 unlink 完成（在独占创建成功后不要提前关闭），或改用「按 handle 判断后 unlink」的协议扩展（如 `posix-rename`/`hardlink` 组合已在 `sftp_publish_no_replace` 中使用）。至少应在注释中标注该窗口。

**置信度**：高。

---

#### L-4 `authorized()` 拒绝任何携带 `Origin` 的请求（预期行为，建议文档化）

**位置**：`native/src/http_service.cpp:81-90`

**证据**：

```cpp
bool authorized(const httplib::Request& request, const std::string& token) {
  if (request.has_header("Origin")) return false;
```

**影响**：这是针对浏览器发起跨源请求的 CSRF 硬化，方向正确。但它会让任何经过会注入 `Origin` 的反向代理/调试工具的合法客户端收到 401，且失败原因不透明。属于有意设计，非缺陷。

**建议修复**：在 `docs/` 或代码注释中显式记录「本 API 仅面向本机非浏览器客户端，任何带 `Origin` 的请求一律拒绝」，避免后续被误当作 bug 移除。

**置信度**：高。

---

## 3. 已排查但未发现问题的高风险区域

以下区域经过完整阅读与交叉验证，未发现缺陷；记录在此以便后续审查复查时不必重复投入：

1. **主机密钥信任选择与 fail-closed**（`ssh.cpp:1032-1148`）：KRL 加载失败即中止（1032-1042）；known_hosts / 指纹 pin / CA 三选一在配置阶段强制恰选其一（`config.cpp:487-493`）；known_hosts 路径要求 `LIBSSH2_KNOWNHOST_CHECK_MATCH`（1141），`CHECK_NOTFOUND` 不被接受；CA 校验失败不回退到其它信任来源。
2. **KRL 签名与吊销验证**（`host_certificate.cpp:499-611`、`1140-1201`）：magic/version/flags 严格校验，签名数上限 16，重复签名者拒绝，逐签名 `verify_signature` 后才使用；section 解析（439-497）对 0x20 序列拒绝 0、0x21 区间校验 low≤high 且 low>0、0x22 位图溢出保护。
3. **KRL URL 校验与传输安全**（`host_certificate.cpp:613-687`、`913-976`、`1240-1262`）：仅 https/http、禁止凭据、IPv6 带括号解析、端口范围 1-65535、长度上限 4096；HTTPS 路径 `SSL_VERIFY_PEER` + `X509_VERIFY_PARAM_set1_host`，明文 HTTP 仅允许 loopback；本地文件路径要求绝对路径、8 KiB 分块、16 MiB 上限。
4. **HTTP CONNECT 与 SOCKS5 解析边界**（`ssh.cpp:402-499`、`542-679`）：状态行解析有界（8192 上限），凭据副本在用后擦除（413-431），SOCKS5 逐字段长度校验、目标地址长度受限。
5. **`server_exec` 的工作目录 / 环境变量 / 引号**（`ssh.cpp:2157-2188` + `policy.cpp:643-732`）：`allow_server_exec` 双门控，命令长度 ≤128 KiB，workdir 必须绝对且无 `.` 分量，env 白名单，键值均经 `quote` 转义。
6. **`sftp_replace_guarded` 的分阶段发布与二次校验**（`ssh.cpp:2959-3237`）：拒绝根路径与结尾斜杠、stage 名带时间戳与序号、独占创建、fsync、staged 元数据复核、重新读取并比对摘要、发布后尺寸复核、失败路径清理。发布使用 `posix_rename` 而非覆盖式 `rename`。
7. **SFTP 报文读取的有界解析**（`ssh.cpp:3330-3412`）：所有长度字段在偏移前校验，避免越界读。
8. **严格 base64 与整数字段校验**（`policy.cpp:44-94`、`host_certificate.cpp:52-115`）：非规范尾随位被拒绝；整数范围逐字段校验。
9. **令牌比较与校验**（`http_service.cpp:45-65`）：`constant_time_equal` 定长比较，`valid_token` 限制 32-256。
10. **`server_exec` 一次性消费语义**（`http_service.cpp:153-164`）：同一 target 的 `server_exec` 只能成功一次，配合互斥量保护。
11. **DNS 解析并发上限与超时**（`ssh.cpp:115-229`）：后台解析槽位上限 4，超时后 worker 通过 `abandoned` 标志与原子计数安全退出，`freeaddrinfo` 在所有路径（含异常路径）执行。
12. **libssh2 阻塞/超时契约**（`ssh.cpp:802-817`）：`api_timeout_ms` 与 `BLOCK_ADJUST` 配合，deadline 经 `remaining_ms` 截断到 `INT_MAX`，无整数溢出。
13. **KBDINT 响应缓冲区所有权**（`ssh.cpp:1454-1547`）：`std::malloc` 交由 libssh2 以 `SSH2_SAFEFREE` 释放（`libssh2/src/userauth.c:2065-2073`），默认分配器为 `malloc/free`，匹配正确；提示数与长度均设上限。
14. **输出累积的字节记账**（`ssh.cpp:996-1003`、`2103-2113`）：按流与合并两种模式分别记账，截断标志与 `output_limit` 错误码一致。
15. **`probe_only` 门控**（`ssh.cpp:3564-3576` 及各操作内检查）：`run_operation` 层与每个操作函数层双重检查，`host_key_probe` 是唯一例外且被反向限制在 probe target 上。
16. **终端与 SFTP 流输入的队列记账**（`ssh.cpp:1420-1433`/`1637`、`1831-1834`/`1997`）：入队与出队在同一互斥量下更新字节与命令计数，上限分别为 1 MiB / 256 条与 1 MiB / 64 条，无符号下溢路径。
17. **`ExecResult` 的错误码语义映射**（`http_service.cpp:92-100`）：`already_exists`/`timeout`/`invalid_parameters`/`concurrent_change`/`replace_unsupported`/resolver 类错误均映射到合理的 HTTP 状态，其余归为 502，不会把内部错误码泄漏给客户端。
18. **符号链接与路径遍历防护**（`sftp_stat_impl`/`sftp_lstat_optional`，`ssh.cpp:2592-2688`）：`lstat` 与 `stat` 分离，`sftp_replace_guarded` 拒绝根路径与点分量；SFTP 路径交由远端解析是 OpenSSH 子系统的既有语义，本地不做二次拼接。
19. **`config.cpp` 代理配置校验**（`226-433`、`578-653`）：URL 解析、bypass 规则、主机名校验齐全，路径/URL 互斥，signer 数上限 16。
20. **构建与 CI 配置**（`native/CMakeLists.txt`、`.github/workflows/check.yml`）：三平台矩阵覆盖，静态运行时与 `/utf-8` 一致，测试目标已注册（`add_test`），但测试覆盖面见第 4 节。

---

## 4. 建议新增的测试

按优先级排序；前两项直接对应当前 High 级发现，可在不依赖真实网络的情况下用假服务器或桩实现覆盖。

1. **exit-status 缺失的失败语义**（对应 H-1）
   起一个仅接受 SSH 连接、打开 channel、回送少量 stdout 后**直接发送 CHANNEL_CLOSE 而不发送 exit-status** 的假服务端（可用 `native/tests/ssh-container/` 的 Docker 场景加一个 `ForceCommand` 包装脚本，或直接用 libssh2 的 `LIBSSH2_CALLBACK_*` 难以伪造，故推荐容器 + `Match User` 强制命令）。断言 `run_ssh_command` 返回 `completed == false` 且 `error_code == "result_unknown"`。补充一个「退出码为 0 的正常命令」用例，确认修复后仍报成功，避免把两者一起改坏。
2. **退出码边界值往返**（对应 H-1 的假失败面）
   目标端执行 `exit 255`、`exit 254`、以及（在允许的 shell 内）`exit $((2**31))` 之类的大值，断言 `exit_code` 原样回传或按 8 位截断符合预期，且**不会**被误报为 `result_unknown`。
3. **worker 模式的 SIGPIPE 存活测试**（对应 H-2）
   在 Linux 上以 `--sftp-download` 启动 worker，配置一个 HTTPS 代理指向一个「接受 TLS 握手后立即 RST」的假代理；断言进程**不是**被信号杀死（退出码非 128+13），而是通过既有 `proxy_tls_failed` 路径返回可诊断错误。同一用例在忽略 SIGPIPE 前后各跑一次，作为回归护栏。
4. **`prevent_sigpipe` 的跨平台断言**
   单元测试断言在 Linux 上进程级 SIGPIPE 处置已被设为忽略（检查 `sigaction(SIGPIPE, nullptr, &old)` 的 `old.sa_handler == SIG_IGN`），使「依赖 httplib 构造顺序」的隐式保证变成显式契约。
5. **`service_status` / 各命令构造器的注入回归**
   对 `service_status`、`docker_logs`、`docker_restart`、`systemd_inspect`、`package_install`、`server_logs` 传入包含 `'`、`;`、`$(`、反引号、`-` 前缀、空格的输入，断言全部被校验器拒绝（`invalid_parameters`），且**当校验器放宽时**新增用例能捕获到未转义拼接。建议再加一个「黄金字符串」用例，断言 `service_status("nginx")` 生成的远端命令与 `shell_single_quote` 版本逐字节一致。
6. **`sftp_read` 截断标志**
   远端准备恰好 `max_bytes` 与 `max_bytes + 1` 字节的文件，断言两者可通过返回字段区分（修复 L-1 后断言 `output_truncated`）。
7. **`SshResources` 析构的重复超时压力测试**（对应 M-2）
   连续发起 N 次必然超时的操作（例如指向一个接受 TCP 连接但从不完成 SSH 握手的假服务端），在 Linux 上用 `/proc/self/status` 的 `VmRSS` 与打开的 fd 数作为断言，确认无单调增长。这可把 M-2 从「待证实」变为可量化结论。
8. **`sftp_list` 上限边界**
   远端放置 4095 / 4096 / 4097 个条目，断言 4096 与 4097 均以 `output_limit` 失败（当前 4096 会多入队一项后失败，修复 L-2 后行为应一致且不含多余条目）。
9. **代理与 DNS 超时路径的错误码稳定性**
   覆盖 `resolve_address_list` 的 `timed_out`/`busy`/`unavailable` 三种返回，断言映射到 `timeout`/`resolver_busy`/`resolver_unavailable` 并最终得到 504/503（`http_service.cpp:98`），防止将来把三类故障合并成一个笼统错误。
10. **HTTP 层的鉴权与 CSRF 行为固化**
    对 `/v1/ssh/exec` 断言：无 `Authorization` → 401；错误 token → 401；携带 `Origin` → 401（即使 token 正确）；非 `application/json` → 415；超 2 MiB → 413/400。这些当前无测试覆盖，属于稳定契约，值得固化。

---

## 附：审查方法与可复核性说明

- 第三方源码路径：libssh2 `native/build-nmake/_deps/libssh2-src`（commit `f216bbe3aa5ec02033de53ed0809a2461fc062c7`）、cpp-httplib `native/build-nmake/_deps/cpp_httplib-src`（v0.18.3）、内置 OpenSSL 3.5.9 `native/build-nmake/yukinal_openssl-prefix/src/yukinal_openssl`。
- 所有引用行号均基于本次审查时的工作区内容；仓库 HEAD 为 `736f06a`，`git status --porcelain` 除未跟踪的 `ssh_agent_development_brief.md` 外无改动，`native/` 下的源码未做任何修改。
- 标注「待证实」的条目为：恶意目标压制 `exit-status` 的现场可复现性（H-1）、SIGPIPE 的实际触发场景（H-2）、`SshResources` 析构的实际泄漏量（M-2）。这三项均需要构造对端或运行时 instrumentation，未在本审查的只读范围内执行。
