# providers/ — LLM boundary implementations

**State: empty on purpose.** `@yukinal/provider-sdk` defines `LLMProvider`.
Implementations land with the provider step.

Per ADR 0003 the MVP ships exactly one implementation:

```text
openai-compatible/   # chat completions + tool calls + streaming over an OpenAI-compatible base URL
```

which covers OpenAI, OpenRouter, Ollama, LM Studio, vLLM and company gateways via
`baseUrl`. Anthropic / Gemini native adapters are additive later and must
not introduce provider branching inside `runtime/agent-loop.ts`.

Rules for anything added here:
- No `if (providerId === "...")` outside this folder.
- Tool names are rewritten with `createProviderNameIndex` (ADR 0004) — never inline.
- `signal` from `ChatRequest` must abort the in-flight HTTP request.
- API keys arrive as `credentialRef` resolved by Rust; this process never holds one
  longer than a request.
