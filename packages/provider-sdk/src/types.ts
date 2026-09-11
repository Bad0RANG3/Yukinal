/**
 * The LLM boundary.
 *
 * Hard rule: the agent core never branches on provider identity.
 * Everything provider-specific — including the `.` -> `__` tool-name rewriting
 * (ADR 0004) — lives behind this interface.
 */

import type { JsonSchema, ModelInfo } from "@yukinal/shared";

/**
 * `ModelInfo` 由 `@yukinal/shared` 定义，这里只做转发。
 *
 * 之前两个包各自声明了一份逐字节相同的接口。它们描述的是**同一个**线上的东西：
 * `ProviderStatus.models`（shared，UI 侧读取）和 `LLMProvider.listModels()`
 * （provider-sdk，适配器侧返回）指的是同一批模型数据。两份声明意味着可以只改一份，
 * 而两侧都是纯类型、编译器不会因为它们"不相等"报错 —— 只要每个使用点各自能解析，
 * 结构差异就一直合法，直到某个字段在跨界时静默变成 undefined。
 *
 * provider-sdk 本来就依赖 shared（上一行的 `JsonSchema` 就是），所以这里转发不在
 * 依赖图里新增任何东西，只是把「谁是权威」写清楚。转发而非重复，意味着漂移在结构上
 * 不可能发生，不需要再加一条一致性断言去追它。
 */
export type { ModelInfo };

export interface ToolCall {
  id: string;
  /** Internal dot-namespaced name. Providers never see this spelling. */
  name: string;
  arguments: Record<string, unknown>;
}

export type LlmMessage =
  | { role: "system"; content: string }
  | { role: "user"; content: string }
  | { role: "assistant"; content: string; toolCalls?: ToolCall[] }
  | { role: "tool"; toolCallId: string; content: string };

export interface ProviderToolSpec {
  type: "function";
  function: {
    /** Provider-facing name (`docker__ps`). */
    name: string;
    description: string;
    parameters: JsonSchema;
  };
}

export interface ChatRequest {
  model: string;
  messages: LlmMessage[];
  tools?: ProviderToolSpec[];
  temperature?: number;
  maxOutputTokens?: number;
  /** abort must propagate to the in-flight HTTP request. */
  signal?: AbortSignal;
  /** a stalled stream must not hang the run. */
  timeoutMs?: number;
}

export type FinishReason = "stop" | "tool_calls" | "length" | "cancelled" | "error";

export type StreamEvent =
  | { type: "text_delta"; text: string }
  | { type: "reasoning_delta"; text: string }
  | { type: "tool_call"; call: ToolCall }
  | { type: "usage"; inputTokens: number; outputTokens: number }
  | { type: "done"; finishReason: FinishReason }
  | { type: "error"; message: string; retryable: boolean };

export interface LLMProvider {
  readonly id: string;
  /** The model this instance talks to (surfaced so the run can log what it used). */
  readonly model?: string;
  listModels(): Promise<ModelInfo[]>;
  stream(request: ChatRequest): AsyncIterable<StreamEvent>;
}

export class ProviderError extends Error {
  constructor(
    message: string,
    readonly retryable: boolean,
    readonly status?: number,
  ) {
    super(message);
    this.name = "ProviderError";
  }
}
