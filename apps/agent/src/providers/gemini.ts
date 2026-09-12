/**
 * Native Google Gemini `generateContent` provider
 * (`POST {baseUrl}/v1beta/models/{model}:streamGenerateContent?alt=sse`).
 *
 * Reaching Gemini through an OpenAI-compatible shim loses the shape the API is
 * built around: one `contents` array whose turns carry `parts` (`text` and
 * `functionCall` / `functionResponse`), a separate `systemInstruction`, and
 * `tools[].functionDeclarations`. All of that translation lives here so the loop
 * keeps speaking `LlmMessage` / `StreamEvent`.
 *
 * Tool names arrive as `docker__ps` and leave untouched: the `.` <-> `__` mapping
 * belongs to `createProviderNameIndex` (ADR 0004).
 *
 * **The one dialect difference the loop must live with:** Gemini returns no call
 * ids, so this adapter invents them (see `#consumeSse`) and the `functionResponse`
 * it later sends back is matched upstream **by function name**, not by id.
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

export interface GeminiConfig {
  /** Full base URL without the API path, e.g. `https://generativelanguage.googleapis.com`. */
  baseUrl: string;
  model: string;
  apiKey?: string;
  customHeaders?: Record<string, string>;
  timeoutMs?: number;
}

const DEFAULT_TIMEOUT_MS = 60_000;

/**
 * `v1beta`：`generateContent` 与 `streamGenerateContent` 目前都在 v1beta 下，v1 只有
 * 一部分模型可用，所以这里按能力更全的那一个版本来。**这是我无法离线核实的假设。**
 */
const API_VERSION_PATH = "v1beta";

/**
 * Gemini 的模型目录条目携带大量（且不断增加的）字段：`version`、`description`、
 * `temperature`、`topP`、`topK`、`thinking`…… 这里刻意**不使用** strict schema，只取我们
 * 真正需要的几个键。上游加一个无关字段不应该让整个模型选择器变成一个 ProviderError；
 * 需要严格的只有「`models` 必须是一个数组」这一层。
 */
const ModelsResponseSchema = z.object({
  models: z
    .array(
      z.object({
        name: z.string().trim().min(1).max(256),
        displayName: z.string().trim().min(1).max(256).optional(),
        inputTokenLimit: z.number().int().positive().optional(),
        supportedGenerationMethods: z.array(z.string()).optional(),
      }),
    )
    .max(1_000),
  nextPageToken: z.string().optional(),
});

export class GeminiProvider implements LLMProvider {
  readonly id = "gemini";
  readonly model: string;

  constructor(readonly config: GeminiConfig) {
    this.model = config.model;
  }

