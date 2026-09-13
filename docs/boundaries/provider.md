# 边界：模型 Provider

`apps/agent/src/providers/` 是 Agent 与模型之间的唯一接口层；每个文件是一个协议适配器，都实现 `@yukinal/provider-sdk` 的 `LLMProvider`，把上游协议收敛成 agent loop 需要的两样东西——**模型目录**与**统一的流式事件**。Provider 的职责只有适配上游 API：不做权限判断、不执行宿主操作、不决定工具是否能被调用，也不允许上游协议细节渗进 agent loop。

| 文件 | `id` | 上游协议 |
| --- | --- | --- |
| `openai-compatible.ts` | `openai-compatible` | OpenAI Chat Completions / Responses（`wireApi` 选方言），覆盖 OpenAI、OpenRouter、Ollama、LM Studio、vLLM 与内部网关 |
| `anthropic.ts` | `anthropic` | Anthropic Messages API（`POST /v1/messages`） |
| `gemini.ts` | `gemini` | Gemini `generateContent`（`POST /v1beta/models/{model}:streamGenerateContent`） |

## 装配图

```text
agent loop
   │  只用 LLMProvider + ChatRequest + StreamEvent
   ▼
buildProvider()（唯一按 kind 分支的地方；kind 是选择器，不是适配器的配置字段）
   ├─ OpenAiCompatibleProvider ─ baseUrl · wireApi · apiKey · customHeaders · timeoutMs
   ├─ AnthropicProvider        ─ baseUrl · apiVersion · apiKey · customHeaders · timeoutMs
   └─ GeminiProvider           ─ baseUrl · apiKey · customHeaders · timeoutMs
   ▼
各自的上游端点
```

`kind`（谁翻译）与 `wireApi`（同一种翻译里的哪套方言）是**正交的两个轴**，不合并成一个扁平枚举——那会让每加一个方言都要动 `kind`（乃至数据库语义）。`wireApi` 只对 `openai-compatible` 有意义，所以另外两种 kind 带着它既不会被发送、也不会被接受：`packages/shared` 的 schema 把这种组合判为**非法输入**，而不是「被忽略的字段」。

**抽象**（`packages/provider-sdk/src/types.ts`）：

```ts
export interface LLMProvider {
  readonly id: string;
  readonly model?: string;
  listModels(): Promise<ModelInfo[]>;
  stream(request: ChatRequest): AsyncIterable<StreamEvent>;
}
```

`ChatRequest` 里有 `model`、`messages`、可选的 `tools`、`temperature`、`maxOutputTokens`，以及两个必须被认真对待的字段：`signal`（取消）与 `timeoutMs`（不许挂死）。`StreamEvent` 是判别联合：`text_delta`、`reasoning_delta`、`tool_call`、`usage`、`done`、`error`。`ProviderError` 携带 `retryable` 与可选状态码，让上层不必解析错误文本。消息在边界内侧使用中立形状 `LlmMessage`，由 Provider 负责转换成上游形状。

**硬规则：agent loop 不根据 Provider 身份分支。** 新增协议的正确做法是新增一个实现，而不是在 loop 里加 `if (provider === ...)`。

**一个 Provider 怎么被装配。** 配置来自 Rust 侧解析的 `RuntimeProviderConfig`，每次运行注入一次：

| 字段 | 含义 |
| --- | --- |
| `kind` | `openai-compatible` / `anthropic` / `gemini`——决定由哪个适配器翻译 |
| `baseUrl` | 基地址。`openai-compatible` 是完整地址（尾部 `/` 会被去掉）；两个原生 kind 填到域名层级，适配器自己接协议路径 |
| `model` | 模型 ID |
| `apiKey` | 可选。本地端点（Ollama 等）可以没有 |
| `customHeaders` | 可选。仅限非敏感的网关元数据 |
| `timeoutMs` | 可选。一次运行为 120 秒，模型目录请求为 30 秒，缺省 60 秒 |
| `wireApi` | `chat`（默认）或 `responses`。**只在 `kind: "openai-compatible"` 时允许出现** |

`anthropic` 与 `gemini` 在行里没有（或只有空白）base URL 时回落到协议自己的公开端点（`https://api.anthropic.com`、`https://generativelanguage.googleapis.com`），因为协议本身就是那家服务商的。`openai-compatible` **没有**这种回落——那个 kind 覆盖的是我们不拥有的端点，「替用户编一个 OpenAI 形状的默认地址等于把密钥送去一个他没选过的服务商」。解析与构造发生在 `apps/agent/src/rpc/router.ts` 的 `buildProvider()`：`agent.run.start` 与 `provider.models` 都要求携带这个配置，缺失、`kind` 不在契约里、或某个 kind 还没有适配器时立刻返回 `INVALID_PARAMS`——**这是构造失败，不是运行失败**。

**请求头处理顺序**（三个适配器同一条规则）：先合并 `customHeaders`；如果存在 API key，则**先删除所有大小写形式的既有凭据头**，再写入自己的那一条（`Bearer <key>` / `x-api-key` / `x-goog-api-key`）；凭据库里的 key 永远是权威来源，自定义头不能覆盖它。`anthropic` 另外无条件删掉自定义头里的 `anthropic-version`，因为版本头归适配器所有。

