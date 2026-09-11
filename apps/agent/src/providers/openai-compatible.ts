/**
 * OpenAI-compatible provider (ADR 0003): one implementation covers OpenAI,
 * OpenRouter, Ollama, codex-style gateways, and internal proxies via `baseUrl`.
 *
 * Real HTTP + SSE only. `signal` is wired to `AbortController` so Stop truly
 * kills the in-flight request; `timeoutMs` bounds a stalled stream. Two dialects:
 * chat completions (default) and the codex `responses` API selected by the user.
 * Tool-name rewriting happens in the loop via `createProviderNameIndex` (ADR 0004).
 */

import type { ChatRequest, FinishReason, LLMProvider, LlmMessage, ModelInfo, StreamEvent } from "@yukinal/provider-sdk";
import { ProviderError } from "@yukinal/provider-sdk";
import { z } from "zod";

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
const ProviderModelsResponseSchema = z.strictObject({
  data: z.array(z.strictObject({ id: z.string().trim().min(1).max(256) })).max(1_000).optional(),
});

export class OpenAiCompatibleProvider implements LLMProvider {
  readonly id = "openai-compatible";
  readonly model: string;

  constructor(readonly config: OpenAiCompatibleConfig) {
    this.model = config.model;
  }

  async listModels(): Promise<ModelInfo[]> {
    try {
      const response = await fetch(`${this.config.baseUrl.replace(/\/$/, "")}/models`, {
        headers: this.#headers(),
        signal: AbortSignal.timeout(this.config.timeoutMs ?? DEFAULT_TIMEOUT_MS),
      });
      if (!response.ok) {
        throw new ProviderError(`listModels failed (${response.status})`, response.status >= 500, response.status);
      }
      const parsed = ProviderModelsResponseSchema.safeParse(await response.json());
      if (!parsed.success) throw new ProviderError("listModels returned an invalid model catalog", false, response.status);
      return (parsed.data.data ?? []).map((entry) => ({
        id: entry.id,
        label: entry.id,
        contextWindow: undefined,
        supportsToolCalling: true,
        supportsStreaming: true,
      }));
    } catch (error) {
      if (error instanceof ProviderError) throw error;
      throw new ProviderError(
        `listModels request failed: ${safeProviderMessage(error instanceof Error ? error.message : String(error))}`,
        true,
      );
    }
  }

