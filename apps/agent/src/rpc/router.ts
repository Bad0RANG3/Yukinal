/**
 * JSON-RPC method dispatch for the sidecar (ADR 0001 / ADR 0006).
 *
 * Rule: params are validated here, never inside handlers, and anything not yet
 * built answers RPC_NOT_IMPLEMENTED instead of returning invented data.
 */

import { createHash } from "node:crypto";

import { z } from "zod";

import {
  AGENT_METHODS,
  AgentRunRequestSchema,
  RuntimeProviderConfigSchema,
  ApprovalResponseSchema,
  YUKINAL_RPC_VERSION,
  RPC_ERROR,
  assertUniqueProviderNames,
  type InitializeParams,
  type InitializeResult,
  type JsonRpcRequest,
  type SystemDescribeResult,
  type ToolDeclaration,
} from "@yukinal/shared";

import {
  AGENT_NOTIFICATIONS,
  type AgentRunRequest,
  type AgentPromptPart,
  type AgentRunResult,
  type AgentStreamEvent,
  type ApprovalResponse,
  type RuntimeProviderConfig,
} from "@yukinal/shared";
import { AGENT_VERSION, type AgentLogger } from "../config.js";
import { resolveRequestedPolicy } from "../permissions/policy-registry.js";
import { AgentLoop } from "../runtime/agent-loop.js";
import { RpcFailure } from "../errors.js";
import { AnthropicProvider } from "../providers/anthropic.js";
import { GeminiProvider } from "../providers/gemini.js";
import { OpenAiCompatibleProvider } from "../providers/openai-compatible.js";
import type { LLMProvider } from "@yukinal/provider-sdk";
import type { ToolRegistry } from "../tools/registry.js";

export const IMPLEMENTATION_STATUS: Record<string, boolean> = {
  [AGENT_METHODS.initialize]: true,
  [AGENT_METHODS.ping]: true,
  [AGENT_METHODS.listTools]: true,
  [AGENT_METHODS.describe]: true,
  [AGENT_METHODS.runStart]: true,
  [AGENT_METHODS.runStop]: true,
  [AGENT_METHODS.approvalRespond]: true,
  [AGENT_METHODS.providerModels]: true,
  [AGENT_METHODS.providerTest]: true,
};

/**
 * `agent.run.start`'s answer (mirrored by `AgentMethodResults` in `@yukinal/agent-sdk`).
 *
 * `runId` is always the identity the run has, which for a resume is the admitted one
 * rather than the id the resuming call happened to carry. `result` is present exactly
 * when the request asked for `delivery: "sync"`.
 */
interface RunStartResponse {
  runId: string;
  started: boolean;
  duplicate?: boolean;
  resumed?: boolean;
  result?: AgentRunResult;
}

export class RpcRouter {
  #initialized = false;
  #activeRuns = new Set<string>();

  /**
   * OpenCode-style admission receipts: retries of one message never fork a second run.
   *
   * A receipt outlives the call that created it, because a message identity is what a
   * transport retries against. `started` records whether execution ever began (an
   * admitted message with `resume: false` has not), and `completed` is the eviction
   * predicate — an admitted message is deliberately *not* completed, since evicting it
   * would let a later resume open a second, differently-named run for a message the
   * runner already accepted.
   */
  #admissions = new Map<
    string,
    { sessionId: string; fingerprint: string; runId: string; started: boolean; completed: boolean }
  >();

  #notificationSink: ((method: string, params: unknown) => void) | undefined;

  constructor(
    private readonly deps: {
      registry: ToolRegistry;
      loop: AgentLoop;
      log: AgentLogger;
      policyIds: string[];
    },
  ) {}

  /** 由传输层接线（stdio 在启动时调用）：agent.* 通知要写到 stdout 协议帧。 */
  attachNotifications(sink: (method: string, params: unknown) => void): void {
    this.#notificationSink = sink;
  }