## 两种请求方言（只有 `openai-compatible` 有方言）

- `chat`（默认）：`POST {baseUrl}/chat/completions`，body 为 `{model, messages, tools, stream: true, temperature, max_tokens}`，`temperature` 缺省 0。文本取 `choices[0].delta.content`；工具调用按 `delta.tool_calls[].index` 建槽位，`id`、`function.name`、`function.arguments` 都是**分片到达**的，因此名称与参数片段是累加拼接出来的，`[DONE]` 时把每个非空槽位一次性产出；参数是拼完再 `JSON.parse`，解析失败时**退化为 `{raw: <原文本>}` 而不是丢弃这次调用**；流在没有 `[DONE]` 的情况下结束（连接被中途关闭）时，已累积的工具调用仍会被放出。
- `responses`：`POST {baseUrl}/responses`，`input` 是消息数组转换后的结果（`tool` 角色转成 `function_call_output` 项，带工具调用的 assistant 消息转成 `output_text` 项加 `function_call` 项），`tools` 展开成 `{type: "function", name, description, parameters}`。处理 `response.output_text.delta`、`response.output_item.added`、`response.function_call_arguments.delta`；`response.failed` 与 `response.incomplete` 产出 `error` 事件。

## 模型目录

| kind | 端点 | 过滤 |
| --- | --- | --- |
| `openai-compatible` | `GET {baseUrl}/models` | 无 |
| `anthropic` | `GET {baseUrl}/v1/models` | 无（`display_name` 作为显示名） |
| `gemini` | `GET {baseUrl}/v1beta/models` | 只保留 `supportedGenerationMethods` 含 `generateContent` 的项，并去掉名字里的 `models/` 前缀 |

`openai-compatible` 用严格 Zod schema 校验响应（`data` 为最多 1000 个 `{id}` 的数组；**`data` 缺失视为空目录**，因为部分网关就是这样回答的）；校验失败或 HTTP 状态异常时抛 `ProviderError`、不返回半截数据。目录与方言无关。设置界面通过 Tauri 命令 `provider_models` 触发一次读取，传给 Provider 的超时是 30 秒、宿主给这次 RPC 的期限是 35 秒（让 Provider 自己的超时先触发并返回可读错误）；失败时界面**不重试**，用户仍可手动填写模型 ID。返回的每个条目都填成 `{id, label, supportsToolCalling: true, supportsStreaming: true}`——端点不会告诉我们这些能力，因此这是**乐观默认值**，不是能力探测结果。

**工具名如何跨过边界。** Provider 只会看到双下划线形式的名字（例如 `docker__ps`）。转换由 `packages/provider-sdk/src/name-index.ts` 的 `createProviderNameIndex()` 完成，它接收 `ToolDeclaration[]` 并返回 `specs()`（按注册顺序生成 Provider 侧声明）、`providerFor(internalName)`（未知工具直接抛错）、`internalFor(providerName)`（未声明过的名字返回 `undefined`）。构建索引时检查两个内部名是否映射到同一个 Provider 名，命中即抛错；`system.describe` 也会报告这类冲突，而宿主在握手阶段拒绝启动存在冲突的 sidecar。因此 Provider 实现里**不应该**出现任何点号与双下划线之间的转换代码。

**取消、超时与错误。** `ChatRequest.signal` 接到内部 `AbortController` 并传给 `fetch`，用户按停止会真正中断在途请求；超时由独立定时器触发，到期即中止。用户主动取消产出 `done` 且 `finishReason` 为 `cancelled`，超时则以 `error` 事件的形式浮出——调用方需要区分这两者。上游错误文本统一经 `safeProviderMessage()`（把 `api key`、`authorization`、`bearer`、`token` 形式的赋值替换为 `[redacted]`，截断到 300 字符）。这条覆盖「Provider 自己构造的错误」：目录读取失败、`response.failed`/`response.incomplete`、Anthropic 的 `error` 事件、Gemini 的 `error`/`promptFeedback.blockReason`。**唯一没有走这条的是 `openai-compatible` 在 chat 方言下抛出的网络层异常**（catch 分支直接透传 `error.message`）——它只可能是 fetch 自身的文本，但按本条规则它也该过一遍清理。

**Anthropic（Messages API）。** 头为 `x-api-key` 与 `anthropic-version`（缺省 `2023-06-01`，可用 `config.apiVersion` 覆盖）。系统提示是**顶层 `system` 字段**（Messages API 的 `messages` 数组里没有 `system` 角色）。工具流量是内容块：assistant 的 `toolCalls` 变成 `tool_use` 块；`tool` 角色消息变成 user 轮里的 `tool_result` 块并用 `tool_use_id` 指回调用，连续的 `tool_result` 合并进同一轮。`tools` 用 `{name, description, input_schema}` 形状，面向模型的名字原样传递。**`max_tokens` 是必填字段**（省略直接 400），缺省取 4096：这一代所有 Claude 模型都接受它，更大的值在旧模型上会被拒；宁可让回复被截断成 `length` 让上层看见，也不要让请求失败。工具参数以 `input_json_delta.partial_json` 的字符串分片到达，按块下标累加后再解析；没有 `message_stop` 的截断流仍会放出已累积的工具调用。终止原因：`tool_use` → `tool_calls`，`max_tokens` → `length`，`end_turn`/`stop_sequence` → `stop`。目录 `GET {baseUrl}/v1/models`，**顶层用严格 schema**（多出未知字段说明目录格式漂移，此时抛错让界面回退到手工填写），**条目级别不严格**（模型对象正是随 API 增删字段的地方）。

