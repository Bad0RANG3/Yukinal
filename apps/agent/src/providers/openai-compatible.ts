/**
 * OpenAI-compatible provider (ADR 0003): one implementation covers OpenAI,
 * OpenRouter, Ollama, and internal gateways via `baseUrl`.
 *
 * Real HTTP + SSE only. `signal` is wired to `AbortController` so Stop truly
 * kills the in-flight request; `timeoutMs` bounds a stalled stream. Tool-name
 * rewriting happens in the loop via `createProviderNameIndex` (ADR 0004); this
 * module only moves bytes.
 */

import type { ChatRequest, FinishReason, LLMProvider, ModelInfo, StreamEvent } from "@yukinal/provider-sdk";
import { ProviderError } from "@yukinal/provider-sdk";

export interface OpenAiCompatibleConfig {
  /** Full base URL, e.g. `https://openrouter.ai/api/v1`. */
  baseUrl: string;
  model: string;
  apiKey?: string;
  customHeaders?: Record<string, string>;
  timeoutMs?: number;
}

const DEFAULT_TIMEOUT_MS = 60_000;

export class OpenAiCompatibleProvider implements LLMProvider {
  readonly id = "openai-compatible";
  readonly model: string;

  constructor(readonly config: OpenAiCompatibleConfig) {
    this.model = config.model;
  }

  async listModels(): Promise<ModelInfo[]> {
    const response = await fetch(`${this.config.baseUrl.replace(/\/$/, "")}/models`, {
      headers: this.#headers(),
      signal: AbortSignal.timeout(this.config.timeoutMs ?? DEFAULT_TIMEOUT_MS),
    });
    if (!response.ok) {
      throw new ProviderError(`listModels failed (${response.status})`, true, response.status);
    }
    const body = (await response.json()) as { data?: Array<{ id: string }> };
    return (body.data ?? []).map((entry) => ({
      id: entry.id,
      label: entry.id,
      contextWindow: undefined,
      supportsToolCalling: true,
      supportsStreaming: true,
    }));
  }

  async *stream(request: ChatRequest): AsyncIterable<StreamEvent> {
    const controller = new AbortController();
    const onParentAbort = (): void => controller.abort(request.signal?.reason);
    request.signal?.addEventListener("abort", onParentAbort, { once: true });
    const timeoutMs = request.timeoutMs ?? this.config.timeoutMs ?? DEFAULT_TIMEOUT_MS;
    const timer = setTimeout(() => controller.abort(new Error("provider stream timed out")), timeoutMs);

    try {
      const response = await fetch(`${this.config.baseUrl.replace(/\/$/, "")}/chat/completions`, {
        method: "POST",
        headers: { "content-type": "application/json", ...this.#headers() },
        body: JSON.stringify({
          model: request.model,
          messages: request.messages,
          tools: request.tools,
          stream: true,
          temperature: request.temperature ?? 0,
          max_tokens: request.maxOutputTokens,
        }),
        signal: controller.signal,
      });

      if (!response.ok || !response.body) {
        const detail = await response.text().catch(() => "");
        throw new ProviderError(
          `chat/completions failed (${response.status}): ${detail.slice(0, 300)}`,
          response.status >= 500,
          response.status,
        );
      }

      yield* this.#consumeSse(response.body);
    } catch (error) {
      if (request.signal?.aborted) {
        yield { type: "done", finishReason: "cancelled" };
        return;
      }
      if (error instanceof ProviderError) throw error;
      yield { type: "error", message: error instanceof Error ? error.message : String(error), retryable: false };
    } finally {
      clearTimeout(timer);
      request.signal?.removeEventListener("abort", onParentAbort);
    }
  }

  #headers(): Record<string, string> {
    const headers: Record<string, string> = {};
    if (this.config.apiKey) {
      headers.authorization = `Bearer ${this.config.apiKey}`;
    }
    Object.assign(headers, this.config.customHeaders ?? {});
    return headers;
  }

  /** SSE `data:` 行解析；tool_calls deltas 累积，流结束时一次性 yield 完整 ToolCall。 */
  async *#consumeSse(body: ReadableStream<Uint8Array>): AsyncGenerator<StreamEvent> {
    const reader = body.getReader();
    const decoder = new TextDecoder();
    let buffer = "";
    let lastFinishReason: string | null = null;

    const slots = new Map<string, { id: string; name: string; args: string }>();

    const toolCallEvents = (): StreamEvent[] =>
      [...slots.values()]
        .filter((slot) => slot.name)
        .map((slot) => {
          let args: Record<string, unknown> = {};
          try {
            args = slot.args ? (JSON.parse(slot.args) as Record<string, unknown>) : {};
          } catch {
            args = { raw: slot.args }; // 中断导致的未闭合 JSON：不丢原文
          }
          return { type: "tool_call" as const, call: { id: slot.id, name: slot.name, arguments: args } };
        });

    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      buffer += decoder.decode(value, { stream: true });

      let events = buffer.split("\n");
      buffer = events.pop() ?? "";

      for (const line of events) {
        const trimmed = line.trim();
        if (!trimmed.startsWith("data:")) continue;
        const payload = trimmed.slice(5).trim();
        if (payload === "[DONE]") {
          for (const event of toolCallEvents()) yield event;
          slots.clear();
          yield { type: "done", finishReason: finishReasonFor(lastFinishReason) };
          return;
        }
        let chunk: SseChunk;
        try {
          chunk = JSON.parse(payload) as SseChunk;
        } catch {
          continue; // 半行/噪音：跳过而不是炸掉流
        }
        const choice = chunk.choices?.[0];
        if (choice?.finish_reason) lastFinishReason = choice.finish_reason;
        const delta = choice?.delta;
        if (!delta) continue;

        if (typeof delta.content === "string" && delta.content.length > 0) {
          yield { type: "text_delta", text: delta.content };
        }
        for (const tool of delta.tool_calls ?? []) {
          const index = tool.index ?? 0;
          const key = String(index);
          const slot = slots.get(key) ?? { id: tool.id ?? `tc_${index}`, name: "", args: "" };
          if (tool.id) slot.id = tool.id;
          if (tool.function?.name) slot.name += tool.function.name;
          if (tool.function?.arguments) slot.args += tool.function.arguments;
          slots.set(key, slot);
        }
      }
    }
    // EOF 而没收到 [DONE]（异常结束）：把手里的工具调用放出来，避免吞掉。
    for (const event of toolCallEvents()) yield event;
  }
}

interface SseChunk {
  choices?: Array<{
    delta?: {
      content?: string | null;
      tool_calls?: Array<{
        index?: number;
        id?: string;
        function?: { name?: string; arguments?: string };
      }>;
    };
    finish_reason?: string | null;
  }>;
}

function finishReasonFor(reason: string | null): FinishReason {
  switch (reason) {
    case "tool_calls":
      return "tool_calls";
    case "length":
      return "length";
    case "cancelled":
      return "cancelled";
    default:
      return "stop";
  }
}