  async handle(request: JsonRpcRequest): Promise<unknown> {
    if (!this.#initialized && request.method !== AGENT_METHODS.initialize) {
      throw new RpcFailure(RPC_ERROR.INVALID_REQUEST, "initialize must be the first call");
    }

    switch (request.method) {
      case AGENT_METHODS.initialize:
        return this.#initialize(request.params);
      case AGENT_METHODS.ping:
        return this.#ping(request.params);
      case AGENT_METHODS.listTools:
        return { tools: this.deps.registry.list() } satisfies { tools: ToolDeclaration[] };
      case AGENT_METHODS.describe:
        return this.#describe();
      case AGENT_METHODS.runStart:
        return this.#runStart(request.params);
      case AGENT_METHODS.runStop: {
        const { runId } = parseOrThrow(AgentRunStopSchema, request.params);
        return { stopped: this.deps.loop.stop(runId) };
      }
      case AGENT_METHODS.approvalRespond: {
        const response = parseOrThrow(ApprovalResponseSchema, request.params) as ApprovalResponse;
        return { accepted: this.deps.loop.respondApproval(response) };
      }
      case AGENT_METHODS.providerModels: {
        const config = parseOrThrow(RuntimeProviderConfigSchema, request.params);
        const provider = buildProvider(config);
        return { models: await provider.listModels() };
      }
      case AGENT_METHODS.providerTest: {
        const config = parseOrThrow(RuntimeProviderConfigSchema, request.params);
        const provider = buildProvider(config);
        let hasText = false;
        let completed = false;
        for await (const event of provider.stream({
          model: config.model,
          messages: [{ role: "user", content: "Reply with OK only." }],
          maxOutputTokens: 256,
          timeoutMs: 30_000,
        })) {
          if (event.type === "text_delta" && event.text.trim()) hasText = true;
          if (event.type === "error") throw new Error("模型测试失败，请检查端点、认证和模型配置。");
          if (event.type === "done") completed = event.finishReason === "stop" || event.finishReason === "length";
        }
        if (!hasText || !completed) throw new Error("模型未返回完整文本回复，请检查模型和接口类型后重试。");
        return { ok: true };
      }
      default:
        throw new RpcFailure(RPC_ERROR.METHOD_NOT_FOUND, `Unknown method "${request.method}"`);
    }
  }

  /**
   * `agent.run.start`: admit one message, then either execute it or not.
   *
   * The response answers three separate questions that used to be collapsed into one
   * `{ runId, started: true }`:
   *
   *   - `started` — did *this* call begin execution? An admitted-but-not-executed
   *     message (`resume: false`) and a retry of a message already under way both
   *     answer `false`, because neither started anything.
   *   - `duplicate` — was the message already admitted, so that no new run was opened?
   *     This is the property that keeps a transport retry from forking a second run.
   *   - `resumed` — did this call start a run that an earlier `resume: false` had
   *     admitted? The admitted `runId` is reused, so the caller's later retries of that
   *     same message keep collapsing onto one run.
   *
   * `delivery: "sync"` additionally holds the response until the run reaches a terminal
   * state and returns its result. The wait is bounded by the loop's own `maxRunMs`
   * timer, not by anything added here.
   */
  async #runStart(params: unknown): Promise<RunStartResponse> {
    const parsed = parseOrThrow(AgentRunRequestSchema, params) as AgentRunRequest;
    // `policyId` is a param, and params are validated here. This is not only a shape
    // check: an async run is spun *after* this call has answered, so a policy the
    // registry does not know would otherwise be reported as `started: true` and only
    // surface later as an `agent.failed` notification — a run the caller was told had
    // started, which never did. Throwing here makes it this request's `INVALID_PARAMS`,
    // before any receipt, before any registration.
    //
    // The loop resolves the requested policy again for the run itself: that precondition
    // belongs to the loop, which must hold it for every caller rather than trusting this
    // one to have checked.
    const requestedPolicy = resolveRequestedPolicy(parsed.policyId);
    this.deps.log.debug("run policy", {
      runId: parsed.runId,
      policyId: requestedPolicy?.id ?? "(environment default)",
    });

    const fingerprint = requestFingerprint(parsed);
    const admission = parsed.messageId ? this.#admissions.get(parsed.messageId) : undefined;

    if (admission) {
      if (admission.sessionId !== parsed.sessionId || admission.fingerprint !== fingerprint) {
        throw new RpcFailure(RPC_ERROR.INVALID_PARAMS, `messageId "${parsed.messageId}" was already admitted with different content`);
      }
      // Already executing, or finished: this call may not open a second run for a
      // message the runner has accepted once. A `sync` retry is answered immediately
      // for the same reason — the run it duplicates belongs to another call, and that
      // call's response is where the result is delivered.
      if (admission.started) return { runId: admission.runId, started: false, duplicate: true };
      // Admitted and never executed. A repeat of `resume: false` has nothing to do —
      // the admission is already recorded and already idempotent.
      if (parsed.resume === false) return { runId: admission.runId, started: false, duplicate: true };
      // The resume. It runs under the admitted identity, not under the `runId` this
      // call happens to carry: the receipt is keyed by message, and reusing its `runId`
      // is what makes a later retry collapse onto this run instead of forking.
      return this.#beginRun({ ...parsed, runId: admission.runId }, admission, true);
    }

