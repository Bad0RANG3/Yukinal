/**
 * The LLM boundary.
 *
 * Hard rule: the agent core never branches on provider identity.
 * Everything provider-specific — including the `.` -> `__` tool-name rewriting
 * (ADR 0004) — lives behind this interface.
 */

import type { JsonSchema } from "@yukinal/shared";

export interface ModelInfo {
  id: string;
  label: string;
  contextWindow?: number;
  supportsToolCalling: boolean;
  supportsStreaming: boolean;
}

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
