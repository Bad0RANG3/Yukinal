import type { ContextSource } from "./context-engine.js";

/**
 * Day-0 context source: knows nothing, on purpose.
 *
 * Real data arrives through Rust/Tauri once `yukinal-database` （not built yet） and
 * `yukinal-collector` （not built yet） exist; the agent then gets an IPC-backed implementation
 * of `ContextSource`. Until then this keeps the wiring honest — no fake metrics.
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
