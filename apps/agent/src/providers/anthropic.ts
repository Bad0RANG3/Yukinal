/**
 * Native Anthropic Messages API provider (`POST {baseUrl}/v1/messages`, SSE).
 *
 * Reaching Claude through an OpenAI-compatible shim loses the two things the
 * Messages API is actually built around: the system prompt is a **top-level
 * field** (there is no `system` role in the `messages` array) and tool traffic is
 * expressed as `tool_use` / `tool_result` **content blocks** rather than a
 * separate `tool_calls` field. Those two translations are the reason this adapter
 * exists; the loop keeps speaking `LlmMessage` / `StreamEvent` and never learns
 * the dialect.
 *
 * Tool names reach this file as `docker__ps` and leave with exactly those bytes:
 * the `.` <-> `__` mapping belongs to `createProviderNameIndex` (ADR 0004) and the
 * Messages API accepts that spelling as-is, so nothing here rewrites names.
 */

import type {
  ChatRequest,
  FinishReason,
  LLMProvider,
  LlmMessage,
  ModelInfo,
  StreamEvent,
} from "@yukinal/provider-sdk";
import { ProviderError } from "@yukinal/provider-sdk";
import { z } from "zod";

export interface AnthropicConfig {
  /** Full base URL without the API path, e.g. `https://api.anthropic.com`. */
  baseUrl: string;
  model: string;
  apiKey?: string;
  customHeaders?: Record<string, string>;
  timeoutMs?: number;
  /**
   * `anthropic-version` request header. Overridable because the header is
   * versioned **by date**: Anthropic ships new header semantics under a new
   * dated value while keeping the old one working, so pinning it in code with no
   * escape hatch would make a protocol change a source edit.
   */
  apiVersion?: string;
}

const DEFAULT_TIMEOUT_MS = 60_000;

/**
 * `anthropic-version: 2023-06-01` —— Anthropic 目前的 GA 日期版本，也是官方文档与各
 * SDK 在用的那一个（更早的 2023-01-01 是初版）。**这是一个我无法离线核实的假设**：
 * 若上游已经推出更晚的日期版本，改 `config.apiVersion` 即可，不需要改代码。
 */
const DEFAULT_API_VERSION = "2023-06-01";

/**
 * Messages API 的 `max_tokens` 是**必填**字段 —— 省略它请求直接 400，所以这里必须有
 * 一个缺省值，而不能像 OpenAI 那样把 `undefined` 留在 body 里（JSON.stringify 会静默
 * 丢掉这个键）。
 *
 * 取 4096 的理由：它是这一代所有 Claude 模型都接受的值（Claude 3 Haiku 的输出上限就是
 * 4096），而更大的值在旧模型上会被拒。宁可让回复被截断成 `length` 让上层看见，也不要
 * 让请求直接失败。调用方要更长输出时传 `request.maxOutputTokens`。
 */
const DEFAULT_MAX_TOKENS = 4096;

/**
 * `/v1/models` 的顶层形状是稳定的（`data` + 分页字段），所以顶层用 strict：多出一个不
 * 认识的字段意味着目录格式漂移了，此时抛 `ProviderError` 让界面回退到手工填写模型 ID，
 * 比把一份可能已经变形的目录塞给选择器要好。
 *
 * 条目级别刻意**不**用 strict，只用 `z.object`（多余字段会被丢掉）：模型对象是随 API
 * 版本增减字段的地方（`type`、`created_at`、今后的能力字段），而我们真正需要的只有 `id`
 * 与 `display_name`。在条目上严格校验会把「上游加了个新字段」变成「目录整体不可用」。
 */
const ModelsResponseSchema = z.strictObject({
  data: z
    .array(
      z.object({
        id: z.string().trim().min(1).max(256),
        display_name: z.string().trim().min(1).max(256).optional(),
      }),
    )
    .max(1_000),
  // 分页字段必须在这里列出来，否则 strict 会把一个完全正常的目录判成畸形（空目录时
  // first_id/last_id 是 null）。我们不用它们，但"不认识"和"不存在"是两回事。
  has_more: z.boolean().optional(),
  first_id: z.string().nullable().optional(),
  last_id: z.string().nullable().optional(),
});

