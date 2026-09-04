/**
 * OpenAI-compatible provider (ADR 0003): one implementation covers OpenAI,
 * OpenRouter, Ollama, codex-style gateways, and internal proxies via `baseUrl`.
 *
 * Real HTTP + SSE only. `signal` is wired to `AbortController` so Stop truly
 * kills the in-flight request; `timeoutMs` bounds a stalled stream. Two dialects:
 * chat completions (default) and the codex `responses` API (CC Switch imports).
 * Tool-name rewriting happens in the loop via `createProviderNameIndex` (ADR 0004).
 */

import type { ChatRequest, FinishReason, LLMProvider, LlmMessage, ModelInfo, StreamEvent } from "@yukinal/provider-sdk";
import { ProviderError } from "@yukinal/provider-sdk";

export interface OpenAiCompatibleConfig {
  /** Full base URL, e.g. `https://openrouter.ai/api/v1`. */
  baseUrl: string;
  model: string;
  apiKey?: string;
  customHeaders?: Record<string, string>;
  timeoutMs?: number;
  /** Endpoint dialect: chat completions (default) or the codex `responses` API. */
  wireApi?: "chat" | "responses";
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
      const responsesDialect = this.config.wireApi === "responses";
      const endpoint = `${this.config.baseUrl.replace(/\/$/, "")}/${responsesDialect ? "responses" : "chat/completions"}`;
      const response = await fetch(endpoint, {
        method: "POST",
        headers: { "content-type": "application/json", ...this.#headers() },
        body: JSON.stringify(
          responsesDialect
            ? {
                model: request.model,
                input: toResponsesInput(request.messages),
                tools: request.tools?.map((tool) => ({
                  type: "function",
                  name: tool.function.name,
                  description: tool.function.description,
                  parameters: tool.function.parameters,
                })),
                stream: true,
                temperature: request.temperature ?? 0,
                max_output_tokens: request.maxOutputTokens,
              }
            : {
                model: request.model,
                messages: request.messages,
                tools: request.tools,
                stream: true,
                temperature: request.temperature ?? 0,
                max_tokens: request.maxOutputTokens,
              },
        ),
        signal: controller.signal,
      });

      if (!response.ok || !response.body) {
        const detail = await response.text().catch(() => "");
        throw new ProviderError(
          `${endpoint} failed (${response.status}): ${detail.slice(0, 300)}`,
          response.status >= 500,
          response.status,
        );
      }

      yield* responsesDialect ? this.#consumeResponsesSse(response.body) : this.#consumeSse(response.body);
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

  /** chat/completions 的 SSE：`data:` 行可能是 JSON chunk，`[DONE]` 结尾。工具调用按 index 累积。 */
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
            args = { raw: slot.args };
          }
          return { type: "tool_call" as const, call: { id: slot.id, name: slot.name, arguments: args } };
        });

    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      buffer += decoder.decode(value, { stream: true });
      const lines = buffer.split("\n");
      buffer = lines.pop() ?? "";
      for (const line of lines) {
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
          continue;
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

  /** codex `responses` API 的 SSE。事件：output_text.delta / output_item.added / function_call_arguments.delta。 */
  async *#consumeResponsesSse(body: ReadableStream<Uint8Array>): AsyncGenerator<StreamEvent> {
    const reader = body.getReader();
    const decoder = new TextDecoder();
    let buffer = "";
    const slots = new Map<string, { id: string; name: string; args: string }>();

    const flush = (): StreamEvent[] =>
      [...slots.entries()]
        .filter(([, slot]) => slot.name)
        .map(([, slot]) => {
          let args: Record<string, unknown> = {};
          try {
            args = slot.args ? (JSON.parse(slot.args) as Record<string, unknown>) : {};
          } catch {
            args = { raw: slot.args };
          }
          return { type: "tool_call" as const, call: { id: slot.id, name: slot.name, arguments: args } };
        });

    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      buffer += decoder.decode(value, { stream: true });
      const lines = buffer.split("\n");
      buffer = lines.pop() ?? "";
      for (const line of lines) {
        const trimmed = line.trim();
        if (!trimmed.startsWith("data:")) continue;
        const payload = trimmed.slice(5).trim();
        if (payload === "[DONE]") {
          for (const event of flush()) yield event;
          yield { type: "done", finishReason: "stop" };
          return;
        }
        let event: ResponsesEvent;
        try {
          event = JSON.parse(payload) as ResponsesEvent;
        } catch {
          continue;
        }
        switch (event.type) {
          case "response.output_text.delta":
            if (event.delta) yield { type: "text_delta", text: event.delta };
            break;
          case "response.output_item.added": {
            const item = event.item;
            if (item?.type === "function_call" && item.id) {
              slots.set(item.id, { id: item.call_id ?? item.id, name: item.name ?? "", args: item.arguments ?? "" });
            }
            break;
          }
          case "response.function_call_arguments.delta": {
            const slot = event.item_id ? slots.get(event.item_id) : undefined;
            if (slot && event.delta) slot.args += event.delta;
            break;
          }
          default:
            break;
        }
      }
    }
    for (const event of flush()) yield event;
    yield { type: "done", finishReason: "stop" };
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

interface ResponsesEvent {
  type: string;
  delta?: string;
  item_id?: string;
  item?: { type?: string; id?: string; call_id?: string; name?: string; arguments?: string };
}

function toResponsesInput(messages: LlmMessage[]): Array<Record<string, unknown>> {
  const input: Array<Record<string, unknown>> = [];
  for (const message of messages) {
    if (message.role === "tool") {
      input.push({ type: "function_call_output", call_id: message.toolCallId, output: message.content });
      continue;
    }
    if (message.role === "assistant" && message.toolCalls?.length) {
      if (message.content) input.push({ role: "assistant", content: [{ type: "output_text", text: message.content }] });
      for (const call of message.toolCalls) {
        input.push({ type: "function_call", call_id: call.id, name: call.name, arguments: JSON.stringify(call.arguments) });
      }
      continue;
    }
    const contentType = message.role === "assistant" ? "output_text" : "input_text";
    input.push({ role: message.role, content: [{ type: contentType, text: message.content }] });
  }
  return input;
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
