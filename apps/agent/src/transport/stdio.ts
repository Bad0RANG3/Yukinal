/**
 * stdio NDJSON transport for the sidecar (ADR 0006).
 *
 * stdout is protocol-only. Logs go to stderr. A malformed line is reported and
 * skipped — it must never take the process down.
 */

import { NdjsonDecoder, RPC_ERROR, encodeFrame, type JsonRpcFailure, type JsonRpcRequest } from "@yukinal/shared";

import type { AgentLogger } from "../config.js";
import { RpcFailure } from "../errors.js";
import type { RpcRouter } from "../rpc/router.js";
import type { HostRpcClient } from "./host-client.js";

export interface StdioServer {
  close(): void;
}

export function startStdioRpc(deps: {
  router: RpcRouter;
  log: AgentLogger;
  /** Bidirectional calls to the Rust host use the same stdout protocol stream. */
  hostToolClient?: HostRpcClient;
  /** Called when the desktop closes our stdin, i.e. it is going away. */
  onParentGone?: () => void;
}): StdioServer {
  // 通知（agent.stream / agent.log）走 stdout 协议帧（ADR 0006）。
  const decoder = new NdjsonDecoder((line, error) => {
    deps.log.warn("dropped malformed frame", { head: line.slice(0, 120), error: String(error) });
  });

  // 上行通知（agent.stream）也是协议帧；desktop 从 stdout 读。
  deps.router.attachNotifications((method, params) => {
    process.stdout.write(encodeFrame({ jsonrpc: "2.0", method, params }));
  });

  const write = (frame: JsonRpcFailure | { jsonrpc: "2.0"; id: number; result: unknown }): void => {
    process.stdout.write(encodeFrame(frame));
  };

  const respond = async (request: JsonRpcRequest): Promise<void> => {
    try {
      const result = await deps.router.handle(request);
      write({ jsonrpc: "2.0", id: request.id, result });
    } catch (error) {
      write({ jsonrpc: "2.0", id: request.id, error: toRpcError(error, deps.log) });
    }
  };

  const onData = (chunk: string): void => {
    for (const message of decoder.push(chunk)) {
      if (deps.hostToolClient?.handleIncoming(message)) continue;
      if (!isRequest(message)) {
        deps.log.warn("ignored non-request frame");
        continue;
      }
      void respond(message);
    }
  };

  const onEnd = (): void => {
    for (const message of decoder.end()) {
      if (deps.hostToolClient?.handleIncoming(message)) continue;
      if (isRequest(message)) void respond(message);
    }
    deps.onParentGone?.();
  };

  process.stdin.setEncoding("utf8");
  process.stdin.on("data", onData);
  process.stdin.on("end", onEnd);

  return {
    close() {
      process.stdin.off("data", onData);
      process.stdin.off("end", onEnd);
      deps.hostToolClient?.close();
    },
  };
}

function isRequest(value: unknown): value is JsonRpcRequest {
  const candidate = asObject(value);
  return candidate.jsonrpc === "2.0" && typeof candidate.method === "string" && typeof candidate.id === "number";
}

function asObject(value: unknown): Record<string, unknown> {
  return typeof value === "object" && value !== null ? (value as Record<string, unknown>) : {};
}

function toRpcError(error: unknown, log: AgentLogger): { code: number; message: string; data?: unknown } {
  if (error instanceof RpcFailure) {
    return { code: error.code, message: error.message, data: error.data };
  }
  if (error instanceof SyntaxError) {
    return { code: RPC_ERROR.INVALID_PARAMS, message: `params could not be parsed: ${error.message}` };
  }
  const message = error instanceof Error ? error.message : String(error);
  if (error instanceof Error && error.stack) log.debug("handler stack", { stack: error.stack });
  // Never forward a stack trace or internals to the caller.
  return { code: RPC_ERROR.INTERNAL_ERROR, message };
}
