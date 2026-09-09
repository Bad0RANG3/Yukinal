# ADR 0003：MVP 先实现 OpenAI-compatible Provider

Status: Accepted
Date: 2026-09-09

## Context

Agent loop 需要 Provider 无关的接口，但 MVP 只需要一条完整、可测试的流式工具调用链。把多个原生 Provider 同时塞进 loop 会扩大状态和测试面，并让协议差异渗透到权限与工具执行代码。

## Decision

`apps/agent/src/providers/openai-compatible.ts` 是当前唯一的 AI Provider 实现。它通过配置的 `baseUrl` 对接兼容 OpenAI Chat Completions 的网关，并支持两种线协议：

- `chat`：`/chat/completions`，默认方言。
- `responses`：`/responses`，用于兼容 Responses API 的网关。

实现覆盖模型列表、SSE 文本增量、工具调用增量、停止取消、超时和安全错误摘要。OpenAI、OpenRouter、Ollama、LM Studio、vLLM 及内部网关只通过 URL、模型、请求头和方言配置区分；loop 不按 Provider ID 分支。

## Consequences

- 一份 Provider 实现即可覆盖本地模型、公共网关和企业代理，测试路径集中。
- Provider-specific 参数必须留在 Provider 边界；Agent loop 只消费 `LLMProvider` 和统一的 `StreamEvent`。
- 不兼容的网关会在 Provider 边界暴露明确错误，不静默改变权限或工具语义。
- Anthropic、Gemini 等原生协议若日后加入，应新增实现并保持 `LLMProvider` 不变，而不是修改 loop 中的 Provider 分支。
