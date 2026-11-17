# SSH 安全与终端验收（2026-11）

## 实际调用链

仓库已删除 Tauri/russh。当前链路是 Python `ToolRegistry` → `ssh_tools` → 宿主目标/审批 RPC → 单目标 C++ 服务 HTTP → libssh2。桌面手动终端是 xterm.js → 本地 `terminal_write` IPC → 独立 C++ `--terminal` worker → libssh2；Agent 的 `host.*` RPC 白名单不提供该终端入口。下面的原生安全修复针对这条现存链路。

## 清单映射

| 项目 | 改动和边界 | 主要文件 |
| --- | --- | --- |
| 1 参数注入 | 结构化 JSON；C++ shell 变量统一 `shell_quote`；完整审批脚本只作为 `sh -c` 的单个引号参数 | `agent/src/yukinal_ssh/client.py`、`native/src/ssh.cpp`、`native/include/yukinal/ssh_security.hpp` |
| 2 host 选项 | caller、连接表单/schema、宿主与原生 config 均拒绝前导 `-`；当前无 ssh/scp 二进制调用 | `client.py`、`desktop_target.cpp`、`config.cpp`、共享 server schema |
| 3 文件范围 | SFTP 同连接 realpath + 配置根边界；先检查词法范围再探测；创建目标验证父目录，拒绝 dangling symlink/socket | `ssh.cpp`、`desktop_target.cpp`、server 配置/迁移 |
| 4/6 AI PTY | 禁止 AI stdin，无法获得会话级永久授权；用户帧由宿主固定标记 `user_pty`，worker 拒绝缺失或其他来源 | `desktop_terminal.cpp`、`ssh.cpp` |
| 5 输出污染 | 成功/失败 ToolMessage、历史摘要、goal review 统一清洗截断，并带 `[[UNTRUSTED_REMOTE_OUTPUT]]` 边界；外部证据后的变更不自动批准 | `untrusted_output.py`、`model_agent.py`、`model_context.py`、`conversation_store.py`、`goal_loop.py`、`desktop_approval.cpp` |
| 7 ANSI | 原生和 xterm 之前的有状态 SGR 白名单处理跨帧 CSI/OSC/DCS/C1；Markdown 清除 ANSI | `ssh_security.hpp`、`terminal-safety.ts`、`TerminalPane.tsx`、`MarkdownText.tsx` |
| 8 生命周期 | 每次请求有一次性 execution ID、绝对截止时间、取消端点；提前取消留 tombstone；宿主 ticket 的原始期限传入服务；通道 watchdog 关闭连接 | `client.py`、`ssh_service_process.py`、`http_service.cpp`、`ssh.cpp`、`desktop_approval.cpp`、`ipc_router.cpp` |
| 9 checkpoint | verifier 包含 scope、目标、工具、budget、goal mode；私有持久 HMAC key 绑定完整 SSH 配置、凭据和证书文件内容，审批等待后再次复核 | `checkpoints.py`、`sidecar.py`、`model_agent.py`、`ssh_tools.py` |
| 10 确认回合 | Registry 拒绝变更，服务端确认只读配置拒绝写操作；AI 无 PTY 复用入口；原生宿主拒绝后续自动 ticket | `tools.py`、`policy.cpp`、`desktop_approval.cpp` |
| 11 凭据 | Python bytearray 清零、临时 env 清除、原生 RAII 清理；异常固定文本；JSONL sink 递归脱敏 | `ssh_service_process.py`、`ssh_tools.py`、`audit.py`、`scoped_secret_cleanup.hpp` |
| 12 字节审计 | libssh2 实际写入边界独立审计来源 `agent_tool/user_pty/system`、目标、session/execution、长度；非交互数据另记 SHA-256，PTY 按键只记元数据；审计失败拒绝写出 | `ssh.cpp`、`desktop_target.cpp` |
| 13 连接复用 | libssh2 不读取 OpenSSH ControlMaster 配置、不运行本地 ssh/scp；拒绝显式复用/socket 命令，SFTP 拒绝 socket | `shell_policy.py`、`policy.cpp`、`ssh.cpp` |
| 14–16 色彩/尺寸 | `xterm-256color`、可选 `COLORTERM=truecolor`；完整明暗 16 色 ITheme；无连字等宽字体；窗口/容器/字体变化重新 fit，并传远端 window-change | `ssh.cpp`、`TerminalPane.tsx`、server 配置、`providers.css` |

