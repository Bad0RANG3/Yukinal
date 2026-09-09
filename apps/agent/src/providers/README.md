# Agent Provider 边界

当前实现是 `openai-compatible.ts` 中的 `OpenAiCompatibleProvider`。它实现 `@yukinal/provider-sdk` 的 `LLMProvider`，把不同兼容网关统一成 Agent loop 使用的模型列表和流式事件。

## 支持范围

- `GET /models`：读取可选模型目录；目录不可用时，设置页仍允许手动输入模型 ID。
- `POST /chat/completions`：默认的 Chat Completions SSE 方言。
- `POST /responses`：Responses SSE 方言，通过 `wireApi: "responses"` 选择。
- 文本增量、工具调用参数增量、完成原因、请求取消和超时。
- HTTP 错误和上游错误的安全摘要；不会把 API key 或响应中的敏感片段直接回显到 UI。

Provider 配置由 Rust 从 SQLite 和系统凭据库解析。Agent 收到的是一次运行的 `RuntimeProviderConfig`；API key 不写入 Agent 配置文件，也不由 Provider 日志记录。

## 维护约束

- Agent loop 只依赖 `LLMProvider` 和 `StreamEvent`，不得根据 Provider ID 添加分支。
- Provider-facing 工具名由 `createProviderNameIndex` 生成；不要在 Provider 或工具实现中手写点号到双下划线的转换。
- 必须把 `ChatRequest.signal` 传到在途 HTTP 请求，并为无响应的流设置超时。
- 新增原生协议时，应新增 Provider 实现和针对该方言的测试，不修改权限、ToolRegistry 或 Rust host 工具的语义。
