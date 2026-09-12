/**
 * Provider + MCP configuration contracts.
 *
 * Two distinct provider families on purpose:
 *  - AI providers         -> stream tokens + tool calls
 *  - Infrastructure providers -> expose Tools (github.*, aws.*, sentry.*)
 */

/**
 * Which protocol translates this provider. The complete vocabulary, in one place,
 * because it is the axis that reaches every layer of the app: the Zod schemas, the
 * Rust enum and the `provider_configs.kind` column, `runtime_provider_config()`, the
 * settings UI, and `buildProvider()` — the one place allowed to branch on it.
 *
 *  - `openai-compatible` — one adapter covering every endpoint that speaks the OpenAI
 *    shape (OpenAI, OpenRouter, Ollama, LM Studio, vLLM, in-house gateways).
 *  - `anthropic` / `gemini` — native protocols whose translation cannot be expressed
 *    as a compatible endpoint, so each has its own adapter (ADR 0011).
 *
 * Adding a kind here is not enough on its own: a kind that cannot be *saved* is a
 * kind the user cannot configure (ADR 0011 point 6).
 */
export const AI_PROVIDER_KINDS = ["openai-compatible", "anthropic", "gemini"] as const;

export type AiProviderKind = (typeof AI_PROVIDER_KINDS)[number];

/**
 * One AI provider row. `kind` and `wireApi` are **orthogonal axes** (ADR 0011 point 2):
 * `kind` says *who translates*, `wireApi` says *which dialect of that one translation*.
 * Adding a native kind must not leak `if (provider === "...")` into the loop.
 */
export interface AiProviderConfig {
  id: string;
  kind: AiProviderKind;
  label: string;
  baseUrl: string;
  model: string;
  /** Reference into the OS credential store. Never the key itself. */
  apiKeyCredentialRef?: string;
  enabled: boolean;
  /** Extra headers for corporate gateways. */
  customHeaders?: Record<string, string>;
  maxInputTokens?: number;
  /**
   * Endpoint dialect (codex `responses` vs chat completions). **Only meaningful for
   * `openai-compatible`**: the two native kinds have no dialect axis, because the
   * protocol *is* the adapter's whole reason to exist.
   *
   * Both sides enforce that rather than ignoring the field: on a native kind the key
   * must be absent (`schemas/provider.ts` rejects it), and Rust omits it when it
   * serializes the row. So "present" always means "this is a chat-dialect choice",
   * and a `gemini` config carrying `wireApi: "responses"` is a rejected input instead
   * of a value whose meaning depends on whether the reader remembered to ignore it.
   */
  wireApi?: "chat" | "responses";
  /** Cached non-sensitive catalog entries used by the model selector. */
  models?: ProviderModelOption[];
  createdAt: string;
  updatedAt: string;
}

export interface ProviderModelOption {
  id: string;
  label: string;
  contextWindow?: number;
  supportsToolCalling: boolean;
  supportsStreaming: boolean;
}

/**
 * Per-run provider material resolved by Rust and injected with `agent.run.start`.
 * The API key rides only on this transient payload — never persisted, never
 * logged — while the durable config (baseUrl/model/label) lives in SQLite.
 */
export interface RuntimeProviderConfig {
  kind: AiProviderKind;
  /**
   * Full base URL. Rust falls back to the protocol's own public endpoint when the row
   * has none (or a blank one): `https://api.anthropic.com` for `anthropic`,
   * `https://generativelanguage.googleapis.com` for `gemini`. There is deliberately no
   * such fallback for `openai-compatible` — that kind covers endpoints we do not own,
   * so inventing a default would send the user's key somewhere they never chose.
   */
  baseUrl: string;
  model: string;
  /** Resolved at the point of use; absent for local endpoints (Ollama…). */
  apiKey?: string;
  customHeaders?: Record<string, string>;
  timeoutMs?: number;
  /**
   * Endpoint dialect: chat completions (default) or the codex `responses` API.
   * Only `openai-compatible` may carry it — see `AiProviderConfig.wireApi`.
   */
  wireApi?: "chat" | "responses";
}

/** Settings form: label optional (defaults to baseUrl), apiKey goes to the keychain here. */
export interface ProviderSaveInput {
  providerId?: string;
  /**
   * Which protocol this row speaks. Required, not defaulted: the whole point of the
   * kind axis is that a stored provider says what it is, and a save that omits the kind
   * would have to be interpreted as one of the three (ADR 0011 point 6).
   */
  kind: AiProviderKind;
  label?: string;
  baseUrl: string;
  model: string;
  /** Present only when the user enters a new key; absent keeps the existing ref. */
  apiKey?: string;
  /** Rejected for the two native kinds; see `AiProviderConfig.wireApi`. */
  wireApi?: "chat" | "responses";
  models?: ProviderModelOption[];
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

/**
 * 删除一个 Provider 的结果。
 *
 * `credentialReclaimed`：那份 keychain 条目在删除后不再被任何 Provider 引用，于是被一并
 * 移除。引用是可以共享的（导入的数据里就有一份引用挂在四行上），所以「密钥还在不在」是
 * 这次操作唯一无法从别处看出来的后果。
 */
export interface ProviderDeleteResponse {
  deleted: boolean;
  credentialReclaimed: boolean;
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