  async *stream(request: ChatRequest): AsyncIterable<StreamEvent> {
    const controller = new AbortController();
    const onParentAbort = (): void => controller.abort(request.signal?.reason);
    request.signal?.addEventListener("abort", onParentAbort, { once: true });
    if (request.signal?.aborted) onParentAbort();
    const timeoutMs = request.timeoutMs ?? this.config.timeoutMs ?? DEFAULT_TIMEOUT_MS;
    const timer = setTimeout(() => controller.abort(new Error("provider stream timed out")), timeoutMs);
    timer.unref?.();

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
        throw new ProviderError(
          `${endpoint} failed (${response.status})`,
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
    Object.assign(headers, this.config.customHeaders ?? {});
    if (this.config.apiKey) {
      // A configured key is authoritative. Remove case variants first so a
      // custom `Authorization` header cannot silently replace the keychain key.
      for (const key of Object.keys(headers)) {
        if (key.toLowerCase() === "authorization") delete headers[key];
      }
      headers.authorization = `Bearer ${this.config.apiKey}`;
    }
    return headers;
  }

  /** chat/completions 的 SSE：`data:` 行可能是 JSON chunk，`[DONE]` 结尾。工具调用按 index 累积。 */
  async *#consumeSse(body: ReadableStream<Uint8Array>): AsyncGenerator<StreamEvent> {
    let lastFinishReason: string | null = null;
    const slots = new Map<string, ToolCallSlot>();

    for await (const payload of sseData(body)) {
      if (payload === "[DONE]") {
        for (const event of toolCallEvents(slots)) yield event;
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
    // EOF 而没收到 [DONE]（异常结束）：把手里的工具调用放出来，避免吞掉。
    for (const event of toolCallEvents(slots)) yield event;
    yield { type: "done", finishReason: finishReasonFor(lastFinishReason) };
  }

  /** codex `responses` API 的 SSE。事件：output_text.delta / output_item.added / function_call_arguments.delta。 */
  async *#consumeResponsesSse(body: ReadableStream<Uint8Array>): AsyncGenerator<StreamEvent> {
    const slots = new Map<string, ToolCallSlot>();

    for await (const payload of sseData(body)) {
      if (payload === "[DONE]") {
        for (const event of toolCallEvents(slots)) yield event;
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
        case "response.failed":
          yield {
            type: "error",
            message: responseTerminalError(event, "Responses API request failed"),
            retryable: false,
          };
          return;
        case "response.incomplete":
          yield {
            type: "error",
            message: responseTerminalError(event, "Responses API response was incomplete"),
            retryable: false,
          };
          return;
        case "response.completed":
          for (const toolEvent of toolCallEvents(slots)) yield toolEvent;
          yield { type: "done", finishReason: "stop" };
          return;
        default:
          break;
      }
    }
    for (const event of toolCallEvents(slots)) yield event;
    yield { type: "done", finishReason: "stop" };
  }
}

/** 一个正在累积的工具调用：名字与参数都是分片到达，所以要拼起来。 */
interface ToolCallSlot {
  id: string;
  name: string;
  args: string;
}

/**
 * 把手里的工具调用槽位转成 `tool_call` 事件。
 *
 * 两个 SSE 消费者（chat/completions 与 responses）各有一份逐字相同的实现，唯一的差别
 * 是前者遍历 `slots.values()`、后者遍历 `slots.entries()` 再丢掉 key —— 那是同一段
 * 代码的两种写法，不是两种语义，所以提到模块级共用。
 *
 * 两条规则值得写下来，因为它们在两个调用点都必须成立：
 *
 * - **没有名字的槽位要跳过**。`output_item.added` 可能先给出 id 与 arguments 而名字
 *   尚未到达；发一个 `name: ""` 的 tool_call 会让模型看见一个它无法调用的工具。
 * - **参数解析失败时保留原文**（`{ raw: slot.args }`），不丢弃、不抛错。宁可让模型看到
 *   一段解析不了的参数并自行纠正，也不要静默吞掉整次调用。
 */
function toolCallEvents(slots: Map<string, ToolCallSlot>): StreamEvent[] {
  return [...slots.values()]
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
}

/** Yield each SSE data line, including a final line without a trailing newline. */
async function* sseData(body: ReadableStream<Uint8Array>): AsyncGenerator<string> {
  const reader = body.getReader();
  const decoder = new TextDecoder();
  let buffer = "";
  for (;;) {
    const { done, value } = await reader.read();
    if (done) {
      buffer += decoder.decode();
      break;
    }
    buffer += decoder.decode(value, { stream: true });
    const lines = buffer.split("\n");
    buffer = lines.pop() ?? "";
    for (const line of lines) {
      const trimmed = line.trim();
      if (trimmed.startsWith("data:")) yield trimmed.slice(5).trim();
    }
  }
  const finalLine = buffer.trim();
  if (finalLine.startsWith("data:")) yield finalLine.slice(5).trim();
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
  error?: { message?: string; code?: string } | null;
  response?: {
    error?: { message?: string; code?: string } | null;
    incomplete_details?: { reason?: string } | null;
  } | null;
}

function responseTerminalError(event: ResponsesEvent, fallback: string): string {
  return safeProviderMessage(
    event.response?.error?.message ??
    event.error?.message ??
    event.response?.incomplete_details?.reason ??
    fallback,
  );
}

/** Upstream gateways sometimes echo a masked API-key suffix in error text. */
function safeProviderMessage(message: string): string {
  return message
    .replace(/(["']?api[\s_-]*key["']?|["']?authorization["']?|["']?bearer["']?|["']?token["']?)\s*[:=]\s*["']?[^,\s)}"']+/gi, "$1=[redacted]")
    .slice(0, 300);
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
