# Agent Provider 边界

本目录是 Agent 与模型之间的唯一接口层。每个文件是一个协议适配器，都实现 `@yukinal/provider-sdk` 的 `LLMProvider`，把各自的上游协议收敛成 agent loop 需要的两样东西：**模型目录**和**统一的流式事件**。

| 文件 | `id` | 上游协议 |
| --- | --- | --- |
| `openai-compatible.ts` | `openai-compatible` | OpenAI Chat Completions / Responses（`wireApi` 选方言），覆盖 OpenAI、OpenRouter、Ollama、LM Studio、vLLM 与内部网关 |
| `anthropic.ts` | `anthropic` | Anthropic Messages API（`POST /v1/messages`） |
| `gemini.ts` | `gemini` | Gemini `generateContent`（`POST /v1beta/models/{model}:streamGenerateContent`） |

**状态：** 三个适配器都已实现、有离线测试，并且**已经装配完成**：`AiProviderKind` 现在是三值轴（`packages/shared/src/types/provider.ts`），`provider_configs.kind` 的写入与读取都按 kind 走（`crates/database/src/repositories/providers.rs`），设置界面能选协议，`runtime_provider_config()` 按 kind 产出各自协议的配置，`buildProvider()` 按 kind 构造。三种 kind 都能从界面配出来并跑起来。

**仍未做到的：** 两个原生适配器没有对真实上游验证过（见文末「这两个适配器的协议细节未对真实 API 验证」）；`RuntimeProviderConfig` 里没有 `apiVersion`，所以 Anthropic 的 `anthropic-version` 只能用适配器的缺省日期；也没有任何界面入口去覆盖它。这两条都是当前状态，不是待办清单上的省略描述。

Provider 的职责只有适配上游 API。它不做权限判断、不执行宿主操作、不决定工具是否能被调用，也不允许上游协议细节渗进 agent loop。

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

`kind`（谁翻译）与 `wireApi`（同一种翻译里的哪套方言）是**正交的两个轴**（ADR 0011 第 2 点）。`wireApi` 只对 `openai-compatible` 有意义，所以另外两种 kind 带着它既不会被发送、也不会被接受：`packages/shared` 的 schema 把这种组合判为非法输入（不是「被忽略的字段」），Rust 的读路径也不会给它们读出这个字段。

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

**硬规则：agent loop 不根据 Provider 身份分支。** 三种 `kind` 的差别全部留在各自适配器里；新增协议的正确做法是新增一个实现，而不是在 loop 里加 `if (provider === ...)`。

## 一个 Provider 是怎么被装配的

配置来自 Rust 侧解析出的 `RuntimeProviderConfig`（`packages/shared/src/types/provider.ts`），每次运行注入一次：

| 字段 | 含义 |
| --- | --- |
| `kind` | `openai-compatible` / `anthropic` / `gemini`——决定由哪个适配器翻译（ADR 0011） |
| `baseUrl` | 基地址。`openai-compatible` 是完整地址，例如 `https://openrouter.ai/api/v1`（尾部的 `/` 会被去掉）；两个原生 kind 填到域名层级，适配器自己接协议路径（`/v1/messages`、`/v1beta/models/{model}:…`） |
| `model` | 模型 ID |
| `apiKey` | 可选。本地端点（Ollama 等）可以没有；Rust 只在存在时传这个字段 |
| `customHeaders` | 可选。仅限非敏感的网关元数据 |
| `timeoutMs` | 可选。一次运行为 120 秒，模型目录请求为 30 秒，缺省 60 秒 |
| `wireApi` | `chat`（默认）或 `responses`。**只在 `kind: "openai-compatible"` 时允许出现**；另外两种 kind 带上它会被 schema 拒绝，而不是被忽略 |

**base URL 的缺省**：`anthropic` 与 `gemini` 在行里没有（或只有空白）base URL 时回落到协议自己的公开端点（`https://api.anthropic.com`、`https://generativelanguage.googleapis.com`），因为协议本身就是那家服务商的。`openai-compatible` **没有**这种回落：那个 kind 覆盖的是我们不拥有的端点（OpenRouter、Ollama、vLLM、内部网关……），替用户编一个 OpenAI 形状的默认地址等于把密钥送去一个他没选过的服务商。设置界面在切换协议时会把这两种默认端点填进空字段，所以正常路径下这个兜底不参与。

解析与构造发生在 `apps/agent/src/rpc/router.ts` 的 `buildProvider()`：`agent.run.start` 与 `provider.models` 都要求携带这个配置，缺失、`kind` 不在契约里、或某个 kind 还没有适配器时立刻返回 `INVALID_PARAMS`——这是构造失败，不是运行失败。