**Gemini（`generateContent`）。** 密钥走 `x-goog-api-key` 头而**不是** `?key=` 查询参数（查询串会进日志）。系统提示走 `systemInstruction`，工具走 `tools[].functionDeclarations`。`functionCall` 一次到达就是完整的 `{name, args}`、没有分片拼接。**Gemini 不返回工具调用 id，只有函数名**，所以适配器用每流递增的 `gemini_call_<n>` 合成 id，并把这个不对称写在代码里：回灌结果实际是按**函数名**匹配的，因此同一回合里同名函数被调用两次是协议层面的歧义。思考模型的 part 带 `thought: true`，映射到中立的 `reasoning_delta` 而不是 `text_delta`（否则推理摘要会混进回答里）。`promptFeedback.blockReason`（没有候选的拒答）产出**不可重试**的 `error` 事件，而不是被当成传输失败。

## 哪些事件会产出、哪些不会

| 事件 | `openai-compatible` | `anthropic` | `gemini` |
| --- | --- | --- | --- |
| `text_delta` | 是 | 是 | 是 |
| `reasoning_delta` | 否，不解析推理增量 | 是，`thinking_delta` | 是，`thought: true` 的 part |
| `tool_call` | 是 | 是 | 是 |
| `usage` | 否，不解析 token 统计 | 是，`message_delta.usage` | 是，`usageMetadata` |
| `done` | 是 | 是 | 是 |
| `error` | 是 | 是 | 是 |

消费方必须容忍任何一个变体缺席。Agent loop 现在会把 `reasoning_delta` 映射为单独展示的 `agent.thinking`，并把累加的 `usage` 映射为 `agent.usage`；桌面端分别展示思考过程与输入/输出 token 累计值。

**明确未验证的假设。** 两个原生适配器都是按公开协议形状实现的，但**没有对真实上游跑过**——写它们的环境没有网络，测试注入的是假的 `fetch` 与构造好的 SSE 流。因此测试证明的是**翻译逻辑**（请求体形状、事件解析、分片拼接、终止映射、取消与脱敏），**不是协议保真度**。适配器里每一处无法离线核实的取值都用注释标成了假设（例如 `anthropic-version` 的缺省日期、`max_tokens` 的缺省值）。**第一次接真实端点时应当先跑一轮 `provider.test`。**

**怎么新增一个 Provider。** ① 只实现接口：新建实现 `LLMProvider` 的类，`stream()` 用异步生成器产出 `StreamEvent`，不要引入 Provider 专属返回类型给上层；② 把 `kind` 加进契约：`packages/shared/src/types/provider.ts` 的 `AI_PROVIDER_KINDS` 与 `packages/shared/src/schemas/provider.ts` 的 `AiProviderKindSchema`，Rust 侧 `crates/database/src/models/` 的 `AiProviderKind`，两端拼写必须逐字一致；③ 装配：在 `buildProvider()` 里按 `kind` 构造，并在 `commands/provider.rs` 的 `runtime_provider_config()` 里产出对应协议需要的字段；④ 数据库两向都走 `kind`：写入用 `kind.as_str()`，读取用 `From_db_column()`，**认不出的取值是硬错误而不是被当成 `openai-compatible`**（那正是「一个用错协议去调的 Provider」）；⑤ 补齐界面与保存路径——kind 的显示顺序、默认端点与「哪些字段有意义」的判断在 `apps/desktop/src/lib/providers.ts`，「只写适配器等于没做」；⑥ 写测试且不依赖真实网络；⑦ **不要改变这些语义**：权限模型、ToolRegistry 的票据校验、宿主工具的输入输出形状、以及工具名的映射规则。

**安全的凭证边界。** Agent 每次运行只接收一份 `RuntimeProviderConfig`；API key 是唯一允许的认证材料，必须来自操作系统凭据库，不得出现在 Provider 日志、活动记录或审计里。`customHeaders` 只允许非敏感的网关元数据，值必须非空、不超过 4096 字符、不含换行，也不能以 `Bearer ` 或 `Basic ` 开头。**当前 `provider_save` 实际上不写入自定义头**（落库时 `custom_headers` 恒为 `None`），这层允许列表用于约束历史数据与导入来源。**没有凭据引用的 Provider 只有在 `baseUrl` 指向本机时才被视为可用**（`localhost`、`127.0.0.1`、`[::1]`）；其他地址缺少密钥时会在**运行前**被判为不可用，而不是等请求失败。
