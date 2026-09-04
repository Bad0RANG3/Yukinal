/**
 * JSON-RPC method dispatch for the sidecar (ADR 0001 / ADR 0006).
 *
 * Rule: params are validated here, never inside handlers, and anything not yet
 * built answers RPC_NOT_IMPLEMENTED instead of returning invented data.
 */

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
  type AgentStreamEvent,
  type ApprovalResponse,
  type RuntimeProviderConfig,
} from "@yukinal/shared";
import { AGENT_VERSION, type AgentLogger } from "../config.js";
import { AgentLoop } from "../runtime/agent-loop.js";
import { RpcFailure } from "../errors.js";
import { OpenAiCompatibleProvider } from "../providers/openai-compatible.js";
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
};

export class RpcRouter {
  #initialized = false;

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
      case AGENT_METHODS.runStart: {
        const parsed = parseOrThrow(AgentRunRequestSchema, request.params) as AgentRunRequest;
        const provider = buildProvider(parsed.providerConfig);
        // run 是流式的：先回 runId（响应帧必须先于任何 agent.* 通知），
        // 过程全走 agent.stream 通知。timers 保证响应先写、事件后到。
        setTimeout(() => {
          void this.#spinRun(parsed, provider);
        }, 0);
        return { runId: parsed.runId, started: true };
      }
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
      default:
        throw new RpcFailure(RPC_ERROR.METHOD_NOT_FOUND, `Unknown method "${request.method}"`);
    }
  }

  /** 后台跑 run：所有可见输出都是 agent.* 通知。 */
  async #spinRun(parsed: AgentRunRequest, provider: OpenAiCompatibleProvider): Promise<void> {
    const emit = (event: AgentStreamEvent): void => {
      this.#notificationSink?.(AGENT_NOTIFICATIONS.stream, event);
    };
    try {
      const result = await this.deps.loop.start(parsed, { emit }, provider);
      this.deps.log.info("run finished", { runId: parsed.runId, state: result.state, steps: result.steps, toolCalls: result.toolCalls });
    } catch (error) {
      this.#notificationSink?.(AGENT_NOTIFICATIONS.stream, {
        type: "agent.failed",
        runId: parsed.runId,
        error: error instanceof Error ? error.message : String(error),
        at: new Date().toISOString(),
      });
    }
  }

  #initialize(params: unknown): InitializeResult {
    const parsed = parseInitializeParams(params);
    this.#initialized = true;
    this.deps.log.info("initialized", { clientVersion: parsed.clientVersion, dataDir: parsed.dataDir });

    return {
      protocolVersion: YUKINAL_RPC_VERSION,
      agentVersion: AGENT_VERSION,
      capabilities: { streaming: true, toolCalling: true, cancellation: true, mcp: false },
    };
  }

  #ping(params: unknown): { pong: string; agentPid: number } {
    const echo = (params as { echo?: unknown } | undefined)?.echo;
    return { pong: typeof echo === "string" ? echo : "pong", agentPid: process.pid };
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
  const candidate = params as Partial<InitializeParams> | undefined;
  if (!candidate || typeof candidate !== "object") {
    throw new RpcFailure(RPC_ERROR.INVALID_PARAMS, "initialize requires params");
  }
  if (candidate.protocolVersion !== YUKINAL_RPC_VERSION) {
    throw new RpcFailure(
      RPC_ERROR.INVALID_PARAMS,
      `protocol version mismatch: desktop asked for "${String(candidate.protocolVersion)}", agent speaks "${YUKINAL_RPC_VERSION}"`,
    );
  }
  return {
    protocolVersion: YUKINAL_RPC_VERSION,
    clientVersion: candidate.clientVersion ?? "unknown",
    dataDir: candidate.dataDir ?? "",
  };
}

const AgentRunStopSchema = z.object({ runId: z.string().min(1) });

/** 每次 run 由 Rust 注入 provider 材料；构造失败立即报错（不是 run 的失败）。 */
function buildProvider(config: RuntimeProviderConfig | undefined): OpenAiCompatibleProvider {
  if (!config) {
    throw new RpcFailure(RPC_ERROR.INVALID_PARAMS, "run.start requires providerConfig (resolved by the core)");
  }
  if (config.kind !== "openai-compatible") {
    throw new RpcFailure(RPC_ERROR.INVALID_PARAMS, `unsupported provider kind "${config.kind}"`);
  }
  return new OpenAiCompatibleProvider({
    baseUrl: config.baseUrl,
    model: config.model,
    apiKey: config.apiKey,
    customHeaders: config.customHeaders,
    timeoutMs: config.timeoutMs,
    wireApi: config.wireApi,
  });
}
