# 0003 — MVP 只实现一个 provider：OpenAI-compatible

Status: accepted (2026-09)

## Context
-P5 要求 provider 无关， 禁止在 core 里 `if (provider === "openai")`，
但 MVP 只需一条能跑通 的链路。

## Decision
`apps/agent/src/providers/` MVP 只有一个实现：`openai-compatible`（chat completions + tools + streaming），
`LLMProvider` 接口不变。

覆盖范围：OpenAI、OpenRouter、Ollama、LM Studio、vLLM、公司内部网关 —— 通过 `baseUrl` 区分，不是通过代码分支。
Anthropic / Gemini 原生协议适配是后续 **新增实现**，不是修改 loop。

## Consequences
- (+) 一份传输代码，一套流式/tool-call 解析逻辑，测试面最小。
- (+) 用户接私有模型是产品卖点，优先做对。
- (−) 某些 provider 的高级参数（thinking budget、prompt caching）MVP 不支持 → 通过 `customHeaders`/`settings` 预留。
- (−) 对"自称 OpenAI-compatible 但实际偏离"的网关，兼容问题会集中在这里；需要显式错误信息，不允许静默重试。
