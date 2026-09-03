/**
 * Provider + MCP configuration contracts.
 *
 * Two distinct provider families on purpose:
 *  - AI providers         -> stream tokens + tool calls
 *  - Infrastructure providers -> expose Tools (github.*, aws.*, sentry.*)
 */

export type AiProviderKind = "openai-compatible";

/**
 * MVP ships exactly one kind: OpenAI-compatible (ADR 0003). Anthropic / Google /
 * Ollama / OpenRouter all speak it via baseUrl, so they need no extra kind here.
 * Adding a native kind must not leak `if (provider === "...")` into the loop.
 */
export interface AiProviderConfig {
  id: string;
  kind: AiProviderKind;
  label: string;
  baseUrl: string;
  model: string;
  /** Reference into the OS credential store. Never the key itself (). */
  apiKeyCredentialRef?: string;
  enabled: boolean;
  /** Extra headers for corporate gateways. */
  customHeaders?: Record<string, string>;
  maxInputTokens?: number;
  createdAt: string;
  updatedAt: string;
}

/**
 * Per-run provider material resolved by Rust and injected with `agent.run.start`.
 * The API key rides only on this transient payload — never persisted, never
 * logged — while the durable config (baseUrl/model/label) lives in SQLite.
 */
export interface RuntimeProviderConfig {
  kind: "openai-compatible";
  /** Full base URL, e.g. https://openrouter.ai/api/v1 */
  baseUrl: string;
  model: string;
  /** Resolved at the point of use; absent for local endpoints (Ollama…). */
  apiKey?: string;
  customHeaders?: Record<string, string>;
  timeoutMs?: number;
}

/** Settings form: label optional (defaults to baseUrl), apiKey goes to the keychain here. */
export interface ProviderSaveInput {
  label?: string;
  baseUrl: string;
  model: string;
  /** Present only when the user enters a new key; absent keeps the existing ref. */
  apiKey?: string;
}

export interface ModelInfo {
  id: string;
  label: string;
  contextWindow?: number;
  supportsToolCalling: boolean;
  supportsStreaming: boolean;
}

export interface ProviderStatus {
  id: string;
  label: string;
  state: "connected" | "not_configured" | "error" | "disabled";
  models?: ModelInfo[];
  detail?: string;
}

/** — infrastructure providers contribute tools, they are not called directly. */
export interface InfrastructureProviderConfig {
  id: string;
  kind: "github" | "gitlab" | "aws" | "gcp" | "azure" | "cloudflare" | "vercel" | "sentry" | "datadog" | "kubernetes";
  label: string;
  credentialRef?: string;
  enabled: boolean;
  settings?: Record<string, unknown>;
}

/** — MCP servers are untrusted by default. */
export interface McpServerConfig {
  id: string;
  label: string;
  transport: "stdio" | "http";
  command?: string;
  args?: string[];
  url?: string;
  enabled: boolean;
  /**
   * Tools this server is allowed to register. Empty = nothing is auto-trusted;
   * the user must opt in per tool after seeing its description.
   */
  allowedTools: string[];
  /** Every MCP tool starts at >= "medium" until reviewed. */
  trustLevel: "reviewed" | "unreviewed";
}
