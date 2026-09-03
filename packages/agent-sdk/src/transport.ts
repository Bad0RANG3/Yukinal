/**
 * Transport abstraction for the agent sidecar (ADR 0001 / ADR 0006).
 *
 * The client never touches child_process or WebSocket directly: Tauri/Rust owns
 * process lifecycle, and tests need a loopback.
 */

import type { JsonRpcMessage } from "@yukinal/shared";

export interface AgentTransport {
  send(message: JsonRpcMessage): void;
  /** Returns an unsubscribe function. */
  onMessage(handler: (message: JsonRpcMessage) => void): () => void;
  onClose(handler: (reason: string) => void): () => void;
  close(): void;
}

/**
 * Test/dev transport: every request is answered synchronously by `responder`.
 * Not for production — the real sidecar is spawned by Rust.
 */
export function createLoopbackTransport(
  responder: (message: JsonRpcMessage) => JsonRpcMessage | undefined,
): AgentTransport & { emit: (message: JsonRpcMessage) => void } {
  const messageHandlers = new Set<(message: JsonRpcMessage) => void>();
  const closeHandlers = new Set<(reason: string) => void>();

  return {
    send(message) {
      const reply = responder(message);
      if (reply !== undefined) {
        queueMicrotask(() => {
          for (const handler of messageHandlers) handler(reply);
        });
      }
    },
    onMessage(handler) {
      messageHandlers.add(handler);
      return () => messageHandlers.delete(handler);
    },
    onClose(handler) {
      closeHandlers.add(handler);
      return () => closeHandlers.delete(handler);
    },
    close() {
      for (const handler of closeHandlers) handler("closed-by-client");
      messageHandlers.clear();
      closeHandlers.clear();
    },
    emit(message) {
      for (const handler of messageHandlers) handler(message);
    },
  };
}