    if (parsed.resume === false) {
      // Admitted, not executed, no events. The receipt is recorded here and nowhere
      // else: it is the whole of the promise `resume: false` makes, and it is what a
      // later resume addresses.
      //
      // A message with no identity cannot be admitted this way. Admission is keyed by
      // `messageId` (that is how a retry is recognised at all), so answering
      // `{ started: false }` without a receipt would be a promise nothing can ever
      // resume — the caller would have to guess whether anything was recorded.
      if (!parsed.messageId) {
        throw new RpcFailure(RPC_ERROR.INVALID_PARAMS, "resume: false requires messageId: the admission is keyed by it");
      }
      this.#admissions.set(parsed.messageId, {
        sessionId: parsed.sessionId,
        fingerprint,
        runId: parsed.runId,
        started: false,
        completed: false,
      });
      this.#pruneAdmissions();
      return { runId: parsed.runId, started: false };
    }

    return this.#beginRun(parsed, undefined, false);
  }

  /**
   * Register the run and hand it to the loop.
   *
   * Everything that another `agent.run.start` could observe — the active-run entry and
   * the receipt's `started` flag — is set *before* the first `await`, so a concurrent
   * retry of the same run or message can never slip through this window.
   */
  async #beginRun(
    parsed: AgentRunRequest,
    admission: { started: boolean } | undefined,
    resumed: boolean,
  ): Promise<RunStartResponse> {
    if (this.#activeRuns.has(parsed.runId)) {
      throw new RpcFailure(RPC_ERROR.INVALID_PARAMS, `runId "${parsed.runId}" is already running`);
    }
    const provider = buildProvider(parsed.providerConfig);
    this.#activeRuns.add(parsed.runId);
    if (admission) admission.started = true;
    if (parsed.messageId && !admission) {
      this.#admissions.set(parsed.messageId, {
        sessionId: parsed.sessionId,
        fingerprint: requestFingerprint(parsed),
        runId: parsed.runId,
        started: true,
        completed: false,
      });
      this.#pruneAdmissions();
    }

    // A run streams: the response frame carries the identity, and everything the run
    // observes arrives as `agent.stream` notifications. With `delivery: "async"` the
    // response is written first — the `setTimeout` puts the run after this synchronous
    // return, so `respond()` in the transport wins the race. With `delivery: "sync"`
    // the response necessarily comes *last*, after every notification of the run.
    // That reordering is safe on both sides of the transport: stdio writes each
    // response when its handler settles (frames carry the request id, and notifications
    // carry none), and Rust's supervisor keeps a pending map keyed by id, so a response
    // arriving after other frames is matched, not mis-assigned.
    if (parsed.delivery === "sync") {
      const result = await this.#spinRun(parsed, provider);
      return resumed
        ? { runId: parsed.runId, started: true, resumed: true, result }
        : { runId: parsed.runId, started: true, result };
    }
    // The crash is already visible as `agent.failed` on the event stream, and this
    // call's response has long been written, so there is nothing left to reject.
    setTimeout(() => {
      void this.#spinRun(parsed, provider).catch(() => undefined);
    }, 0);
    return resumed
      ? { runId: parsed.runId, started: true, resumed: true }
      : { runId: parsed.runId, started: true };
  }

  /**
   * 后台跑 run：所有可见输出都是 agent.* 通知。
   *
   * 这里抛出来的是**契约**错误（provider 没配好、prompt 为空、runId 已在跑），不是「这次
   * 运行失败了」——后者是 loop 自己返回的 `state: "failed"`。所以它被重新抛出：`sync`
   * 调用方的响应帧要带上这个错误，而 `async` 调用方早已拿到响应，只能从它发出的
   * `agent.failed` 通知里看到。
   */
  async #spinRun(parsed: AgentRunRequest, provider: LLMProvider): Promise<AgentRunResult> {
    const emit = (event: AgentStreamEvent): void => {
      this.#notificationSink?.(AGENT_NOTIFICATIONS.stream, event);
    };
    try {
      const result = await this.deps.loop.start(parsed, { emit }, provider);
      this.deps.log.info("run finished", { runId: parsed.runId, state: result.state, steps: result.steps, toolCalls: result.toolCalls });
      return result;
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      this.deps.log.error("run crashed", { runId: parsed.runId, error: message.slice(0, 300) });
      this.#notificationSink?.(AGENT_NOTIFICATIONS.stream, {
        type: "agent.failed",
        runId: parsed.runId,
        error: message,
        at: new Date().toISOString(),
      });
      // The notification above is emitted for both delivery modes, and deliberately so:
      // the event stream stays a complete record of what happened to a run no matter how
      // the caller asked for the answer. The rethrow is what a `sync` caller additionally
      // gets — its response frame carries the failure — while an `async` caller took its
      // response before the run began and has only the notification. So a `sync` failure
      // is visible twice, in two channels that mean different things; that is not a
      // duplicate report, and suppressing the event for `sync` would punch a hole in the
      // stream exactly when someone is debugging a failed run.
      throw error;
    } finally {
      this.#activeRuns.delete(parsed.runId);
      if (parsed.messageId) {
        const admission = this.#admissions.get(parsed.messageId);
        if (admission?.runId === parsed.runId) {
          admission.completed = true;
          this.#pruneAdmissions();
        }
      }
    }
  }

  /**
   * Keep retry receipts bounded without evicting an active receipt. Evicting an
   * in-flight message would make a transport retry start a second run, and evicting an
   * admitted-but-not-yet-resumed one would make the later resume open a run under a new
   * id instead of the admitted one. Only `completed` receipts — runs that reached a
   * terminal state — are evictable, so the map is bounded by completed receipts plus
   * whatever is admitted or in flight.
   */
  #pruneAdmissions(): void {
    const limit = 256;
    if (this.#admissions.size <= limit) return;
    for (const [messageId, admission] of this.#admissions) {
      if (this.#admissions.size <= limit) break;
      if (admission.completed) this.#admissions.delete(messageId);
    }
  }

  #initialize(params: unknown): InitializeResult {
    if (this.#initialized) {
      throw new RpcFailure(RPC_ERROR.INVALID_REQUEST, "initialize has already completed");
    }
    const parsed = parseInitializeParams(params);
    this.#initialized = true;
    this.deps.log.info("initialized", { clientVersion: parsed.clientVersion, dataDir: parsed.dataDir });

    return {
      protocolVersion: YUKINAL_RPC_VERSION,
      agentVersion: AGENT_VERSION,
      capabilities: {
        streaming: true,
        toolCalling: true,
        cancellation: true,
        // 这是一个**事实**，不是意图（ADR 0014）：只有注册表里此刻真的有 MCP 工具才为真。
        // 目录只能在 stdio 通道起来之后去取（宿主在握手完成前不转发 sidecar 的请求），所以
        // 握手时它通常是 false —— 那正是实话，而不是「MCP 没接」。
        mcp: this.deps.registry.list().some((declaration) => declaration.origin.kind === "mcp"),
      },
    };
  }

  #ping(params: unknown): { pong: string; agentPid: number } {
    const parsed = parseOrThrow(PingParamsSchema, params);
    return { pong: parsed.echo ?? "pong", agentPid: process.pid };
  }

  #describe(): SystemDescribeResult {
    const names = this.deps.registry.list().map((declaration) => declaration.name);
    const collisions: string[] = [];
    try {
      assertUniqueProviderNames(names);
    } catch (error) {
      collisions.push(error instanceof Error ? error.message : String(error));
    }

    return {
      providers: [],
      toolCount: names.length,
      permissionPolicyIds: this.deps.policyIds,
      toolNameCollisions: collisions,
      implemented: { ...IMPLEMENTATION_STATUS },
    };
  }
}