请求头的处理顺序是有意为之，三个适配器都是同一条规则、只是换成自己协议的凭据头：先合并 `customHeaders`，如果存在 API key，则**先删除所有大小写形式的既有凭据头**（`openai-compatible` 与 `gemini` 删 `authorization`，`anthropic` 删 `authorization` 与 `x-api-key`），再写入自己的那一条（`Bearer <key>` / `x-api-key` / `x-goog-api-key`）。凭据库里的 key 永远是权威来源，自定义头不能覆盖它。`anthropic` 另外无条件删掉自定义头里的 `anthropic-version`，因为版本头归适配器所有（旋钮是 `config.apiVersion`）。

## 两种请求方言（只有 `openai-compatible` 有方言）

由 `wireApi` 选择，二者都在同一个文件里实现，并用同一套 `StreamEvent` 输出。两个原生协议没有这一节，因为它们的请求与响应形状就是它们唯一的形状。

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

三种 kind 都实现了 `listModels()`，各自读**自己协议**的目录端点：

| kind | 端点 | 过滤 |
| --- | --- | --- |
| `openai-compatible` | `GET {baseUrl}/models` | 无 |
| `anthropic` | `GET {baseUrl}/v1/models` | 无（`display_name` 作为显示名） |
| `gemini` | `GET {baseUrl}/v1beta/models` | 只保留 `supportedGenerationMethods` 含 `generateContent` 的项，并去掉名字里的 `models/` 前缀 |

`openai-compatible` 用严格的 Zod schema 校验响应（`data` 为最多 1000 个 `{id}` 的数组；`data` 缺失视为空目录，因为部分网关就是这样回答的）。校验失败或 HTTP 状态异常时抛出 `ProviderError`，不返回半截数据；网络层异常会被包成 `retryable: true` 的错误。

返回的每个条目都填成 `{id, label, supportsToolCalling: true, supportsStreaming: true}`——端点不会告诉我们这些能力，因此这里是乐观默认值，调用方不应把它当作能力探测结果。目录与方言无关，`openai-compatible` 的 `chat` 与 `responses` 用的是同一个端点。

链路上的实际行为：

- 设置界面通过 Tauri 命令 `provider_models` 触发一次读取。传递到 Provider 的超时是 30 秒，宿主给这次 RPC 的期限是 35 秒，让 Provider 自己的超时先触发并返回一个可读的错误；失败时界面不重试（`retry: 0`）。
- 读取成功的结果会被缓存进 Provider 配置行（`models` 字段），供模型选择器在离线时使用。
- 目录不可用时用户仍可手动填写模型 ID，因此一个不实现 `/models` 的网关、或者目录格式漂移的上游，都仍然可用。


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
- 上游给出的错误文本统一经过 `safeProviderMessage()`：把 `api key`、`authorization`、`bearer`、`token` 形式的赋值替换成 `[redacted]`，并截断到 300 字符。上游网关在错误体里回显掩码密钥是真实存在的情况，界面不应该成为它的出口。**这条覆盖的是「Provider 自己构造的错误」**：目录读取失败、`response.failed` / `response.incomplete`、Anthropic 的 `error` 事件、Gemini 的 `error` / `promptFeedback.blockReason`、以及三个适配器 `stream()` 里被包成 `error` 事件的异常。唯一没有走这条的是 `openai-compatible` 在 chat 方言下抛出的网络层异常（`openai-compatible.ts` 的 catch 分支直接透传 `error.message`）——它只可能是 fetch 自身的文本，但按本条规则它也该过一遍清理。

## 另外两个协议怎么被适配的

两个原生适配器存在的原因是：兼容层只能覆盖请求与响应的**形状**，覆盖不了协议表达的东西。用兼容端点接 Claude 或 Gemini 意味着把同样的翻译工作推给用户去搭代理。

### `anthropic`（Messages API）

