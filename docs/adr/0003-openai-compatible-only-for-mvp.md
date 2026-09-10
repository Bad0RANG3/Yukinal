# ADR 0003 MVP 只实现 OpenAI-compatible Provider

Status: Accepted
Date: 2026-09-09

## Context

Agent loop 需要一个与上游无关的模型接口，但 MVP 的目标是先获得一条完整、可测试的流式工具调用链。若同时在 loop 中加入多个原生 Provider，协议差异会扩大状态与测试面，并可能渗入权限和工具执行逻辑。

## Decision

`apps/agent/src/providers/openai-compatible.ts` 是当前唯一的 AI Provider 实现。它经由配置的 `baseUrl` 连接 OpenAI-compatible 网关，并支持两种 wire API：

- `chat`：使用 `/chat/completions`，为默认方言。
- `responses`：使用 `/responses`，供兼容 Responses API 的网关选择。

实现负责模型目录、SSE 文本增量、工具调用增量、停止取消、超时和安全错误摘要。OpenAI、OpenRouter、Ollama、LM Studio、vLLM 及内部网关只通过 URL、模型、请求头和方言配置区分；Agent loop 不根据 Provider ID 分支。

## Consequences

- 一份实现即可覆盖本地模型、公共网关与企业代理，测试路径集中。
- Provider 专属参数保留在 Provider 边界，Agent loop 只消费 `LLMProvider` 和统一的 `StreamEvent`。
- 不兼容的网关会在 Provider 边界产生明确错误，不会静默改变权限或工具语义。
- 若未来接入 Anthropic、Gemini 等原生协议，应新增 Provider 实现并保持 `LLMProvider` 不变，而非在 loop 中添加 Provider 分支。
