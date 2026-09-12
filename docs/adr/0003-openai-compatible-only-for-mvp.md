# ADR 0003 只实现一个 OpenAI-compatible Provider

Status: Accepted（「只实现一个 Provider」一节已由 [ADR 0011](0011-native-anthropic-and-gemini-providers.md) 取代；「Provider 身份只在装配点分支」与「不做方言自动探测」两条仍然有效。另：成本一节「`reasoning_delta` 与 `usage` 当前实现不会产出」同样被 0011 取代 —— 两个原生适配器都会发这两个事件（thinking / thought part → `reasoning_delta`，流末 `usage`），所以「依赖 token 计费的功能暂时无法实现」不再普遍成立，只有 OpenAI-compatible 这条路确实仍然不产出；同段的「`AiProviderKind` 只有 `openai-compatible` 一个取值」也已被取代，现在是三个。原文保留，以保留当时的约束与代价）
Date: 2026-09-09

## Context

Agent loop 需要一个与上游无关的模型接口，但第一阶段的目标是先拿到一条完整可测的流式工具调用链：能流式拿到文本、能拿到工具调用、能被取消、超时不会挂死、错误不会泄漏密钥。

如果同时在 loop 里接入多个原生协议，协议差异会立刻扩大状态空间与测试面：有的把工具调用放在增量里、有的放在独立事件里，有的用 `max_tokens`、有的用 `max_output_tokens`，有的会在错误体里回显凭据。更糟的是这些差异很容易顺着调用链渗进权限判断和工具执行逻辑，而这两者都不应该知道上游是谁。

与此同时，绝大多数实际可用的端点——OpenAI 官方、OpenRouter、Ollama、LM Studio、vLLM、企业内部网关——都能说一种兼容方言。先把它做好，比先做多种协议更接近「能用」。

## Decision

`apps/agent/src/providers/openai-compatible.ts` 中的 `OpenAiCompatibleProvider` 是当前唯一的 Provider 实现。它实现 `packages/provider-sdk` 的 `LLMProvider`，把不同兼容端点收敛成 agent loop 需要的模型目录和统一的 `StreamEvent` 流。

**两种请求方言**，由配置里的 `wireApi` 选择：

| `wireApi` | 请求 | 响应解析 |
| --- | --- | --- |
| `chat`（默认） | `POST {baseUrl}/chat/completions`，body 为 `messages` + `tools` + `stream: true` | SSE `data:` 行，取 `choices[0].delta.content` 作为文本增量，按 `delta.tool_calls[].index` 累积工具名与参数片段；`[DONE]` 或流结束（EOF）时把已累积的工具调用一次性放出 |
| `responses` | `POST {baseUrl}/responses`，body 为 `input` + 展开成 `{type, name, description, parameters}` 的 `tools` + `stream: true` | SSE 事件：`response.output_text.delta` 取文本，`response.output_item.added` 建立工具槽位，`response.function_call_arguments.delta` 追加参数，`response.failed` / `response.incomplete` 转成错误事件 |

两种方言都把 `temperature` 缺省为 `0`，并把消息历史转换到各自的输入形状（`responses` 方言会把 assistant 的工具调用转成 `function_call` 项，把工具结果转成 `function_call_output` 项）。

**模型目录**通过 `GET {baseUrl}/models` 读取，用严格的 Zod schema 校验响应形状（最多 1000 个条目），失败时抛出 `ProviderError` 而不是返回半截数据。目录与方言无关：同一份结果同时服务于两种请求形状。响应体缺少 `data` 字段时视为空目录而不是错误——有些网关就是这样回答的——此时设置界面允许手动填写模型 ID，并把上一次成功读取到的目录缓存到 Provider 配置行里供模型选择器使用。

**请求边界**：

- `ChatRequest.signal` 被接到内部的 `AbortController` 上，用户按停止会真正中断在途的 HTTP 请求，而不是让流继续跑到结束。
- 超时由定时器独立触发（缺省 60 秒；宿主为一次运行传入 120 秒、为模型目录请求传入 30 秒），到期即中止请求。
- 错误信息经过 `safeProviderMessage()`：把 `api key`、`authorization`、`bearer`、`token` 形式的赋值替换成 `[redacted]`，并截断到 300 字符。上游网关在错误体里回显掩码密钥的情况是真实存在的，UI 不应该成为它的出口。
- 自定义请求头先合并进请求，如果配置了 API key，则**先删掉所有大小写形式的 `authorization`**，再由凭据库里的 key 写入 `Bearer`。自定义头无法覆盖真实密钥。

**Provider 身份不参与任何分支判断。** 目前 `AiProviderKind` 只有 `openai-compatible` 一个取值；agent loop 只依赖 `LLMProvider` 和 `StreamEvent`，不会出现「如果是某某 Provider 就换一种行为」的代码。Provider 专属参数（`baseUrl`、`wireApi`、`customHeaders`、超时）全部留在 Provider 边界内。

## Consequences

**收益**

- 一份实现覆盖本地模型、公共网关和企业代理，测试路径集中：方言解析、工具调用累积、取消、超时和错误脱敏都在同一个文件里被覆盖。
- 新增一个兼容端点只需要填 `baseUrl`、模型名和可选的 `wireApi`，不需要写代码，也不需要重新理解权限模型。
- 不兼容的网关会在 Provider 边界产生明确错误（状态码 + 简短摘要），不会静默改变工具语义或授权结果。

**成本**

- 兼容方言的差异是真实存在的：有的网关拒绝 `tool_choice`，有的不接受 `temperature: 0`，有的在流里插入空 delta。这些差异目前表现为连接失败或空回复，需要用户自己判断端点是否合格。
- `StreamEvent` 里定义了 `reasoning_delta` 和 `usage` 两个变体，但当前实现不会产出它们：这个 Provider 既不解析推理增量，也不解析 token 统计。消费方必须容忍它们缺席，而依赖 token 计费的功能暂时无法实现。
- 超时与用户取消在事件层表现不同：用户取消会得到 `done` 且 `finishReason` 为 `cancelled`，而超时会让内部中止以错误事件的形式浮出。两者的区别需要调用方自己判断。
- `GET /models` 失败即视为目录不可用。有些网关不实现该端点，用户必须手动输入模型 ID。

## Alternatives considered

- **为 OpenAI、Anthropic、Gemini 各写一个原生 Provider。** 会立刻引入三套流式与工具调用形状，而第一阶段真正的风险在权限与执行链路上，不在协议数量上。推迟。
- **引入第三方统一抽象库。** 会把上游协议差异转成第三方库的差异，并让「为什么这条流被取消」这类问题多出一层不可控的中间件。否决。
- **在 loop 里按 Provider ID 分支处理方言差异。** 会让权限与工具执行逻辑间接依赖上游协议，正是本 ADR 要避免的。否决。
- **把 `wireApi` 做成自动探测。** 探测失败时要发两次真实请求，且会让「发出去的到底是什么」变得不可解释；显式配置更重要。否决。