export class AnthropicProvider implements LLMProvider {
  readonly id = "anthropic";
  readonly model: string;

  constructor(readonly config: AnthropicConfig) {
    this.model = config.model;
  }

  async listModels(): Promise<ModelInfo[]> {
    const endpoint = `${this.config.baseUrl.replace(/\/$/, "")}/v1/models`;
    try {
      const response = await fetch(endpoint, {
        headers: this.#headers(),
        signal: AbortSignal.timeout(this.config.timeoutMs ?? DEFAULT_TIMEOUT_MS),
      });
      if (!response.ok) {
        // 不读 body：上游的 401/500 正文经常把掩码后的密钥片段一起回显出来。
        throw new ProviderError(`listModels failed (${response.status})`, response.status >= 500, response.status);
      }
      const parsed = ModelsResponseSchema.safeParse(await response.json());
      if (!parsed.success) {
        throw new ProviderError("listModels returned an invalid model catalog", false, response.status);
      }
      return parsed.data.data.map((entry) => ({
        id: entry.id,
        label: entry.display_name ?? entry.id,
        contextWindow: undefined,
        // 端点不告诉我们能力，这里是乐观默认值（与 OpenAI adapter 一致）。
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

    const endpoint = `${this.config.baseUrl.replace(/\/$/, "")}/v1/messages`;

    try {
      const system = systemPromptOf(request.messages);
      const body: Record<string, unknown> = {
        model: request.model,
        max_tokens: request.maxOutputTokens ?? DEFAULT_MAX_TOKENS,
        // 与 OpenAI adapter 一样缺省 0（落在 Anthropic 允许的 0..1 区间内），让同一次运行
        // 在不同 Provider 上尽可能可比；调用方传值即覆盖。
        temperature: request.temperature ?? 0,
        messages: toAnthropicMessages(request.messages),
        stream: true,
      };
      // 空的 system 字段不如不发：`system: ""` 对上游是无意义的输入，而缺省是合法状态。
      if (system) body.system = system;
      if (request.tools?.length) body.tools = request.tools.map(toAnthropicTool);

      const response = await fetch(endpoint, {
        method: "POST",
        headers: { "content-type": "application/json", ...this.#headers() },
        body: JSON.stringify(body),
        signal: controller.signal,
      });

      if (!response.ok || !response.body) {
        throw new ProviderError(`${endpoint} failed (${response.status})`, response.status >= 500, response.status);
      }

      yield* this.#consumeSse(response.body);
    } catch (error) {
      // 父信号取消是一次干净的结束，不是失败；超时不同，它是内部中止，必须浮出为 error。
      if (request.signal?.aborted) {
        yield { type: "done", finishReason: "cancelled" };
        return;
      }
      if (error instanceof ProviderError) throw error;
      // 响应体从不读取，所以这里只可能是网络层文本；仍然过一遍脱敏再交给上层。
      yield {
        type: "error",
        message: safeProviderMessage(error instanceof Error ? error.message : String(error)),
        retryable: false,
      };
    } finally {
      clearTimeout(timer);
      request.signal?.removeEventListener("abort", onParentAbort);
    }
  }

  #headers(): Record<string, string> {
    const headers: Record<string, string> = {};
    Object.assign(headers, this.config.customHeaders ?? {});
    if (this.config.apiKey) {
      // 凭据库里的 key 是权威来源：先删掉所有大小写形式的凭据头，否则 customHeaders 里的
      // 一个 `X-Api-Key` 会静默顶掉用户配置的密钥（与 OpenAI adapter 同一规则）。
      for (const key of Object.keys(headers)) {
        const lower = key.toLowerCase();
        if (lower === "x-api-key" || lower === "authorization") delete headers[key];
      }
      headers["x-api-key"] = this.config.apiKey;
    }
    // 版本头归适配器所有：customHeaders 不该决定协议版本，`config.apiVersion` 才是那个旋钮。
    for (const key of Object.keys(headers)) {
      if (key.toLowerCase() === "anthropic-version") delete headers[key];
    }
    headers["anthropic-version"] = this.config.apiVersion ?? DEFAULT_API_VERSION;
    return headers;
  }

  /**
   * Messages API 的 SSE。
   *
   * 事件序列（我们真正关心的部分）：`message_start` → 每个内容块一组
   * `content_block_start` / `content_block_delta`* / `content_block_stop` →
   * `message_delta`（终止原因与输出 token 计数）→ `message_stop`。
   *
   * 两个协议细节决定了这里的形状：
   *
   * - 工具参数以 `input_json_delta.partial_json` **字符串分片**到达，必须按块下标累加后
   *   再 parse；`content_block_start` 时给出的 `input` 是空对象占位，不能用。
   * - 用量分两处报告：输入 token 只在 `message_start` 里，最终输出 token 只在
   *   `message_delta` 里，所以 `usage` 事件必须等到流末（`message_stop`）才发，而不能在
   *   拿到一半时就发。
   */
  async *#consumeSse(body: ReadableStream<Uint8Array>): AsyncGenerator<StreamEvent> {
    let stopReason: string | null = null;
    let inputTokens = 0;
    let outputTokens = 0;
    let sawUsage = false;
    const slots = new Map<number, ToolUseSlot>();

    loop: for await (const payload of sseData(body)) {
      let event: AnthropicStreamEvent;
      try {
        event = JSON.parse(payload) as AnthropicStreamEvent;
      } catch {
        // 不是 JSON 的 data 行（例如某个代理追加的 `[DONE]`）直接忽略：本方言由
        // `message_stop` 与响应体结束终止，不需要哨兵值。
        continue;
      }
      // 按 payload 里的 `type` 分派，而不是按 SSE 的 `event:` 行：类型字段一定在，
      // 而经过代理后 event 行未必还在。
      switch (event.type) {
        case "message_start": {
          const usage = event.message?.usage;
          if (usage) {
            inputTokens = usage.input_tokens ?? inputTokens;
            outputTokens = usage.output_tokens ?? outputTokens;
            sawUsage = true;
          }
          break;
        }
        case "content_block_start": {
          const block = event.content_block;
          if (block?.type === "tool_use") {
            const index = event.index ?? 0;
            // 名字在这里是完整的（Anthropic 只分片参数），id 也一定有；兜底值只是为了让
            // 槽位形状保持完整，正常路径不会用到。
            slots.set(index, { id: block.id ?? `toolu_${index}`, name: block.name ?? "", args: "" });
          }
          break;
        }
        case "content_block_delta": {
          const delta = event.delta;
          if (delta?.type === "text_delta") {
            if (delta.text) yield { type: "text_delta", text: delta.text };
          } else if (delta?.type === "thinking_delta") {
            // 扩展思考的增量是**思考**，不是回答：映射到中立的 `reasoning_delta`
            // （ADR 0011 第 7 点）而不是混进 text_delta —— 混进去会让推理内容变成最终答案。
            if (delta.thinking) yield { type: "reasoning_delta", text: delta.thinking };
          } else if (delta?.type === "input_json_delta") {
            const slot = slots.get(event.index ?? 0);
            if (slot && delta.partial_json) slot.args += delta.partial_json;
          }
          // `signature_delta` 刻意忽略：它是一段用于回填思考块的签名，没有任何可显示的文本。
          break;
        }
        case "content_block_stop": {
          const index = event.index ?? 0;
          const slot = slots.get(index);
          if (!slot) break;
          slots.delete(index);
          const callEvent = toolCallEvent(slot);
          if (callEvent) yield callEvent;
          break;
        }
        case "message_delta": {
          if (event.delta?.stop_reason) stopReason = event.delta.stop_reason;
          const usage = event.usage;
          if (usage) {
            if (typeof usage.input_tokens === "number") inputTokens = usage.input_tokens;
            if (typeof usage.output_tokens === "number") outputTokens = usage.output_tokens;
            sawUsage = true;
          }
          break;
        }
        case "message_stop":
          break loop;
        case "error":
          // 流中途的错误只可能在上层已经消费了部分输出之后到达，所以一律标成不可重试
          // （与 OpenAI adapter 的 `response.failed` 同一处理）；是否值得重试是调用方的判断。
          yield {
            type: "error",
            message: safeProviderMessage(event.error?.message ?? "Anthropic stream error"),
            retryable: false,
          };
          return;
        default:
          // `ping`、以及将来新增的事件：忽略而不是报错，前向兼容比严格更值钱。
          break;
      }
    }

    // 走到这里有两种情况：正常收到 `message_stop`，或者响应体在 `message_stop` 之前就结束了
    // （连接被中途掐断）。后一种情况下手里的工具调用仍然要放出来，否则它们会凭空消失。
    for (const event of toolCallEvents(slots)) yield event;
    if (sawUsage) yield { type: "usage", inputTokens, outputTokens };
    yield { type: "done", finishReason: finishReasonFor(stopReason) };
  }
}

/** 一个正在累积的 `tool_use` 块：参数是分片到达的 JSON 文本，所以要拼起来再解析。 */
interface ToolUseSlot {
  id: string;
  name: string;
  args: string;
}

/**
 * 把槽位转成 `tool_call` 事件，两条规则与 OpenAI adapter 一致：
 *
 * - **没有名字的调用不发**。`name: ""` 会让模型看见一个它无法调用的工具，比不发更糟。
 * - **参数解析失败保留原文**（`{ raw }`）而不是抛错：宁可让模型看到一段它自己写坏的
 *   参数并纠正，也不要静默吞掉整次调用。
 */
function toolCallEvent(slot: ToolUseSlot): StreamEvent | null {
  if (!slot.name) return null;
  let args: Record<string, unknown> = {};
  try {
    args = slot.args ? (JSON.parse(slot.args) as Record<string, unknown>) : {};
  } catch {
    args = { raw: slot.args };
  }
  return { type: "tool_call", call: { id: slot.id, name: slot.name, arguments: args } };
}

function toolCallEvents(slots: Map<number, ToolUseSlot>): StreamEvent[] {
  return [...slots.values()]
    .map((slot) => toolCallEvent(slot))
    .filter((event): event is StreamEvent => event !== null);
}

/** Anthropic 的 `system`：顶层字段，多个 system 消息用空行拼接（Messages API 没有 system 角色）。 */
function systemPromptOf(messages: readonly LlmMessage[]): string {
  const parts: string[] = [];
  for (const message of messages) {
    if (message.role === "system" && message.content) parts.push(message.content);
  }
  return parts.join("\n\n");
}

/** 出站内容块。用 `Record<string, unknown>` 是刻意的：这是线上形状，不是给上层用的类型。 */
type AnthropicBlock = Record<string, unknown>;

interface AnthropicMessage {
  role: "user" | "assistant";
  content: AnthropicBlock[];
}

/**
 * 中立消息 → Messages API 的 `messages`。
 *
 * - `system` 消息在 `systemPromptOf` 里被提走，这里跳过（留在数组里会被上游 400）。
 * - assistant 的 `toolCalls` 变成 `tool_use` 块；纯文本回合变成单个 `text` 块。
 * - `tool` 消息变成 **user** 轮里的 `tool_result` 块，用 `tool_use_id` 指回那次调用 ——
 *   这是 Messages API 唯一的结果回填形状。
 *
 * 连续的 `tool_result` 会合并进同一个 user 轮：协议要求结果块排在同一轮内容的最前面，
 * 而把每个结果单独发一轮会把它们拆到不同的 assistant/user 交替里。
 */
function toAnthropicMessages(messages: readonly LlmMessage[]): AnthropicMessage[] {
  const out: AnthropicMessage[] = [];
  for (const message of messages) {
    switch (message.role) {
      case "system":
        break;
      case "user":
        out.push({ role: "user", content: [{ type: "text", text: message.content }] });
        break;
      case "assistant": {
        const content: AnthropicBlock[] = [];
        if (message.content) content.push({ type: "text", text: message.content });
        for (const call of message.toolCalls ?? []) {
          content.push({ type: "tool_use", id: call.id, name: call.name, input: call.arguments });
        }
        // 空的 content 数组在 Messages API 里是非法的，所以一次没有任何内容的 assistant
        // 回合直接丢掉，而不是发一个上游会拒绝的块数组。
        if (content.length > 0) out.push({ role: "assistant", content });
        break;
      }
      case "tool": {
        const block: AnthropicBlock = {
          type: "tool_result",
          tool_use_id: message.toolCallId,
          content: message.content,
        };
        const previous = out.at(-1);
        if (previous?.role === "user" && previous.content.every(isToolResult)) previous.content.push(block);
        else out.push({ role: "user", content: [block] });
        break;
      }
    }
  }
  return out;
}

function isToolResult(block: AnthropicBlock): boolean {
  return block.type === "tool_result";
}

/**
 * 工具声明 → Anthropic 的工具形状：`input_schema` 而不是 OpenAI 的 `parameters`。
 * `name` 原样透传（`docker__ps` 保持不变），ADR 0004 的映射只发生在 name index 里。
 */
function toAnthropicTool(tool: NonNullable<ChatRequest["tools"]>[number]): AnthropicBlock {
  return {
    name: tool.function.name,
    description: tool.function.description,
    input_schema: tool.function.parameters,
  };
}

interface AnthropicStreamEvent {
  type?: string;
  index?: number;
  message?: { usage?: { input_tokens?: number; output_tokens?: number } };
  content_block?: { type?: string; id?: string; name?: string };
  delta?: {
    type?: string;
    text?: string;
    thinking?: string;
    partial_json?: string;
    stop_reason?: string | null;
  };
  usage?: { input_tokens?: number; output_tokens?: number };
  error?: { type?: string; message?: string } | null;
}

/** 终止原因映射。`tool_use` 是最重要的一条：它决定 loop 是否继续跑工具。 */
function finishReasonFor(reason: string | null): FinishReason {
  switch (reason) {
    case "tool_use":
      return "tool_calls";
    case "max_tokens":
      return "length";
    case "end_turn":
    case "stop_sequence":
      return "stop";
    default:
      // `refusal`、`pause_turn`、以及将来新增的值都归到 stop：loop 把它们当作一次终止的
      // 文本回合处理，总比因为不认识一个字符串就失败要好。
      return "stop";
  }
}

/**
 * 逐行产出 SSE 的 `data:` 负载，**包括没有结尾换行的最后一行**（连接在最后一个事件
 * 之后被掐断时，那一行仍然有效，丢掉它等于丢掉一次工具调用）。
 *
 * 与 OpenAI adapter 里的同名 reader 形状相同，刻意各留一份：每个适配器自己负责它那一门
 * 方言的帧解析，改一门不会静默改到另一门。
 */
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

/**
 * 上游网关在错误正文里回显掩码密钥是真实存在的情况，界面不该成为它的出口。
 * 与 OpenAI adapter 的 `safeProviderMessage` 同一契约（截断 300 字符 + 脱敏赋值形式）。
 */
function safeProviderMessage(message: string): string {
  return message
    .replace(/(["']?api[\s_-]*key["']?|["']?authorization["']?|["']?bearer["']?|["']?token["']?)\s*[:=]\s*["']?[^,\s)}"']+/gi, "$1=[redacted]")
    .slice(0, 300);
}
