# ADR 0011 增加原生 Anthropic 与 Gemini Provider，身份分支仍只允许发生在装配点

Status: Accepted
Date: 2026-09-12

## Context

[ADR 0003](0003-openai-compatible-only-for-mvp.md) 决定只实现一种 Provider，用两种请求方言（`chat` 与 `responses`）覆盖兼容端点，并把原生协议明确推迟。它同时定下了一条更重要的规矩：**Provider 身份不参与任何分支判断**，agent loop 只依赖 `LLMProvider` 与 `StreamEvent`。

推迟的代价现在可以直接观察到，而且不是「多一点工作量」那一类：兼容端点只能覆盖请求和响应的**形状**，覆盖不了协议本身表达的东西。

- Anthropic Messages API 把系统提示放在顶层 `system` 字段而不是一条 `system` 消息；工具调用与工具结果不是 `tool_calls` 数组，而是 `tool_use` / `tool_result` 内容块；工具参数以 `input_json_delta` 分片到达；鉴权是 `x-api-key` 加一个**按日期版本化**的 `anthropic-version` 头。
- Gemini `generateContent` 用 `systemInstruction`、`functionCall` / `functionResponse` part，流式需要 `:streamGenerateContent?alt=sse`，鉴权头是 `x-goog-api-key`，而且**不返回工具调用 id**。

这些翻译不是可选的装饰：写错它们会体现在工具调用能不能闭环、取消能不能及时生效、错误信息是不是可读上。而「用兼容端点凑」意味着用户必须自己搭一层代理，并且在代理里承担同样的翻译工作——把问题从我们的代码挪到了用户的运维里。

## Decision

1. **`AiProviderKind` 变成真正的多值轴**：`openai-compatible` | `anthropic` | `gemini`，在所有层同步：`packages/shared` 的类型与 Zod schema、Rust 的枚举与数据库读写、`runtime_provider_config()`、设置界面。
2. **`kind` 与 `wireApi` 是两个正交的轴，不合并。** `kind` 决定「由谁翻译」，`wireApi` 决定「同一种翻译里有哪套方言」（`chat` / `responses` 仍然是 OpenAI-compatible 内部的方言选择）。合并成一个扁平枚举会让每加一个方言都要动 `kind`。
3. **唯一允许按身份分支的地方仍然是装配点**：`apps/agent/src/rpc/router.ts` 的 `buildProvider()`。agent loop 继续只认 `LLMProvider` 与 `StreamEvent`，不认识任何 Provider 名字（ADR 0003 的这条规矩不变，也不会因为新增实现而放宽）。
4. **凭据边界不变。** 密钥只从操作系统凭据库进入 Rust，随每次运行的 `RuntimeProviderConfig` 交给 sidecar；适配器只在自己的请求头上使用它，不记录、不回显、不写盘。调用方传入的 `authorization` / `x-api-key` 变体在设置自己的凭据头之前一律删除（沿用 OpenAI-compatible 适配器的做法）。任何可能到达界面或审计的错误文本都要先脱敏并截断。
5. **模型目录按各自协议读取**：Anthropic `GET /v1/models`，Gemini `GET /v1beta/models` 且只保留 `supportedGenerationMethods` 含 `generateContent` 的项。目录读取失败**不阻止**运行：用户始终可以手填 model id，这条与今天的行为一致。
6. **保存与配置路径必须按 `kind` 分派。** 一个只能保存一种 kind 的设置页等于「实现了一个用户配不出来的 Provider」，所以界面必须能选 kind，保存路径必须把 kind 落库并在读回时**真正读出来**。
7. **适配器可以产出 `usage` 与 `reasoning_delta`。** 协议本身提供用量与思考增量，而 `StreamEvent` 已经定义了这两个变体。消费方必须继续容忍它们缺席（OpenAI-compatible 适配器仍然两者都不产出）；`agent loop` 目前把两者丢弃，是否上屏是界面的独立决定，不影响本记录。

## Consequences

**收益**

- 原生协议的工具调用、流式与取消按协议本身实现，不再依赖用户自建代理；错误信息带着真实的 HTTP 状态与协议事件，而不是「兼容端点表现了某种差异」。
- 用量与思考增量第一次成为可能（兼容端点拿不到它们）。
- 三种 kind 各自独立实现，任何一个是坏的时候，另外两个不受影响——这正是 ADR 0003 想要的那种隔离，只是现在有三个而不是一个。

**成本**

- 三套方言要一起维护，而且其中两套有会过期的版本化头（`anthropic-version` 是日期字符串）。升级不是可选项，是一次必须跟随的维护动作。
- Gemini 不返回工具调用 id：回灌 `tool_result` 只能按函数名匹配，因此**同一个回合里同名函数被调用两次**是协议层面的歧义。实现里必须把这个不对称写在代码注释与工具结果里，而不是假装有 id。
- 新增两类真实失败模式：Gemini 的 `promptFeedback.blockReason`（没有候选，属于拒答而不是传输错误）、缺少终止事件的截断流（`message_stop` 没到达时必须仍然给出终态，不能让运行挂住）。
- `AiProviderKind` 从单值变多值会暴露一处既有的隐式错误：数据库读路径目前**忽略** `kind` 列并硬编码 `OpenaiCompatible`，写入路径也把 `'openai-compatible'` 写死在 SQL 里。不修这一处，新 kind 存进去会被读成 openai-compatible——所以它属于本次改动的一部分，而不是后续优化。
- 设置界面变复杂：多一个 kind 选择、每种 kind 的字段不同、每种 kind 的目录读取失败原因不同。

## Alternatives considered

- **继续只用兼容端点。** 用户要为每个原生服务自建一层代理并承担同样的翻译工作；工具调用闭环、取消与思考增量都拿不到。否决。
- **只做 Anthropic，Gemini 以后再说。** 两套翻译逻辑不同但同构（系统指令、工具 part、SSE 分片、无 id），一次做两个比分两次做便宜；而且「另一种 kind」这件事本身需要先被证明架构撑得住。否决。
- **引入统一的第三方多 Provider 抽象库。** 会把权限、命名映射与错误脱敏这些属于我们的决定交给库的形状，也会让「工具名怎么变成 `docker__ps`」这条 ADR 0004 的规矩被绕过。ADR 0003 已经否决过一次，理由不变。
- **把 `kind` 与 `wireApi` 合并成一个扁平枚举。** 两者描述的是不同的问题（谁翻译 / 怎么翻译），合并后每加一个方言都要动 kind 乃至数据库语义。否决。
- **自动探测 dialect / kind。** 探测本身要花真实请求，而且会让「到底发出去了什么」无法从配置解释——这条 ADR 0003 否决过，新增原生协议不改变这个判断。