  async listModels(): Promise<ModelInfo[]> {
    const endpoint = `${this.config.baseUrl.replace(/\/$/, "")}/${API_VERSION_PATH}/models`;
    try {
      const response = await fetch(endpoint, {
        headers: this.#headers(),
        signal: AbortSignal.timeout(this.config.timeoutMs ?? DEFAULT_TIMEOUT_MS),
      });
      if (!response.ok) {
        // 不读 body：上游的错误正文经常把掩码后的密钥片段一起回显出来。
        throw new ProviderError(`listModels failed (${response.status})`, response.status >= 500, response.status);
      }
      const parsed = ModelsResponseSchema.safeParse(await response.json());
      if (!parsed.success) {
        throw new ProviderError("listModels returned an invalid model catalog", false, response.status);
      }
      return parsed.data.models
        // 目录里同时有 embedding / 只支持 countTokens 之类的模型；选择器只该列出能对话的。
        // 字段缺失视为「不支持」，因为上游总是会给它。
        .filter((model) => model.supportedGenerationMethods?.includes("generateContent") ?? false)
        .map((model) => {
          const id = model.name.replace(/^models\//, "");
          return {
            id,
            label: model.displayName ?? id,
            // `inputTokenLimit` 就是上下文窗口；`outputTokenLimit` 不是，别混用。
            contextWindow: model.inputTokenLimit,
            // 端点不告诉我们能力，这里是乐观默认值（与 OpenAI adapter 一致）。
            supportsToolCalling: true,
            supportsStreaming: true,
          };
        });
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

    const endpoint =
      `${this.config.baseUrl.replace(/\/$/, "")}/${API_VERSION_PATH}/models/${modelPath(request.model)}` +
      ":streamGenerateContent?alt=sse";

    try {
      const system = systemPromptOf(request.messages);
      const body: Record<string, unknown> = {
        contents: toGeminiContents(request.messages),
        generationConfig: generationConfigFor(request),
      };
      if (system) body.systemInstruction = { parts: [{ text: system }] };
      if (request.tools?.length) {
        body.tools = [
          {
            functionDeclarations: request.tools.map((tool) => ({
              name: tool.function.name,
              description: tool.function.description,
              parameters: tool.function.parameters,
            })),
          },
        ];
      }

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
      // 一个 `X-Goog-Api-Key` / `Authorization` 会静默顶掉用户配置的密钥。
      for (const key of Object.keys(headers)) {
        const lower = key.toLowerCase();
        if (lower === "x-goog-api-key" || lower === "authorization") delete headers[key];
      }
      // 用请求头而不是 `?key=` 查询参数：URL 会原样进代理日志、访问日志与错误上报，
      // 而凭据库里读出来的 key 不该出现在那些地方（查询参数还会被 Referer 之类的链路带走）。
      headers["x-goog-api-key"] = this.config.apiKey;
    }
    return headers;
  }

  /**
   * `streamGenerateContent?alt=sse` 的 SSE：每个 `data:` 行是一段完整的
   * `{candidates, usageMetadata, promptFeedback}` JSON。
   *
   * 与 Anthropic 的两点不同：
   *
   * - `functionCall` 一次到达就是完整的 `{name, args}`，没有分片拼接，所以 `tool_call`
   *   事件在看见它的那一刻就发出，不需要等流末。
   * - 用量是**累计**值，每个 chunk 都会重报一遍，因此这里只记住最后一份，在流末发一次
   *   `usage`；否则一次三 chunk 的回复会发出三条重复的用量事件。
   */
  async *#consumeSse(body: ReadableStream<Uint8Array>): AsyncGenerator<StreamEvent> {
    let finish: string | null = null;
    let usage: { inputTokens: number; outputTokens: number } | null = null;
    let callIndex = 0;

    for await (const payload of sseData(body)) {
      let chunk: GeminiChunk;
      try {
        chunk = JSON.parse(payload) as GeminiChunk;
      } catch {
        // 不是 JSON 的 data 行（例如某个代理追加的 `[DONE]`）忽略：本方言由响应体结束终止。
        continue;
      }

      // Google API 的错误形状（HTTP 失败时作为正文，流中途也可能作为一段 data 到达）。
      if (chunk.error) {
        yield {
          type: "error",
          message: safeProviderMessage(chunk.error.message ?? "Gemini stream error"),
          retryable: false,
        };
        return;
      }

      // 拒答：没有 candidates，只有一句 blockReason。这是不可重试的 —— 同样的提示词再发
      // 一次还是会被拦下（要改的是提示词，不是重试策略）。
      const blockReason = chunk.promptFeedback?.blockReason;
      if (blockReason) {
        yield {
          type: "error",
          message: safeProviderMessage(`Gemini refused the prompt (blockReason: ${blockReason})`),
          retryable: false,
        };
        return;
      }

      if (chunk.usageMetadata) {
        usage = {
          inputTokens: chunk.usageMetadata.promptTokenCount ?? 0,
          outputTokens: chunk.usageMetadata.candidatesTokenCount ?? 0,
        };
      }

      for (const candidate of chunk.candidates ?? []) {
        if (candidate.finishReason) finish = candidate.finishReason;
        for (const part of candidate.content?.parts ?? []) {
          if (typeof part.text === "string" && part.text.length > 0) {
            // 思考模型的 thought part 带 `thought: true`。它是思考摘要，不是回答，所以映射到
            // 中立的 `reasoning_delta`（ADR 0011 第 7 点）：当成 text_delta 发出去等于把推理
            // 过程混进最终答案里。
            yield part.thought === true
              ? { type: "reasoning_delta", text: part.text }
              : { type: "text_delta", text: part.text };
          }
          const call = part.functionCall;
          // 没有名字的调用不发（与 OpenAI adapter 同一规则）：模型会看见一个无法调用的工具。
          if (!call?.name) continue;
          callIndex += 1;
          yield {
            type: "tool_call",
            call: {
              /**
               * Gemini 不返回调用 id，所以这里合成一个。用「流内递增计数器」而不是随机值：
               * id 只需要在一次回复内唯一（loop 用它把 `tool_use` 与后面的结果配起来），
               * 而确定性让同一次回复可复现、可测试、可缓存。**后果**：这个 id 从不到达
               * Gemini —— 回填结果时匹配靠的是函数名（见 `toGeminiContents`），所以 id 换
               * 成什么都不影响上游，只影响我们自己的配对。
               */
              id: `gemini_call_${callIndex}`,
              name: call.name,
              arguments: functionCallArgs(call.args),
            },
          };
        }
      }
    }

    if (usage) yield { type: "usage", inputTokens: usage.inputTokens, outputTokens: usage.outputTokens };
    yield { type: "done", finishReason: finishReasonFor(finish) };
  }
}

/**
 * 模型 ID 直接进路径。先剥掉 `models/` 前缀：有些客户端会把 `listModels()` 返回的
 * `name`（形如 `models/gemini-2.5-flash`）整条粘进配置里，拼出 `models/models/...` 只会得到
 * 一个 404。然后逐段编码 —— 既挡住 `?`/`#` 之类的路径注入，又不会把 `tunedModels/foo`
 * 里合法的斜杠编成 %2F。
 */
function modelPath(model: string): string {
  return model
    .replace(/^models\//, "")
    .split("/")
    .map((segment) => encodeURIComponent(segment))
    .join("/");
}

/**
 * `generationConfig`。
 *
 * `maxOutputTokens` **只在调用方给了值时才带**：与 Anthropic 不同，这个字段是可选的，
 * 省略时由模型自己的输出上限生效。硬塞一个默认值反而会把长回答静默截断成 `length`。
 * `temperature` 缺省 0，与 OpenAI adapter 保持一致。
 */
function generationConfigFor(request: ChatRequest): Record<string, unknown> {
  const config: Record<string, unknown> = { temperature: request.temperature ?? 0 };
  if (request.maxOutputTokens !== undefined) config.maxOutputTokens = request.maxOutputTokens;
  return config;
}

/** Gemini 的系统提示词是独立字段，不是一个 role。 */
function systemPromptOf(messages: readonly LlmMessage[]): string {
  const parts: string[] = [];
  for (const message of messages) {
    if (message.role === "system" && message.content) parts.push(message.content);
  }
  return parts.join("\n\n");
}

/** 出站 part / content 用 `Record<string, unknown>`：这是线上形状，不是给上层用的类型。 */
type GeminiPart = Record<string, unknown>;

interface GeminiContent {
  role: "user" | "model";
  parts: GeminiPart[];
}

/**
 * 中立消息 → `contents`。
 *
 * - `system` 消息在 `systemPromptOf` 里被提走，这里跳过。
 * - assistant 回合是 **`model`** 角色（Gemini 的词汇里没有 assistant），其 `toolCalls`
 *   变成 `functionCall` part。
 * - `tool` 消息变成 user 轮里的 `functionResponse` part。**Gemini 没有 id，只有函数名**，
 *   而我们手里只有 `toolCallId`，所以先从前面 assistant 回合的 `toolCalls` 建一张
 *   id → 名字的表，再按 id 反查名字。查不到时退化成把 id 当名字发出去：上游会把这次结果
 *   当成一个它没调用过的函数的结果，但至少结果没有凭空消失，模型能看见内容并自行纠正。
 * - 连续的结果合并进同一轮：Gemini 的一轮内容只能有一个 role，分开发等于伪造多轮对话。
 */
function toGeminiContents(messages: readonly LlmMessage[]): GeminiContent[] {
  const contents: GeminiContent[] = [];
  const namesByCallId = new Map<string, string>();

  for (const message of messages) {
    switch (message.role) {
      case "system":
        break;
      case "user":
        contents.push({ role: "user", parts: [{ text: message.content }] });
        break;
      case "assistant": {
        const parts: GeminiPart[] = [];
        if (message.content) parts.push({ text: message.content });
        for (const call of message.toolCalls ?? []) {
          namesByCallId.set(call.id, call.name);
          parts.push({ functionCall: { name: call.name, args: call.arguments } });
        }
        // 空的 parts 数组会被上游拒绝，所以一次没有任何内容的 model 回合直接丢掉。
        if (parts.length > 0) contents.push({ role: "model", parts });
        break;
      }
      case "tool": {
        const part: GeminiPart = {
          functionResponse: {
            name: namesByCallId.get(message.toolCallId) ?? message.toolCallId,
            response: functionResponseBody(message.content),
          },
        };
        const previous = contents.at(-1);
        if (previous?.role === "user" && previous.parts.every(isFunctionResponse)) previous.parts.push(part);
        else contents.push({ role: "user", parts: [part] });
        break;
      }
    }
  }
  return contents;
}

function isFunctionResponse(part: GeminiPart): boolean {
  return part.functionResponse !== undefined;
}

/**
 * `functionResponse.response` 必须是一个 JSON 对象（Struct）。我们的工具结果是字符串，
 * 通常是 JSON 文本，所以能解析成对象的原样透传（结构化结果不该被包一层字符串），其余的
 * 包在 `content` 键下 —— 上游会拒绝非对象的 `response`。
 */
function functionResponseBody(content: string): Record<string, unknown> {
  try {
    const parsed: unknown = JSON.parse(content);
    if (isPlainObject(parsed)) return parsed;
  } catch {
    // 不是 JSON：下面按纯文本包起来。
  }
  return { content };
}

/**
 * `args` 正常是上游解析好的对象，但经过网关重新序列化后可能是 JSON 字符串。解析失败时
 * 退化成 `{ raw }` 而不是丢掉这次调用（与 OpenAI adapter 的规则一致）。
 */
function functionCallArgs(args: unknown): Record<string, unknown> {
  if (typeof args === "string") {
    try {
      const parsed: unknown = JSON.parse(args);
      return isPlainObject(parsed) ? parsed : { raw: args };
    } catch {
      return { raw: args };
    }
  }
  return isPlainObject(args) ? args : {};
}

function isPlainObject(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

interface GeminiChunk {
  candidates?: Array<{
    content?: {
      parts?: Array<{ text?: string; thought?: boolean; functionCall?: { name?: string; args?: unknown } }>;
    };
    finishReason?: string | null;
  }>;
  promptFeedback?: { blockReason?: string | null } | null;
  usageMetadata?: { promptTokenCount?: number; candidatesTokenCount?: number };
  error?: { code?: number; message?: string; status?: string } | null;
}

/**
 * 终止原因映射。
 *
 * `SAFETY` / `RECITATION` / `PROHIBITED_CONTENT` 等一律归到 `stop`：这些值意味着「模型
 * 停了下来」，而 loop 对 `stop` 的处理就是结束这一轮。后果是**安全终止会表现为一次内容
 * 为空的正常回答**，而不是错误；真正需要报错的是 `promptFeedback.blockReason`（连
 * candidate 都没有），那条路径已经单独处理。
 */
function finishReasonFor(reason: string | null): FinishReason {
  switch (reason) {
    case "MAX_TOKENS":
      return "length";
    case "STOP":
      return "stop";
    default:
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
