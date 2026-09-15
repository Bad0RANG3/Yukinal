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

## 已完成的一次记录（2026-09-15）

- 官方 `@modelcontextprotocol/server-everything` 包 `2026.8.31`：报告 server `mcp-servers/everything`
  `2.0.0`，Streamable HTTP，无认证；`echo` 工具调用成功。原始探针确认 POST 的 SSE 回包、GET
  `200 text/event-stream` 与 DELETE `200`。
- 官方 TypeScript SDK `1.30.0` 的 `enableJsonResponse` 临时 server：Streamable HTTP，无认证；
  JSON 回包与 `echo` 工具调用成功。
- 独立 `@ericthered926/duckduckgo-mcp-server` 包 `0.6.0`：报告 server `duckduckgo-mcp-server`
  `0.3.0`，stdio，无认证；握手与两个工具的目录读取成功，搜索调用被 DuckDuckGo 的上游反爬限制拒绝。
- 独立 `mcp-fetch-server` 包 `1.1.2`：stdio，无认证；握手与工具目录成功，访问 `example.com`
  时如实收到它自己的 SSRF 防护错误（当前代理将域名解析到 `198.18.0.78`）。这不是客户端把错误吞掉或改写成成功。

这是一组有边界的抽样记录，不代表所有第三方 server 都兼容；真实上游错误也保留在记录中。

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

真实运行后，把命令输出中的日期、模型/server 版本、认证方式、transport 和结果填回 TODO 的外部
验证记录；没有真实凭据或 server 时不要勾选完成。