- 请求：`POST {baseUrl}/v1/messages`，头为 `x-api-key` 与 `anthropic-version`（缺省 `2023-06-01`，可用 `config.apiVersion` 覆盖，因为它按日期版本化）。**`apiVersion` 目前没有任何配置路径把它送进来**：`RuntimeProviderConfig` 里没有这个字段，Rust 也就不可能传，所以运行起来永远是那个缺省日期；要把它做成用户可配的，需要同时动 shared schema 与 `runtime_provider_config()`。
- **系统提示是顶层 `system` 字段**：Messages API 的 `messages` 数组里没有 `system` 角色。空的 system 会被省略而不是发一个空串。
- **工具流量是内容块**：assistant 的 `toolCalls` 变成 `tool_use` 块；`tool` 角色的消息变成 user 轮里的 `tool_result` 块并用 `tool_use_id` 指回调用，连续的 `tool_result` 会合并进同一轮（协议要求结果块排在内容最前面）。
- `tools` 用 `{name, description, input_schema}` 形状；面向模型的名字原样传递，不做任何改写。
- `max_tokens` 是**必填**字段（省略直接 400），缺省取 4096：这一代所有 Claude 模型都接受它，更大的值在旧模型上会被拒。宁可让回复被截断成 `length` 让上层看见，也不要让请求失败。
- 流式事件：`message_start`、`content_block_start` / `content_block_delta`（`text_delta`、`thinking_delta`、`input_json_delta`）/ `content_block_stop`、`message_delta`（终止原因与输出用量）、`message_stop`、`ping`、`error`。工具参数以 `input_json_delta.partial_json` 的**字符串分片**到达，按块下标累加后再解析。没有 `message_stop` 的截断流仍会放出已累积的工具调用。
- 终止原因：`tool_use` → `tool_calls`，`max_tokens` → `length`，`end_turn` / `stop_sequence` → `stop`。
- 目录：`GET {baseUrl}/v1/models`，顶层用严格 schema（多出未知字段说明目录格式漂移，此时抛错让界面回退到手工填写模型 ID）；条目级别不严格，因为模型对象正是随 API 增删字段的地方。

### `gemini`（`generateContent`）

- 请求：`POST {baseUrl}/v1beta/models/{model}:streamGenerateContent?alt=sse`，密钥走 `x-goog-api-key` 头而**不是** `?key=` 查询参数——查询串会进日志。
- 系统提示走 `systemInstruction`，工具走 `tools[].functionDeclarations`，其余是 `contents` 与 `generationConfig`。
- `functionCall` 一次到达就是完整的 `{name, args}`，没有分片拼接；`role: "tool"` 变成 `functionResponse` part。**Gemini 不返回工具调用 id，只有函数名**，所以适配器用每流递增的 `gemini_call_<n>` 合成 id，并把这个不对称写在代码里：回灌结果实际是按**函数名**匹配的，因此同一回合里同名函数被调用两次是协议层面的歧义。
- 思考模型的 part 带 `thought: true`，映射到中立的 `reasoning_delta` 而不是 `text_delta`——否则推理摘要会混进回答里。
- 终止：`STOP` → `stop`，`MAX_TOKENS` → `length`。`promptFeedback.blockReason`（没有候选的拒答）产出不可重试的 `error` 事件，而不是被当成传输失败。
- 目录：`GET {baseUrl}/v1beta/models`，只保留 `supportedGenerationMethods` 含 `generateContent` 的项，并去掉名字里的 `models/` 前缀。

### 这两个适配器的协议细节未对真实 API 验证

两者都是按公开协议形状实现的，但**没有对真实上游跑过**：本仓库的开发环境没有网络访问，测试注入的是假的 `fetch` 与构造好的 SSE 流。因此测试证明的是**翻译逻辑**（请求体形状、事件解析、分片拼接、终止映射、取消与脱敏），不是协议保真度。适配器里每一处无法离线核实的取值都用注释标成了假设（例如 `anthropic-version` 的缺省日期、`max_tokens` 的缺省值）。第一次接真实端点时应当先跑一轮 `provider.test`。

## 哪些事件会产出、哪些不会

`StreamEvent` 里的六个变体，三种适配器的产出情况并不相同：

| 事件 | `openai-compatible` | `anthropic` | `gemini` |
| --- | --- | --- | --- |
| `text_delta` | ✅ | ✅ | ✅ |
| `reasoning_delta` | ❌ 不解析推理增量 | ✅ `thinking_delta` | ✅ `thought: true` 的 part |
| `tool_call` | ✅ | ✅ | ✅ |
| `usage` | ❌ 不解析 token 统计 | ✅ `message_delta.usage` | ✅ `usageMetadata` |
| `done` | ✅ | ✅ | ✅ |
| `error` | ✅ | ✅ | ✅ |

消费方必须容忍任何一个变体缺席——**今天的 agent loop 就丢掉了 `reasoning_delta` 与 `usage`**（`apps/agent/src/runtime/agent-loop.ts` 的默认分支），所以「适配器产出了它」不等于「界面上能看到它」。任何依赖 token 计费或推理过程展示的功能都还不能建立在这两个事件上。

## 怎么新增一个 Provider

下面每一步都是**已经装配过一次的形状**（`anthropic` 与 `gemini` 走的正是这条路），所以它现在描述的是实际操作，而不是设想：

