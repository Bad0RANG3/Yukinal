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

import { AGENT_VERSION, type AgentLogger } from "../config.js";
import { AgentLoop } from "../runtime/agent-loop.js";
import { RpcFailure } from "../errors.js";
import type { ToolRegistry } from "../tools/registry.js";

export const IMPLEMENTATION_STATUS: Record<string, boolean> = {
  [AGENT_METHODS.initialize]: true,
  [AGENT_METHODS.ping]: true,
  [AGENT_METHODS.listTools]: true,
  [AGENT_METHODS.describe]: true,
  [AGENT_METHODS.runStart]: false,
  [AGENT_METHODS.runStop]: false,
  [AGENT_METHODS.approvalRespond]: false,
};

export class RpcRouter {
  #initialized = false;

  constructor(
    private readonly deps: {
      registry: ToolRegistry;
      loop: AgentLoop;
      log: AgentLogger;
      policyIds: string[];
    },
  ) {}

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
        return this.deps.loop.start(parseOrThrow(AgentRunRequestSchema, request), {
          emit: () => {
            /* stream events are not wired yet through the stdio transport */
          },
        });
      case AGENT_METHODS.runStop:
      case AGENT_METHODS.approvalRespond: {
        // Validate the contract first, then fail honestly: the request shape is
        // settled even though the behaviour is not implemented yet.
        if (request.method === AGENT_METHODS.approvalRespond) {
          parseOrThrow(ApprovalResponseSchema, request);
        }
        throw new RpcFailure(
          RPC_ERROR.NOT_IMPLEMENTED,
          `${request.method} is not implemented yet`,
          { method: request.method },
        );
      }
      default:
        throw new RpcFailure(RPC_ERROR.METHOD_NOT_FOUND, `Unknown method "${request.method}"`);
    }
  }

  #initialize(params: unknown): InitializeResult {
    const parsed = parseInitializeParams(params);
    this.#initialized = true;
    this.deps.log.info("initialized", { clientVersion: parsed.clientVersion, dataDir: parsed.dataDir });

    return {
      protocolVersion: YUKINAL_RPC_VERSION,
      agentVersion: AGENT_VERSION,
      capabilities: { streaming: false, toolCalling: true, cancellation: true, mcp: false },
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
function parseOrThrow<T>(schema: z.ZodType<T>, request: JsonRpcRequest): T {
  try {
    return schema.parse(request.params ?? {});
  } catch (error) {
    throw new RpcFailure(RPC_ERROR.INVALID_PARAMS, `${request.method}: invalid params`, {
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
