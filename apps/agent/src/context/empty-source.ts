import type { ContextSource } from "./context-engine.js";

/**
 * Fallback context source for standalone sidecar tests and browser-like runs.
 *
 * The desktop composition root uses the host-backed source when Rust is present;
 * this implementation keeps the agent honest when there is no host to ask — no
 * fake servers or metrics are invented.
 */
export function createEmptyContextSource(): ContextSource {
  return {
    async server() {
      return undefined;
    },
    async snapshot() {
      return undefined;
    },
    async workspace() {
      return undefined;
    },
  };
}
