# 外部验证运行手册

本文件只描述显式 opt-in 的真实端点验证。普通 `pnpm.cmd check`、`cargo test` 和默认的
Provider/MCP 测试不会访问网络，也不会下载模型、SDK 或 MCP server。

## Anthropic / Gemini

在已准备好真实凭据、模型和网络策略的机器上运行：

```powershell
$env:YUKINAL_LIVE_PROVIDER_TESTS = "1"
$env:YUKINAL_LIVE_PROVIDERS = "anthropic,gemini"
$env:ANTHROPIC_API_KEY = "..."
$env:YUKINAL_ANTHROPIC_MODEL = "..."
$env:GEMINI_API_KEY = "..."
$env:YUKINAL_GEMINI_MODEL = "..."
pnpm.cmd --filter @yukinal/agent test:live
```

可选的 `YUKINAL_ANTHROPIC_BASE_URL`、`YUKINAL_ANTHROPIC_API_VERSION`、
`YUKINAL_GEMINI_BASE_URL` 用于受控网关或版本复测。测试会覆盖真实文本流、工具调用后的结果回填、
流式文本后的取消、图片与 PDF 输入，并打印不含凭据的日期、模型、端点和 Anthropic API version 记录。

取消、超时、429、5xx 与 malformed stream 仍由离线假响应测试覆盖；真实端点不会为了制造错误而增加
付费请求。音频也不在这里加入，直到音频能力完成并被单独纳入协议验证。

## MCP 互操作矩阵

`crates/core/tests/mcp_live.rs` 只接受本机已有的命令和端点。矩阵文件中的 HTTP 凭据必须写成环境变量
名，不能把 secret 写进文件：

```powershell
$env:YUKINAL_MCP_LIVE = "1"
$env:YUKINAL_MCP_LIVE_MATRIX = "C:\path\to\mcp-live-matrix.json"
$env:MCP_LIVE_TOKEN = "..."
cargo test -p yukinal-core --test mcp_live --offline -- --test-threads=1
```

矩阵至少需要一个 stdio 和一个 Streamable HTTP server，并记录实现来源、选定版本、transport、
实际 handshake 名称/版本和工具调用结果。HTTP 客户端会按现有实现验证 JSON/SSE 回包、GET stream 与
DELETE 关闭；测试不会自动安装官方 SDK，也不会自行启动虚拟机。

示例形状：

```json
{
  "servers": [
    {
      "id": "official-ts",
      "label": "Official TypeScript SDK",
      "implementation": "official-typescript-sdk",
      "version": "pin-the-local-version",
      "transport": "stdio",
      "program": "node",
      "args": ["C:/path/to/local-server.mjs"],
      "tool": "echo",
      "arguments": {"text": "live"}
    },
    {
      "id": "independent-http",
      "label": "Independent implementation",
      "implementation": "independent-server",
      "version": "pin-the-deployed-version",
      "transport": "http",
      "url": "https://localhost.example/mcp",
      "auth_header": {"name": "Authorization", "env": "MCP_LIVE_TOKEN"},
      "tool": "echo",
      "arguments": {"text": "live"}
    }
  ]
}
```

真实运行要记录日期、模型/server 版本、认证方式、transport 和结果；没有真实凭据或 server 时不得把能力写成已验证。一次运行的结果属于验收证据，不回写成长期行为承诺。

## 没有虚拟机时怎么验证

审计（[安全与性能审计建议](./Yukinal-security-performance-audit.md)）里的验收项经常被读成「必须开一台虚拟机」，但**第一阶段（安全止血）的每一条都不需要虚机**：它们验证的是本进程派生的子进程、回环服务、纯策略函数与线协议帧，这些都能在同一台开发机上用真实进程和真实握手驱动。虚拟机真正不可替代的只有跨平台内核特性与安装包，那部分交给 CI runner，见下文。

### 本机可完成的替代（无需虚机）

