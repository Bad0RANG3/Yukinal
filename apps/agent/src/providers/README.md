# Agent Provider 边界

本目录是 Agent 与模型之间的唯一接口层。当前的实现是 `openai-compatible.ts` 中的 `OpenAiCompatibleProvider`，它实现 `@yukinal/provider-sdk` 的 `LLMProvider`，把各种兼容端点收敛成 agent loop 需要的两样东西：**模型目录**和**统一的流式事件**。

Provider 的职责只有适配上游 API。它不做权限判断、不执行宿主操作、不决定工具是否能被调用，也不允许上游协议细节渗进 agent loop。

```text
agent loop
   │  只用 LLMProvider + ChatRequest + StreamEvent
   ▼
OpenAiCompatibleProvider
   │  baseUrl · wireApi · apiKey · customHeaders · timeoutMs
   ▼
OpenAI-compatible 端点（OpenAI / OpenRouter / Ollama / LM Studio / vLLM / 内部网关）
```

## 抽象是什么

`packages/provider-sdk/src/types.ts` 定义契约，全部内容只有一个接口和一组事件类型：

```ts
export interface LLMProvider {
  readonly id: string;
  readonly model?: string;
  listModels(): Promise<ModelInfo[]>;
  stream(request: ChatRequest): AsyncIterable<StreamEvent>;
}
```

- `ChatRequest` 里有 `model`、`messages`、可选的 `tools`、`temperature`、`maxOutputTokens`，以及两个必须被认真对待的字段：`signal`（取消）和 `timeoutMs`（不许挂死）。
- `StreamEvent` 是判别联合：`text_delta`、`reasoning_delta`、`tool_call`、`usage`、`done`、`error`。
- `ProviderError` 携带 `retryable` 与可选的状态码，让上层不必解析错误文本。

消息在边界内侧使用中立形状（`LlmMessage`：`system` / `user` / `assistant`（可带 `toolCalls`）/ `tool`（带 `toolCallId`）），由 Provider 负责转换成上游需要的形状。loop 不知道也不会去构造 `messages` 之外的方言字段。

**硬规则：agent loop 不根据 Provider 身份分支。** 目前 `AiProviderKind` 只有 `openai-compatible` 一个取值；接入新协议的正确做法是新增一个实现，而不是在 loop 里加 `if (provider === ...)`。

## 一个 OpenAI-compatible 端点是怎么被适配的

配置来自 Rust 侧解析出的 `RuntimeProviderConfig`（`packages/shared/src/types/provider.ts`），每次运行注入一次：

| 字段 | 含义 |
| --- | --- |
| `baseUrl` | 完整基地址，例如 `https://openrouter.ai/api/v1`（尾部的 `/` 会被去掉） |
| `model` | 模型 ID |
| `apiKey` | 可选。本地端点（Ollama 等）可以没有；Rust 只在存在时传这个字段 |
| `customHeaders` | 可选。仅限非敏感的网关元数据 |
| `timeoutMs` | 可选。一次运行为 120 秒，模型目录请求为 30 秒，缺省 60 秒 |
| `wireApi` | `chat`（默认）或 `responses` |

解析与构造发生在 `apps/agent/src/rpc/router.ts` 的 `buildProvider()`：`agent.run.start` 与 `provider.models` 都要求携带这个配置，缺失或 `kind` 不受支持时立刻返回 `INVALID_PARAMS`——这是构造失败，不是运行失败。

请求头的处理顺序是有意为之：先合并 `customHeaders`，如果存在 API key，则**先删除所有大小写形式的 `authorization`**，再写入 `Bearer <key>`。凭据库里的 key 永远是权威来源，自定义头不能覆盖它。

## 两种请求方言

由 `wireApi` 选择，二者都在同一个文件里实现，并用同一套 `StreamEvent` 输出。

### `chat`（默认）

