# 安全与数据边界

## 凭据

- 服务器密码、SSH 私钥、私钥口令和 Provider API key 只写入操作系统凭据库（macOS Keychain、Windows Credential Manager、Linux Secret Service，经由 `keyring`），SQLite 只保存 `keychain://<service>/<account>` 形式的引用（`crates/credentials`）。带口令的私钥把口令存成**第二个**凭据条目（同一个 account 加 `-passphrase` 后缀），连接时才解析。
- Provider 的 key 在 Rust 侧于使用点解析，随 `agent.run.start` 的一次性参数交给 sidecar。它不写入 Agent 配置文件、不写入日志、不写入活动记录。Agent 的进程配置只有 `YUKINAL_DATA_DIR`、`YUKINAL_LOG_LEVEL`、`YUKINAL_MAX_RUN_MS`。
- ssh-agent 是第三种认证方式，它只发送 `{method:"agent"}`，**不带任何秘密**（有一条测试专门钉住「agent 不允许夹带密码」）。口令与 agent 两条路径都没有对真实服务器跑过 —— 本环境既没有 ssh-agent 也没有可连的服务器。
- 自定义请求头只允许非敏感的网关元数据（`Referer`、`Origin`、`User-Agent`、`X-App-Name` 等 9 个名字），且值不能是 `Bearer`/`Basic` 凭据；`Authorization` 在任何情况下都会被凭据库里的 key 覆盖（`crates/core/src/provider.rs` 的 `sanitize_custom_headers`）。
- 审计输入按键名脱敏（`apiKey`、`password`、`content`、`oldString`、`newString` 等），`filesystem.read` 的文件正文不写入审计，输出摘要命中敏感标记时整体替换为「已省略」。sidecar 诊断日志在离开进程边界前会清理凭据和多行私钥块（`crates/core` 的脱敏模块与 `apps/agent/src/security/`）。

## 主机指纹

- 首次成功认证后把主机指纹按 `host:port` 记录到数据目录下的 `known_hosts`（自有格式 `v1:host:port:SHA256:…`，指纹写法与 OpenSSH 一致）；之后指纹不一致即拒绝连接，并且错误里同时给出**已钉住的**与**服务器出示的**两个指纹 —— 两个都看得见，才谈得上判断。
- 服务器编辑页提供状态、探针、信任、遗忘四个动作：探针只报告服务器出示的指纹、不写入任何东西，「信任此指纹」只有在这次会话里**真的探过**之后才可用，而不匹配时界面不提供任何「忽略 / 仍然继续」的出口。pin 按 `host:port` 关联而不是按 server id，所以同一台机器在两个条目下共用一条信任状态。
- 没有变的是**首次连接仍然默认 TOFU**：未知主机会被接受并钉住，所以生产环境应在首次连接前独立核验指纹。SSH crate 另有一条「必须匹配已知指纹」的严格策略（未钉住时会在建立 TCP 之前就拒绝），桌面端的连接路径没有启用它。

## Agent 能碰什么

- 宿主工具只接受 `host: "remote"` 且 `serverId` 以 `srv_` 开头的目标，并会核对目标环境与该服务器注册的环境是否一致、工作区是否真的挂在该服务器上；不一致直接拒绝。
- 文件工具的路径必须是绝对路径、不含控制字符，且在宿主侧按三类规则被拒绝（大小写不敏感）：路径中包含 `/.ssh/`、`/.kube/`、`/.aws/`、`/.azure/`、`/.config/gcloud/`、`/proc/`、`/run/secrets/`、`/var/run/secrets/` 之一；文件名为 `shadow`、`gshadow`、`sudoers`、`id_rsa`/`id_dsa`/`id_ecdsa`/`id_ed25519`、`.env` 与除 `.env.example`/`.env.sample`/`.env.template` 之外的 `.env.*`、`credentials`/`credentials.json`/`secrets`/`secrets.json`；或后缀为 `.pem`、`.key`、`.p12`、`.pfx`、`.jks`。这条封锁在宿主侧生效，被攻破的 sidecar 也无法绕过。
- 服务器名、日志内容、命令输出和远端文件正文都当作不可信数据：系统提示词明确要求不要把远端内容当指令，工具输出在回传模型、界面和审计之前会做敏感值清理。
- 外部动作都有上限：单帧 8 MiB、远程命令输出 4 MiB、界面文件读取 1 MiB、Agent 文件读取默认 128 KiB（上限 1 MiB）、文件写入 512 KiB、日志 120 行、服务 200 条、活动与执行审计每次最多 100 条、工具输出摘要 4000 字符、模型文本 20 万字符。
- **审计。** 每次工具执行都会由宿主写成 `tool_executions` 行，并额外生成一条 `activities` 记录。自动执行的来源会被如实记录为 `policy`、`agent` 或 `user`：Agent 自主批准不会伪装成用户批准。只有一个**终态**结果可以被落库（`pending`/`running`/`waiting_approval` 会被拒绝），所以审计里不会出现「已结束但还在跑」的行。
