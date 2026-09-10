# Agent Provider 边界

当前的 Provider 实现在 `openai-compatible.ts` 中，类名为 `OpenAiCompatibleProvider`。它实现 `@yukinal/provider-sdk` 的 `LLMProvider`，将不同的兼容端点收敛成 Agent loop 所需的模型目录和统一流式事件。

Provider 的职责是适配上游 API；它不决定工具权限、不会执行宿主操作，也不应让上游协议细节渗入 Agent loop。

## 支持的协议能力

- 通过 `GET /models` 读取模型目录。目录不可用时，设置界面仍允许手动填写模型 ID。
- 通过 `POST /chat/completions` 使用默认的 Chat Completions SSE 方言。
- 通过 `POST /responses` 使用 Responses SSE 方言，由 `wireApi: "responses"` 选择。
- 处理文本增量、工具调用参数增量、完成原因、请求取消和超时。
- 将 HTTP 或上游错误整理为可安全显示的摘要，避免在 UI 中回显 API key 或响应中的敏感片段。

Rust 从 SQLite 和系统凭据库解析 Provider 配置。Agent 每次运行只接收一份 `RuntimeProviderConfig`；API key 不写入 Agent 的配置文件，也不得出现在 Provider 日志中。

API key 是唯一允许的认证材料，并且必须来自 OS 凭据库。`customHeaders` 只允许 `Referer`、`Origin`、`User-Agent` 与应用标识等非敏感网关元数据；`Authorization`、`X-Api-Key`、cookie、Bearer/Basic 值及其他凭据不能保存在 SQLite 或随运行配置传递。

## 维护规则

- Agent loop 只依赖 `LLMProvider` 和 `StreamEvent`，不得根据 Provider ID 添加行为分支。
- Provider 面向模型的工具名必须由 `createProviderNameIndex` 生成。不要在 Provider 或工具实现中手写点号与双下划线之间的转换。
- 必须将 `ChatRequest.signal` 传给在途 HTTP 请求，并为无响应的流建立超时处理。
- 添加新的原生 API 时，应新增对应 Provider 实现和该方言的测试；权限模型、ToolRegistry 和 Rust 宿主工具的语义不应因此改变。

有关工具命名的可逆映射，参见 [ADR 0004](../../../../docs/adr/0004-tool-name-mapping.md)；Provider 范围的选择，参见 [ADR 0003](../../../../docs/adr/0003-openai-compatible-only-for-mvp.md)。