- 请求：`POST {baseUrl}/chat/completions`，body 为 `{model, messages, tools, stream: true, temperature, max_tokens}`，其中 `temperature` 缺省为 `0`。
- 响应：SSE。逐行读取 `data:` 负载；`[DONE]` 结束本次流。
- 文本：取 `choices[0].delta.content`，产出 `text_delta`。
- 工具调用：按 `delta.tool_calls[].index` 建槽位，`id`、`function.name` 与 `function.arguments` 都是**分片到达**的，因此名称与参数片段是累加拼接出来的；`[DONE]` 时把每个非空槽位一次性产出为 `tool_call`。参数是拼完再 `JSON.parse`，解析失败时退化为 `{raw: <原文本>}`，而不是丢弃这次调用。
- 结束原因：取最后一次非空的 `finish_reason`，映射为 `tool_calls` / `length` / `cancelled` / 其他一律 `stop`。
- 流在没有 `[DONE]` 的情况下结束时（连接被中途关闭），已累积的工具调用仍然会被放出，避免工具调用凭空消失。

### `responses`

供兼容 Responses API 的网关使用。

- 请求：`POST {baseUrl}/responses`，body 为 `{model, input, tools, stream: true, temperature, max_output_tokens}`。这里的 `input` 是消息数组转换后的结果：`tool` 角色转成 `{type: "function_call_output", call_id, output}`，带工具调用的 assistant 消息转成 `output_text` 项加一个或多个 `function_call` 项，其余消息转成带 `input_text`/`output_text` 内容项的消息。`tools` 会被展开成 `{type: "function", name, description, parameters}`。
- 响应：SSE 事件流，处理 `response.output_text.delta`（文本）、`response.output_item.added`（为 `function_call` 建槽位）、`response.function_call_arguments.delta`（追加参数）。
- 终止事件：`response.failed` 与 `response.incomplete` 会产出 `error` 事件并结束本次流，错误信息按下面的规则脱敏。

## 模型目录怎么工作

`listModels()` 请求 `GET {baseUrl}/models`，用严格的 Zod schema 校验响应（`data` 为最多 1000 个 `{id}` 的数组；`data` 缺失视为空目录，因为部分网关就是这样回答的）。校验失败或 HTTP 状态异常时抛出 `ProviderError`，不返回半截数据；网络层异常会被包成 `retryable: true` 的错误。

返回的每个条目都填成 `{id, label: id, supportsToolCalling: true, supportsStreaming: true}`——端点不会告诉我们这些能力，因此这里是乐观默认值，调用方不应把它当作能力探测结果。目录与方言无关，`chat` 与 `responses` 用的是同一个端点。

链路上的实际行为：

- 设置界面通过 Tauri 命令 `provider_models` 触发一次读取。传递到 Provider 的超时是 30 秒，宿主给这次 RPC 的期限是 35 秒，让 Provider 自己的超时先触发并返回一个可读的错误；失败时界面不重试（`retry: 0`）。
- 读取成功的结果会被缓存进 Provider 配置行（`models` 字段），供模型选择器在离线时使用。
- 目录不可用时用户仍可手动填写模型 ID，因此一个不实现 `/models` 的网关依然可用。

## 工具名如何跨过边界

**Provider 只会看到双下划线形式的名字，例如 `docker__ps`。** 转换由 `packages/provider-sdk/src/name-index.ts` 的 `createProviderNameIndex()` 完成，它接收 `ToolDeclaration[]`，返回：

- `specs()`：按注册顺序生成的 `ProviderToolSpec[]`，交给请求的 `tools` 字段；
- `providerFor(internalName)`：`docker.ps` → `docker__ps`，未知工具直接抛错；
- `internalFor(providerName)`：`docker__ps` → `docker.ps`，未声明过的名字返回 `undefined`。

loop 用 `specs()` 构造请求，用 `internalFor()` 把模型返回的名称换回内部名称；映射不到的名称会被当作未知工具处理，不会猜。构建索引时会检查两个内部名称是否映射到同一个 Provider 名称，命中即抛错；`system.describe` 也会报告这类冲突，而宿主在握手阶段拒绝启动存在冲突的 sidecar。规则本身的定义与理由见 [ADR 0004](../../../../docs/adr/0004-tool-name-mapping.md)。

因此，Provider 实现里**不应该**出现任何点号与双下划线之间的转换代码，也不应该修改工具名。

## 取消、超时与错误