新增分支的审查规则写在根目录 [AGENTS.md](../AGENTS.md) 和 PR 模板，必须同时审查 Python 与原生执行端。

## 兼容性和限制

- SFTP 根默认是 SSH 用户 home（配置 `.`），不再默认暴露 `/`。需要 `/etc` 等目录时，由操作者在连接配置的允许根中明确加入；文件浏览器从首个根打开。任意已批准的 `server.exec` 仍是远程 shell，不等同于 SFTP 文件沙箱。
- 安全终端仅允许 SGR 和普通文本/换行，保留 16/256/truecolor 着色；vim/top/全屏 TUI、清屏、光标移动、OSC 标题/超链接等功能被有意阻断。目标端需允许 `AcceptEnv COLORTERM`；使用 OpenSSH 客户端时对应 `SendEnv COLORTERM`。libssh2 直接发送 env request，不读取客户端 ssh_config。
- AI PTY 写入被禁用。未来如提供它，必须先实现每次输入独立、绑定具体 ChangePlan 步骤的原生票据，不能仅添加一个 `agent_tool` 来源字段。
- checkpoint 绑定覆盖完整连接配置和密码/私钥/口令的 HMAC，不落盘明文凭据；外部 SSH agent 只能绑定其 socket/模式，不能证明外部进程中的密钥未更换，因此禁止跨 sidecar 生命周期恢复。没有宿主身份解析器的独立 HTTP 服务也禁止复用旧 checkpoint。
- CPython 不可变字符串和库内部临时副本无法可靠原地清零；当前实现主动擦除自有可变 secret buffer、清除映射引用并缩短子进程环境生命周期，不能声称从整个进程内存抹除了所有副本。
- 字节审计不保存原始命令或按键。PTY 按键连单字节摘要也不记录，避免用枚举恢复交互密码；来源、会话/执行 ID 与长度可关联工具/会话记录，但不支持重放原始字节流。
- 关闭通道可以阻止待执行/后续写入，不能回滚已经被远端接收的效果，也不能保证终止远端主动脱离会话的后台进程；超时结果按未知处理。
- SFTP realpath 与后续操作不是服务端 chroot，仍存在远端文件系统并发变化窗口；真实恶意 SSH 服务器本身不能由客户端可信化。ControlMaster 静态拒绝规则是纵深防御，不是任意脚本沙箱。

## 验证

专项回归包含：前导 `-` 主机、分号/引号/反引号/换行参数、`../` 与 symlink 越界、伪造输出边界、提示注入文本、旧历史/目标复核输出、二次 AI PTY 输入、ticket 回放/过期、执行中取消、OSC/DCS/清屏跨帧、256/truecolor、Markdown ANSI、人工输入来源及远端窗口尺寸。

Windows 本地完整 `pnpm check` 已通过（2026-11-18，Python 3.11.17、MSVC 2022）：

- 原生 CTest：17/17，包含真实 libssh2 服务与 SSH mock、Agent → 原生服务集成、桌面审批/取消与终端 worker。
- Python Agent：388/388；控制平面 SQLite：157/157。
- 桌面前端：650/650；共享包：148/148；provider SDK：4/4；agent SDK：6/6。
- TypeScript 类型检查、桌面生产构建、Python sidecar smoke、仓库规范、秘密扫描和文档链接检查全部通过；`git diff --check` 通过。

跨平台复核另补充 POSIX 文件打开失败时的描述符清理测试，SSH 凭据边界专项 11/11 通过；使用可移植的线程停止/回收逻辑兼容当前 macOS 发布工具链。

本地没有 Docker，未将 mock 结果计作真实 OpenSSH 验收。Ubuntu CI 启用了 OpenSSH 容器测试，覆盖 `ls --color`、`git status`/`git diff`、256/truecolor、COLORTERM、窗口尺寸和安全拒绝用例；Windows、macOS Intel CI 同样运行完整门禁。远端 CI 的实测结果以该提交的 GitHub Actions 记录为准。