/**
 * Bind an admission to the complete prompt, not only its display text.
 *
 * Hashing keeps image bytes out of the long-lived receipt map while still making a
 * retry with different attachments fail instead of silently resuming under content
 * the runner never admitted.
 */
function requestFingerprint(request: AgentRunRequest): string {
  const parts = request.parts ?? [{ type: "text" as const, text: request.prompt }];
  const canonical = JSON.stringify({
    prompt: request.prompt,
    parts: parts.map(canonicalPart),
  });
  return createHash("sha256").update(canonical).digest("hex");
}

/**
 * The per-part shape that goes into the admission fingerprint.
 *
 * Every attachment kind is included **with its bytes**, which is the whole point: admitting a
 * `messageId` twice with different media is the defect this prevents, so a kind that could be
 * left out would be a hole. A `switch` rather than a chain of ternaries so that adding a kind
 * is a compile error here instead of a silently identical fingerprint.
 */
function canonicalPart(part: AgentPromptPart): unknown {
  switch (part.type) {
    case "text":
      return { type: "text", text: part.text };
    case "image":
      return {
        type: "image",
        mediaType: part.mediaType,
        data: part.data,
        name: part.name ?? null,
      };
    case "file":
      return { type: "file", mediaType: part.mediaType, data: part.data, name: part.name };
    case "document":
      return { type: "document", mediaType: part.mediaType, data: part.data, name: part.name };
    case "audio":
      return {
        type: "audio",
        mediaType: part.mediaType,
        data: part.data,
        name: part.name ?? null,
      };
  }
}