1. **只实现接口。** 新建一个实现 `LLMProvider` 的类，`id` 取一个稳定字符串，`stream()` 用异步生成器产出 `StreamEvent`。不要引入 Provider 专属的返回类型给上层使用。
2. **把 `kind` 加进契约。** `packages/shared/src/types/provider.ts` 的 `AI_PROVIDER_KINDS`（三处类型都从它派生）与 `packages/shared/src/schemas/provider.ts` 的 `AiProviderKindSchema`；Rust 侧 `crates/database/src/models.rs` 的 `AiProviderKind`（一个 `enum_as_str!` 调用 + 一个变体，`ALL`/`as_str`/`from_db` 由宏生成）。两端的拼写必须逐字一致，用例 `ai_provider_kind_uses_shared_wire_spelling` 守这条。
3. **装配。** 在 `apps/agent/src/rpc/router.ts` 的 `buildProvider()` 里按 `kind` 构造，并在 `commands/provider.rs` 的 `runtime_provider_config()` 里产出对应协议需要的字段（`kind`、`baseUrl`、以及该协议自己才有的键；`wireApi` 只有 `openai-compatible` 会有）。`buildProvider()` 是唯一允许按 Provider 身份分支的地方，因为它就是装配点。
4. **数据库两向都走 `kind`。** 写入用 `config.kind.as_str()`，读取用 `AiProviderKind::from_db_column()`；认不出的取值是**硬错误**（`decode_error`），不是被当成 `openai-compatible` —— 那正是「一个用错协议去调的 Provider」。`every_ai_provider_kind_round_trips_through_the_kind_column` 与 `an_unknown_provider_kind_fails_the_read_instead_of_becoming_openai_compatible` 是这两条性质的用例。
5. **补齐界面与保存路径。** 保存命令是 `provider_save`（kind 是必填参数，不再是 `provider_save_openai` 那种把「只有一种 kind」写进命令名的形式）。设置界面在 `apps/desktop/src/features/settings/RuntimeSettings.tsx`，kind 的显示顺序、默认端点与「哪些字段有意义」的判断在 `apps/desktop/src/lib/providers.ts`。新增 kind 必须同时给出这些，否则用户在设置页配不出它——只写适配器等于没做。
6. **写测试，且不依赖真实网络。** 现有测试的做法是注入假的 `fetch` 与构造好的 SSE 流，覆盖文本增量、分片工具调用、`[DONE]`、EOF 无终止符、错误脱敏与取消。两个原生适配器的测试就在本目录，可以直接照抄结构；`apps/agent/src/rpc/router.test.ts` 里每个 kind 都有一条从 `agent.run.start` 走到底的用例。
7. **不要改变这些语义：** 权限模型、ToolRegistry 的票据校验、宿主工具的输入输出形状，以及工具名的映射规则。新增 Provider 只应影响「怎么和模型说话」。

## 安全的凭证边界

Agent 每次运行只接收一份 `RuntimeProviderConfig`。API key 是唯一允许的认证材料，并且必须来自操作系统凭据库；它不写入 Agent 的配置文件，也不得出现在 Provider 日志、活动记录或审计里（`apps/agent/src/config.ts` 的日志器会对敏感键名与文本做清理）。

`customHeaders` 只允许非敏感的网关元数据：`Referer`、`Origin`、`User-Agent`、`X-App-Name`、`X-App-Version`、`X-Client-Name`、`X-Client-Version`、`X-Title`（`HTTP-Referer` 亦在允许列表内）。名字比较不区分大小写；值必须非空、不超过 4096 字符、不含换行，也不能以 `Bearer ` 或 `Basic ` 开头。`Authorization`、`X-Api-Key`、cookie 以及任何 `Bearer`/`Basic` 值都不能保存在 SQLite 或随运行配置传递；保存路径会把允许列表之外的头全部丢弃。当前 `provider_save` 实际上不写入自定义头（落库时 `custom_headers` 恒为 `None`），这层允许列表用于约束历史数据与导入来源。

没有凭据引用的 Provider 只有在 `baseUrl` 指向本机时才被视为可用（`localhost`、`127.0.0.1`、`[::1]` 的 http 或 https 形式）；其他地址缺少密钥时会在运行前被判为不可用，而不是等请求失败。

## 维护规则小结

- agent loop 只依赖 `LLMProvider` 与 `StreamEvent`，不得按 Provider ID 添加行为分支。
- 面向模型的工具名必须由 `createProviderNameIndex()` 生成，不要手写转换。
- 必须把 `ChatRequest.signal` 传到在途 HTTP 请求上，并为无响应的流建立超时。
- 错误文本在交给界面之前必须脱敏并截断。
- 相关决策记录：[ADR 0011](../../../../docs/adr/0011-native-anthropic-and-gemini-providers.md)（为什么要加原生协议、身份分支只能发生在装配点）、[ADR 0003](../../../../docs/adr/0003-openai-compatible-only-for-mvp.md)（最初的单 Provider 决定与两种方言）与 [ADR 0004](../../../../docs/adr/0004-tool-name-mapping.md)（工具名如何映射）。
