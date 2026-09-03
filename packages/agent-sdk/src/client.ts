/**
 * Typed JSON-RPC client for the agent sidecar (ADR 0001).
 *
 * Every action here is a request/response; everything the agent wants to show the
 * user arrives as an `agent.stream` notification (streaming, not one
 * payload at the end).
 */

import {
  AGENT_METHODS,
  AGENT_NOTIFICATIONS,
  RPC_ERROR,
  isJsonRpcResponse,
  type AgentRunRequest,
  type AgentStreamEvent,
  type ApprovalResponse,
  type InitializeParams,
  type InitializeResult,
  type JsonRpcError,
  type JsonRpcMessage,
  type SystemDescribeResult,
  type ToolDeclaration,
} from "@yukinal/shared";

import type { AgentTransport } from "./transport.js";

/** Keys are the literal method names held in `AGENT_METHODS`. */
export type AgentMethodParams = {
  initialize: InitializeParams;
  "system.describe": Record<string, never>;
  "system.ping": { echo?: string };
  "tools.list": Record<string, never>;
  "agent.run.start": AgentRunRequest;
  "agent.run.stop": { runId: string };
  "agent.approval.respond": ApprovalResponse;
};

export type AgentMethodResults = {
  initialize: InitializeResult;
  "system.describe": SystemDescribeResult;
  "system.ping": { pong: string; agentPid: number };
  "tools.list": { tools: ToolDeclaration[] };
  "agent.run.start": { runId: string; traceId: string };
  "agent.run.stop": { cancelled: boolean };
  "agent.approval.respond": { acknowledged: boolean };
};

export type AgentMethodName = keyof AgentMethodParams & keyof AgentMethodResults;

export class AgentRpcError extends Error {
  constructor(
    readonly code: number,
    message: string,
    readonly data?: unknown,
  ) {
    super(message);
    this.name = "AgentRpcError";
  }

  /** Scaffolding stages answer with this instead of faking behaviour. */
  get notImplemented(): boolean {
    return this.code === RPC_ERROR.NOT_IMPLEMENTED;
  }
}

export interface AgentClientOptions {
  /** Per-request timeout: the sidecar must answer, even with an error. */
  requestTimeoutMs?: number;
  clientVersion?: string;
}

interface Pending {
  resolve: (value: unknown) => void;
  reject: (error: Error) => void;
  timer: ReturnType<typeof setTimeout>;
}

export class AgentClient {
  readonly #requestTimeoutMs: number;
  readonly #pending = new Map<number, Pending>();
  readonly #streamHandlers = new Set<(event: AgentStreamEvent) => void>();
  #nextId = 1;
  #closed = false;

  constructor(
    private readonly transport: AgentTransport,
    private readonly options: AgentClientOptions = {},
  ) {
    this.#requestTimeoutMs = options.requestTimeoutMs ?? 30_000;
    transport.onMessage((message) => this.#handle(message));
    transport.onClose(() => this.#failAll(new Error("agent connection closed")));
  }

  get clientVersion(): string {
    return this.options.clientVersion ?? "0.0.0";
  }

  onStream(handler: (event: AgentStreamEvent) => void): () => void {
    this.#streamHandlers.add(handler);
    return () => {
      this.#streamHandlers.delete(handler);
    };
  }

  request<M extends AgentMethodName>(method: M, params: AgentMethodParams[M]): Promise<AgentMethodResults[M]> {
    if (this.#closed) return Promise.reject(new Error("agent client is closed"));
    const id = this.#nextId++;
    const payload: JsonRpcMessage = { jsonrpc: "2.0", id, method, params };

    return new Promise<AgentMethodResults[M]>((resolve, reject) => {
      const timer = setTimeout(() => {
        this.#pending.delete(id);
        reject(new AgentRpcError(RPC_ERROR.TIMEOUT, `${method} timed out after ${this.#requestTimeoutMs}ms`));
      }, this.#requestTimeoutMs);

      this.#pending.set(id, { resolve: resolve as (value: unknown) => void, reject, timer });

      try {
        this.transport.send(payload);
      } catch (error) {
        clearTimeout(timer);
        this.#pending.delete(id);
        reject(error instanceof Error ? error : new Error(String(error)));
      }
    });
  }

  initialize(dataDir: string): Promise<InitializeResult> {
    const params: InitializeParams = {
      protocolVersion: "1.0",
      clientVersion: this.clientVersion,
      dataDir,
    };
    return this.request(AGENT_METHODS.initialize, params);
  }

  describe(): Promise<SystemDescribeResult> {
    return this.request(AGENT_METHODS.describe, {});
  }

  ping(echo?: string): Promise<{ pong: string; agentPid: number }> {
    return this.request(AGENT_METHODS.ping, echo === undefined ? {} : { echo });
  }

  listTools(): Promise<ToolDeclaration[]> {
    return this.request(AGENT_METHODS.listTools, {}).then((result) => result.tools);
  }

  startRun(run: AgentRunRequest): Promise<{ runId: string; traceId: string }> {
    return this.request(AGENT_METHODS.runStart, run);
  }

  stopRun(runId: string): Promise<{ cancelled: boolean }> {
    return this.request(AGENT_METHODS.runStop, { runId });
  }

  respondApproval(response: ApprovalResponse): Promise<{ acknowledged: boolean }> {
    return this.request(AGENT_METHODS.approvalRespond, response);
  }

  close(): void {
    this.#closed = true;
    this.#failAll(new Error("agent client closed"));
    this.transport.close();
  }

  #handle(message: JsonRpcMessage): void {
    if (isJsonRpcResponse(message)) {
      const entry = this.#pending.get(message.id);
      if (!entry) return;
      this.#pending.delete(message.id);
      clearTimeout(entry.timer);
      if ("error" in message) {
        const error: JsonRpcError = message.error;
        entry.reject(new AgentRpcError(error.code, error.message, error.data));
      } else {
        entry.resolve(message.result);
      }
      return;
    }

    if (message.method === AGENT_NOTIFICATIONS.stream && isAgentStreamEvent(message.params)) {
      for (const handler of this.#streamHandlers) handler(message.params);
    }
  }

  #failAll(error: Error): void {
    for (const entry of this.#pending.values()) {
      clearTimeout(entry.timer);
      entry.reject(error);
    }
    this.#pending.clear();
  }
}

function isAgentStreamEvent(value: unknown): value is AgentStreamEvent {
  if (typeof value !== "object" || value === null) return false;
  const candidate = value as Partial<AgentStreamEvent>;
  return typeof candidate.type === "string" && typeof candidate.runId === "string";
}
