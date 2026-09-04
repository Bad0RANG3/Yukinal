/** Outbound JSON-RPC client for requests that the Rust host executes. */

import {
  encodeFrame,
  HostContextRequestSchema,
  HostContextResponseSchema,
  HOST_METHODS,
  HostToolExecuteRequestSchema,
  HostToolExecuteResponseSchema,
  type HostContextRequest,
  type HostContextResponse,
  type HostToolExecuteRequest,
  type HostToolExecuteResponse,
} from "@yukinal/shared";

interface PendingRequest {
  resolve(value: unknown): void;
  reject(error: Error): void;
  signal?: AbortSignal;
  onAbort?: () => void;
}

export class HostRpcClient {
  #nextId = 1;
  readonly #pending = new Map<number, PendingRequest>();

  constructor(private readonly send: (frame: string) => void) {}

  execute(request: HostToolExecuteRequest, signal?: AbortSignal): Promise<HostToolExecuteResponse> {
    const params = HostToolExecuteRequestSchema.parse(request);
    return this.#request(
      HOST_METHODS.toolExecute,
      params,
      (value) => HostToolExecuteResponseSchema.parse(value),
      signal,
    );
  }

  fetchContext(request: HostContextRequest, signal?: AbortSignal): Promise<HostContextResponse> {
    const params = HostContextRequestSchema.parse(request);
    return this.#request(
      HOST_METHODS.contextFetch,
      params,
      (value) => HostContextResponseSchema.parse(value),
      signal,
    );
  }

  #request<T>(
    method: string,
    params: unknown,
    parse: (value: unknown) => T,
    signal?: AbortSignal,
  ): Promise<T> {
    if (signal?.aborted) return Promise.reject(new Error("host request cancelled"));

    const id = this.#nextId++;
    return new Promise<T>((resolve, reject) => {
      const pending: PendingRequest = {
        resolve: (value) => {
          try {
            resolve(parse(value));
          } catch (error) {
            reject(error instanceof Error ? error : new Error(String(error)));
          }
        },
        reject,
        signal,
      };
      const onAbort = (): void => {
        this.#pending.delete(id);
        reject(new Error("host request cancelled"));
      };
      pending.onAbort = onAbort;
      this.#pending.set(id, pending);
      signal?.addEventListener("abort", onAbort, { once: true });

      try {
        this.send(encodeFrame({ jsonrpc: "2.0", id, method, params }));
      } catch (error) {
        this.#pending.delete(id);
        signal?.removeEventListener("abort", onAbort);
        reject(error instanceof Error ? error : new Error(String(error)));
      }
    });
  }

  /** Consume a JSON-RPC response frame from the Rust host. */
  handleIncoming(message: unknown): boolean {
    const candidate = asObject(message);
    if (typeof candidate.id !== "number" || (!Object.hasOwn(candidate, "result") && !Object.hasOwn(candidate, "error"))) {
      return false;
    }
    const pending = this.#pending.get(candidate.id);
    if (!pending) return true;
    this.#pending.delete(candidate.id);
    if (pending.signal && pending.onAbort) pending.signal.removeEventListener("abort", pending.onAbort);

    if (Object.hasOwn(candidate, "error")) {
      const error = asObject(candidate.error);
      pending.reject(new Error(typeof error.message === "string" ? error.message : "host request failed"));
    } else {
      pending.resolve(candidate.result);
    }
    return true;
  }

  close(): void {
    const pending = [...this.#pending.values()];
    this.#pending.clear();
    for (const request of pending) {
      if (request.signal && request.onAbort) request.signal.removeEventListener("abort", request.onAbort);
      request.reject(new Error("host connection closed"));
    }
  }
}

function asObject(value: unknown): Record<string, unknown> {
  return typeof value === "object" && value !== null ? (value as Record<string, unknown>) : {};
}