/**
 * Contract violations are INVALID_PARAMS, never INTERNAL_ERROR: the caller sent a
 * shape we agreed not to accept.
 */
function parseOrThrow<T>(schema: z.ZodType<T>, params: unknown): T {
  try {
    return schema.parse(params ?? {});
  } catch (error) {
    throw new RpcFailure(RPC_ERROR.INVALID_PARAMS, `invalid params`, {
      issues: error instanceof z.ZodError ? error.issues.map((issue) => ({ path: issue.path.join("."), message: issue.message })) : [String(error)],
    });
  }
}

function parseInitializeParams(params: unknown): InitializeParams {
  return parseOrThrow(InitializeParamsSchema, params);
}

const InitializeParamsSchema = z.strictObject({
  protocolVersion: z.literal(YUKINAL_RPC_VERSION),
  clientVersion: z.string().trim().min(1).max(128),
  dataDir: z.string().max(4096),
});

const PingParamsSchema = z.strictObject({ echo: z.string().max(1_000).optional() });
const AgentRunStopSchema = z.strictObject({ runId: z.string().trim().min(1).max(256) });

/**
 * The single place in the agent allowed to branch on provider identity (ADR 0003 /
 * ADR 0011 point 3: `buildProvider()` is the assembly point).
 *
 * Everything downstream — the loop, the permission engine, the tool registry — sees only
 * `LLMProvider` and `StreamEvent`. Adding a protocol means adding an adapter and a case
 * here, never an `if (provider === …)` inside the run.
 *
 * `kind` and `wireApi` are orthogonal (ADR 0011 point 2): `wireApi` picks the dialect of
 * the OpenAI-compatible translation and is therefore passed to that adapter **only** —
 * the schema already refuses a `wireApi` on the two native kinds, rather than letting it
 * mean something different here.
 *
 * An unsupported kind is INVALID_PARAMS at construction time, not a run that fails later:
 * this is a contract violation (the caller sent a provider we agreed not to accept), and
 * `agent.run.start` must not answer `started: true` for a run that cannot be built.
 */
function buildProvider(config: RuntimeProviderConfig | undefined): LLMProvider {
  if (!config) {
    throw new RpcFailure(RPC_ERROR.INVALID_PARAMS, "run.start requires providerConfig (resolved by the core)");
  }
  const { baseUrl, model, apiKey, customHeaders, apiVersion, timeoutMs } = config;
  switch (config.kind) {
    case "openai-compatible":
      return new OpenAiCompatibleProvider({ baseUrl, model, apiKey, customHeaders, timeoutMs, wireApi: config.wireApi });
    case "anthropic":
      return new AnthropicProvider({ baseUrl, model, apiKey, customHeaders, apiVersion, timeoutMs });
    case "gemini":
      return new GeminiProvider({ baseUrl, model, apiKey, customHeaders, timeoutMs });
  }
  // 走到这里说明 `AiProviderKind` 里有一个 kind 还没有适配器 —— 那是一次**契约**失败，不是
  // 运行失败，所以同一个 `INVALID_PARAMS`：调用方拿到的错误与「kind 拼错了」是同一种。
  // 新增 kind 时这一段会先以编译错误的形式出现（`config.kind` 在这里是 `never`），
  // 提醒装配点必须一起改；这行只是把它变成一句可读的错误而不是未捕获的异常。
  throw new RpcFailure(RPC_ERROR.INVALID_PARAMS, `unsupported provider kind "${String(config.kind)}"`);
}