| 审计验收项 | 本地替代 | 位置 |
| --- | --- | --- |
| SSH 未知 / 匹配 / 不匹配三态，且未知主机在认证前停止 | 进程内 `russh` **真实**服务器（真 key exchange、真握手、真认证），只监听回环临时端口；服务器侧记录认证回调次数，证明拒绝发生在认证之前 | `crates/ssh/tests/handshake.rs`、`crates/ssh/tests/known_hosts.rs` |
| 终端 / 文件 / SFTP / 服务器探测不再首连自动信任 | 真 `AppState`（临时目录 + 真 SQLite）+ 数据库里的身份行 → `resolve_capabilities` 断言策略为 `RequireMatch` | `apps/desktop/src-tauri/src/commands/terminal.rs` 的测试 |
| MCP 无法读取专门注入父进程的测试密钥 | 真 Node fixture 子进程回填 `process.env` 的**键名**；断言 `pnpm check` 注入的 `YUKINAL_TEST_*` 没有泄漏 | `crates/core/tests/fixtures/mcp-server.js`、`crates/core/tests/mcp_stdio.rs` |
| Provider 非本机地址强制 HTTPS、内嵌凭据被拒 | 纯函数 `validate_provider_base_url` / `providerBaseUrlRejection` 的边界表；Rust 与 TS 各一份测试 | `crates/core/src/provider.rs`、`packages/shared/src/schemas/provider.test.ts` |
| 跨源 301/302 不携带 API Key | 用假 `fetch` 断言 `redirect: "manual"`，并让 302 直接成为失败；真实 loopback 服务器用于正常路径 | `apps/agent/src/providers/openai-compatible.test.ts` |
| 绕过前端直接调用 Tauri command 无法保存非法地址 | 校验发生在命令层 `provider_save`（而非仅 schema），Rust 纯函数已有测试 | `apps/desktop/src-tauri/src/commands/provider.rs` |
| SSE 单行 / 累计与 NDJSON 单帧上限 | 生成超长 SSH 单行让它抛错；NDJSON 用 `read_frame` / `ensure_frame_size` 的有界测试 | `apps/agent/src/providers/openai-compatible.test.ts`、`crates/core/src/mcp/wire.rs`、`crates/core/src/sidecar/mod.rs` |
| 终端高输出后仍继续转发 | 广播队列的 `Lagged` 处理在 Rust 侧改为「发丢失通知并继续」；`terminal.output_lost` 契约由共享 schema 与事件词表测试钉住 | `apps/desktop/src-tauri/src/lib.rs`、`packages/shared/src/schemas/event-vocabulary.test.ts` |
| `yes` / 编译日志 / 多终端并发 | 纯本地压力：真实 PTY + 持续输出，观察是否收到 `terminal.output_lost` 而不卡死 | 手工步骤，见 `docs/development.md` |
| 数据库十万级消息搜索 | 本地生成数据集 + SQLite，不需要虚机 | 后续（阶段二/三） |

### CI runner 代替本地虚机

跨平台与内核特性无法在单机覆盖，但也不需要自己维护虚机：`.github/workflows/` 已经在 GitHub 提供的 Windows、Linux、macOS runner 上跑门禁。校园网只要能通过 HTTPS 访问 GitHub 就能使用，不需要任何本机虚拟化。

- 三平台编译、`cargo test` 与 sidecar 冒烟：`.github/workflows/check.yml`。
- 安装包构建与安装后验收：`.github/workflows/package.yml`（当前仍需按 `docs/packaging.md` 的现状补齐签名与安装记录）。
- 需要 macOS `sandbox-exec`、Linux `bubblewrap`/`Landlock`/`seccomp`、Windows Job Object 的**沙箱**验证属于第三阶段：把「选哪条策略」与「调用哪个系统原语」拆成 trait + fake，从而在本地测策略分支；系统原语本身在对应 runner 上做一次真实 smoke。

### 仍然存在的残余缺口（需要时不假装已覆盖）

- **DNS 重绑定**：保存时解析一次无法阻止「解析与连接之间」的 TOCTOU。命令层与 sidecar 目前只做地址**形状**与 scheme 策略；把域名解析并固定到 IP 的连接需要自定义 agent，列入阶段二。
- **Linux/macOS 沙箱**：本机（Windows）无法跑 `bubblewrap` / `sandbox-exec`，只能靠 CI runner 或对应平台机器。
- **系统休眠、时钟跳变、真实代理认证、Provider 限流**：需要真实外部环境，见上文的外部验证矩阵；不能由回环服务替代。
- **安装后首次启动与升级回滚**：需要干净机器或隔离 runner，不能用开发目录代替。

诚实记录这些缺口，比把「本地门禁通过」写成「已在真实验收」更有用：前者会提醒下一位维护者还需要补什么，后者会把未验证路径伪装成已支持能力。