- `ChatRequest.signal` 被接到内部的 `AbortController` 上并传递给 `fetch`，用户按停止会真正中断在途请求；父信号在流结束时会解绑监听器。
- 超时由独立定时器触发（`request.timeoutMs ?? config.timeoutMs ?? 60 秒`），到期即中止。
- 用户主动取消产出 `done` 且 `finishReason` 为 `cancelled`；超时则会以 `error` 事件的形式浮出，因为它是内部中止而不是父信号取消。调用方需要区分这两者。
- 错误信息统一经过 `safeProviderMessage()`：把 `api key`、`authorization`、`bearer`、`token` 形式的赋值替换成 `[redacted]`，并截断到 300 字符。上游网关在错误体里回显掩码密钥是真实存在的情况，界面不应该成为它的出口。

## 当前实现不会产出的事件

`StreamEvent` 里定义了 `reasoning_delta` 和 `usage`，但这个 Provider 两者都不产出：它不解析推理增量，也不解析 token 统计。消费方必须容忍它们缺席；任何依赖 token 计费或推理过程展示的功能都还不能建立在这两个事件上。

## 怎么新增一个 Provider

1. **只实现接口。** 新建一个实现 `LLMProvider` 的类，`id` 取一个稳定字符串，`stream()` 用异步生成器产出 `StreamEvent`。不要引入 Provider 专属的返回类型给上层使用。
2. **导出与装配。** 在 `packages/shared/src/types/provider.ts` 的 `AiProviderKind` 与 `RuntimeProviderConfig` 里加入新的 `kind`，在 Rust 侧 `commands/provider.rs` 的 `runtime_provider_config()` 中产出对应配置，并在 `apps/agent/src/rpc/router.ts` 的 `buildProvider()` 里按 `kind` 构造——这是唯一允许按 Provider 身份分支的地方，因为它就是装配点。
3. **补齐设置界面。** `provider_save_openai` 目前只处理一种 kind；新增 kind 需要同时提供保存路径、凭据引用与模型目录读取，否则用户在设置页无法配置它。
4. **写测试，且不依赖真实网络。** 现有测试的做法是注入假的 `fetch` 与构造好的 SSE 流，覆盖文本增量、分片工具调用、`[DONE]`、EOF 无终止符、错误脱敏与取消。
5. **不要改变这些语义：** 权限模型、ToolRegistry 的票据校验、宿主工具的输入输出形状，以及工具名的映射规则。新增 Provider 只应影响「怎么和模型说话」。

## 安全的凭证边界

Agent 每次运行只接收一份 `RuntimeProviderConfig`。API key 是唯一允许的认证材料，并且必须来自操作系统凭据库；它不写入 Agent 的配置文件，也不得出现在 Provider 日志、活动记录或审计里（`apps/agent/src/config.ts` 的日志器会对敏感键名与文本做清理）。

`customHeaders` 只允许非敏感的网关元数据：`Referer`、`Origin`、`User-Agent`、`X-App-Name`、`X-App-Version`、`X-Client-Name`、`X-Client-Version`、`X-Title`（`HTTP-Referer` 亦在允许列表内）。名字比较不区分大小写；值必须非空、不超过 4096 字符、不含换行，也不能以 `Bearer ` 或 `Basic ` 开头。`Authorization`、`X-Api-Key`、cookie 以及任何 `Bearer`/`Basic` 值都不能保存在 SQLite 或随运行配置传递；保存路径会把允许列表之外的头全部丢弃。当前 `provider_save_openai` 实际上不写入自定义头，这层允许列表用于约束历史数据与导入来源。

没有凭据引用的 Provider 只有在 `baseUrl` 指向本机时才被视为可用（`localhost`、`127.0.0.1`、`[::1]` 的 http 或 https 形式）；其他地址缺少密钥时会在运行前被判为不可用，而不是等请求失败。

## 维护规则小结

- agent loop 只依赖 `LLMProvider` 与 `StreamEvent`，不得按 Provider ID 添加行为分支。
- 面向模型的工具名必须由 `createProviderNameIndex()` 生成，不要手写转换。
- 必须把 `ChatRequest.signal` 传到在途 HTTP 请求上，并为无响应的流建立超时。
- 错误文本在交给界面之前必须脱敏并截断。
- 相关决策记录：[ADR 0003](../../../../docs/adr/0003-openai-compatible-only-for-mvp.md)（为什么只有一种 Provider）与 [ADR 0004](../../../../docs/adr/0004-tool-name-mapping.md)（工具名如何映射）。
