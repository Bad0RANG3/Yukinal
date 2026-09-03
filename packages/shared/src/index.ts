/**
 * @yukinal/shared — single source of truth for cross-layer contracts.
 *
 * Rules:
 * - No UI code, no Node-only APIs, no LLM SDK imports in here.
 * - Types are the contract; zod schemas are the runtime gate; Rust mirrors these
 *   shapes with serde (snake_case fields, dot-free enum strings).
 */

export * from "./types/enums.js";
export * from "./types/risk.js";
export * from "./types/tool.js";
export * from "./types/server.js";
export * from "./types/collector.js";
export * from "./types/activity.js";
export * from "./types/provider.js";
export * from "./types/chat.js";
export * from "./types/health.js";

export * from "./schemas/server.js";
export * from "./schemas/permission.js";
export * from "./schemas/collector.js";
export * from "./schemas/ipc.js";

export * from "./events/index.js";
export * from "./ipc/index.js";
export * from "./naming/tool-name.js";
export * from "./protocol/jsonrpc.js";
export * from "./protocol/ndjson.js";
