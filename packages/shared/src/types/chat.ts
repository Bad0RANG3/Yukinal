/**
 * Agent run + chat contracts.
 *
 * These types are the vocabulary between: React <-> agent-sdk <-> apps/agent <-> LLM.
 */

import type { AGENT_RUN_STATES, TOOL_RESULT_STATUSES } from "./enums.js";
import type { RuntimeProviderConfig } from "./provider.js";
import type { AgentPermissionMode, AgentRunMode, PermissionApprovalSource, PermissionMode, RiskLevel } from "./risk.js";
import type { ToolOrigin, ToolTarget } from "./tool.js";

export type AgentRunState = (typeof AGENT_RUN_STATES)[number];

/** Derived from the tuple so `schemas/agent.ts` and this type cannot disagree. */
export type ToolResultStatus = (typeof TOOL_RESULT_STATUSES)[number];

export interface ChatSession {
  id: string;
  workspaceId?: string;
  serverId?: string;
  title: string;
  createdAt: string;
  updatedAt: string;
  archivedAt?: string;
  messageCount: number;
  lastMessagePreview?: string;
}

export interface ChatMessage {
  id: string;
  sessionId: string;
  role: "user" | "assistant" | "tool" | "system";
  content: string;
  /** User prompt parts persisted with the message, including bounded images, PDFs and text files. */
  parts?: AgentPromptPart[];
  /** Set for role === "tool": links the bubble to its trace card. */
  traceId?: string;
  createdAt: string;
}

/**
 * How many sessions the current search would return, per archive state.
 *
 * The filter tabs label themselves with these numbers, so they have to be counted
 * under the *same* query the list was asked for — a client-side count of the rows it
 * happens to hold would silently describe only the first page.
 */
export interface ChatSessionCounts {
  active: number;
  archived: number;
}

export interface ChatSessionListInput {
  query?: string;
  /** Omitted returns both states; false returns active sessions, true archived sessions. */
  archived?: boolean;
  /**
   * Rows to skip, for paging past the first page.
   *
   * Offset rather than a cursor: the only ordering is `updatedAt DESC`, which a page
   * boundary can express, and a conversation that is updated while the user pages
   * simply moves to the top instead of being skipped by a stale cursor.
   */
  offset?: number;
  limit?: number;
}

export interface ChatSessionRenameInput {
  sessionId: string;
  title: string;
}

export interface ChatSessionDetail {
  session: ChatSession;
  messages: ChatMessage[];
}

export interface ChatSessionCreateInput {
  sessionId?: string;
  workspaceId?: string;
  serverId?: string;
  title: string;
}

export interface ChatMessageAppendInput {
  sessionId: string;
  messageId?: string;
  role: ChatMessage["role"];
  content: string;
  parts?: AgentPromptPart[];
  traceId?: string;
  createdAt?: string;
}

export const AGENT_IMAGE_MEDIA_TYPES = [
  "image/png",
  "image/jpeg",
  "image/webp",
  "image/gif",
] as const;

export type AgentImageMediaType = (typeof AGENT_IMAGE_MEDIA_TYPES)[number];

export const AGENT_DOCUMENT_MEDIA_TYPES = ["application/pdf"] as const;

export type AgentDocumentMediaType = (typeof AGENT_DOCUMENT_MEDIA_TYPES)[number];

/**
 * Audio the composer can attach. Every one of these is identified by **magic bytes**, not by
 * the file extension or the browser's MIME guess: the same discipline the images and PDFs use.
 *
 * The set is wider than any single provider accepts — WAV and MP3 are what the OpenAI audio
 * parts take, OGG and FLAC are Gemini's — so a provider that cannot carry one of them says so
 * instead of quietly dropping it.
 */
export const AGENT_AUDIO_MEDIA_TYPES = [
  "audio/wav",
  "audio/mpeg",
  "audio/ogg",
  "audio/flac",
] as const;

export type AgentAudioMediaType = (typeof AGENT_AUDIO_MEDIA_TYPES)[number];

export const AGENT_PROMPT_LIMITS = {
  maxParts: 10,
  maxTextChars: 100_000,
  maxImages: 4,
  /** Raw decoded image bytes, before base64 expansion. */
  maxImageBytes: 4 * 1024 * 1024,
  /**
   * Raw bytes across every image, PDF **and audio clip** in one message. Base64 expands
   * this to about 6.7 MiB, leaving room under the sidecar's 8 MiB frame bound for text
   * and JSON syntax. One shared budget rather than three: the bound that matters is the
   * frame, and per-kind budgets that could sum past it would be a bound in name only.
   */
  maxTotalInlineBytes: 5 * 1024 * 1024,
  maxImageNameChars: 128,
  maxDocuments: 2,
  /** Raw PDF bytes before base64 expansion. */
  maxDocumentBytes: 3 * 1024 * 1024,
  maxDocumentNameChars: 128,
  maxAudios: 2,
  /** Raw audio bytes before base64 expansion. */
  maxAudioBytes: 4 * 1024 * 1024,
  maxAudioNameChars: 128,
  maxFiles: 4,
  /** Raw UTF-8 bytes in one attached text file. */
  maxFileBytes: 256 * 1024,
  /** Raw UTF-8 bytes across every attached text file in one message. */
  maxTotalFileBytes: 512 * 1024,
  maxFileNameChars: 128,
} as const;

export interface AgentTextPromptPart {
  type: "text";
  text: string;
}

/**
 * A bounded inline image. `data` is canonical base64 without a data-URL prefix;
 * provider adapters add their own envelope. Keeping the bytes in the part means a
 * retry or a durable transcript can reconstruct the exact model input without a
 * provider-specific URL or a second file-resolution step.
 */
export interface AgentImagePromptPart {
  type: "image";
  mediaType: AgentImageMediaType;
  data: string;
  name?: string;
}

/**
 * A bounded UTF-8 text file selected by the user. The contents stay inline so a
 * retry or restored conversation reconstructs the same model input; provider
 * adapters render it as a clearly delimited text block.
 */
export interface AgentTextFilePromptPart {
  type: "file";
  mediaType: "text/plain";
  data: string;
  name: string;
}

/**
 * A bounded inline PDF. The bytes stay in the prompt part so retries and restored
 * conversations reproduce the exact model input; each provider adapts it to its
 * own document block.
 */
export interface AgentDocumentPromptPart {
  type: "document";
  mediaType: "application/pdf";
  data: string;
  name: string;
}

/**
 * A bounded inline audio clip. The bytes stay in the prompt part for the same reason the
 * images do: a retry and a restored conversation reproduce the exact model input, and no
 * provider-specific upload state has to survive.
 */
export interface AgentAudioPromptPart {
  type: "audio";
  mediaType: AgentAudioMediaType;
  data: string;
  name?: string;
}

export type AgentPromptPart =
  | AgentTextPromptPart
  | AgentImagePromptPart
  | AgentTextFilePromptPart
  | AgentDocumentPromptPart
  | AgentAudioPromptPart;

/** What UI sends to start a run.: targets are ids, not prose. */
export interface AgentRunRequest {
  runId: string;
  sessionId: string;
  prompt: string;
  /** Stable message identity for admission/retry semantics. */
  messageId?: string;
  /** OpenCode-compatible message parts; `prompt` remains the compatibility fallback. */
  parts?: AgentPromptPart[];
  /**
   * `sync` holds the `agent.run.start` response until the run reaches a terminal state
   * and answers with its result; `async` (the default) answers as soon as the message
   * is admitted. The `agent.*` notifications stream identically either way — with
   * `sync` the response frame necessarily arrives after them.
   */
  delivery?: "async" | "sync";
  /**
   * Whether the runner should resume execution after admitting the message. `false`
   * admits the message and records the receipt without starting anything; a later call
   * with the same `messageId` starts that admitted run and keeps its `runId`.
   */
  resume?: boolean;
  workspaceId?: string;
  /** Currently focused server; the agent may still need to disambiguate. */
  focusServerId?: string;
  target?: ToolTarget;
  /**
   * The policy this run must be decided under, overriding the one derived from the
   * target environment. It is resolved before the run starts and every tool call of
   * the run is evaluated against it; an unknown id is rejected rather than falling
   * back to the environment default. Omitted -> the environment's built-in policy.
   */
  policyId?: string;
  /** User-selected execution delegation for this run. */
  permissionMode?: AgentPermissionMode;
  /**
   * Bounds what this run may accomplish at all. `plan` and `readonly` are
   * enforced as a hard deny on every non-read tool call by the permission
   * engine; `goal` leaves the run unconstrained. Omitted -> `goal`.
   */
  mode?: AgentRunMode;
  /** Durable sidecar needs a per-run provider: Rust resolves and injects this. */
  providerConfig?: RuntimeProviderConfig;
}

/** `agent.run.stop` params: stop decouples from the run only by its id. */
export interface AgentRunStopParams {
  runId: string;
}

/** Cumulative token usage reported by a Provider for one Agent run. */
export interface AgentTokenUsage {
  inputTokens: number;
  outputTokens: number;
}

export interface AgentRunResult {
  runId: string;
  state: AgentRunState;
  /** Final assistant text, also streamed as chunks. */
  text: string;
  steps: number;
  toolCalls: number;
  error?: string;
  /**
   * The execution trace this run wrote. Same value as the `traceId` carried by every
   * `agent.tool_call` / `agent.tool_result` of the run, so a finished run's audit rows
   * can be looked up without replaying the event stream.
   */
  traceId?: string;
}

/** Approval round-trip between the desktop and the sidecar. */
export interface ApprovalRequest {
  approvalId: string;
  runId: string;
  toolName: string;
  input: unknown;
  reason: string;
  /** Pre-rendered, risk-ordered explanation of *why* this needs approval. */
  factsSummary: string[];
  target: ToolTarget;
  expiresAt: string;
}

/**
 * "approve_session" only ever widens within the current run, never across runs.
 *
 * Declared as a tuple so `schemas/permission.ts` can write `z.enum(APPROVAL_DECISIONS)`
 * instead of a second hand-copied list — see the note on `HOST_CONTEXT_KINDS`.
 */
export const APPROVAL_DECISIONS = ["approve_once", "approve_session", "reject"] as const;
export type ApprovalDecision = (typeof APPROVAL_DECISIONS)[number];

export interface ApprovalResponse {
  approvalId: string;
  /** Bind the response to the run that displayed the approval. */
  runId: string;
  decision: ApprovalDecision;
  respondedAt: string;
}

/**
 * The stream the UI renders.
 *
 * `policyId` on the two tool events is the policy the Permission Engine actually
 * decided under — carried here so "which policy governed this call" is observable
 * from the stream instead of only inferable from the target environment. Optional
 * for the same reason `AgentRunResult.traceId` is: an older sidecar build does not
 * send it, and the consumer has to keep parsing whatever sidecar is running.
 *
 * `origin` is where the tool came from (`builtin` / `mcp` / `provider`). It is the
 * difference between "the agent ran a tool this build shipped" and "the agent ran a tool a
 * third-party MCP server declared five minutes ago" — an audit trail that cannot tell those
 * apart is not answering the question people actually ask of it (ADR 0014).
 */
export type AgentStreamEvent =
  | { type: "agent.started"; runId: string; at: string }
  | { type: "agent.thinking"; runId: string; textDelta?: string; at: string }
  | { type: "agent.text"; runId: string; textDelta: string; at: string }
  | { type: "agent.usage"; runId: string; usage: AgentTokenUsage; at: string }
  | {
      type: "agent.tool_call";
      runId: string;
      traceId: string;
      stepId: string;
      callId: string;
      toolName: string;
      input: unknown;
      target: ToolTarget;
      riskLevel: RiskLevel;
      decision: PermissionMode;
      approvedBy?: PermissionApprovalSource;
      policyId?: string;
      origin?: ToolOrigin;
      at: string;
    }
  | {
      type: "agent.tool_result";
      runId: string;
      traceId: string;
      stepId: string;
      callId: string;
      toolName: string;
      input: unknown;
      target: ToolTarget;
      riskLevel: RiskLevel;
      decision: PermissionMode;
      approvedBy?: PermissionApprovalSource;
      policyId?: string;
      origin?: ToolOrigin;
      status: ToolResultStatus;
      outputSummary: string;
      error?: string;
      startedAt: string;
      endedAt: string;
      durationMs: number;
      at: string;
    }
  | { type: "agent.waiting_approval"; runId: string; approval: ApprovalRequest; at: string }
  | { type: "agent.approval_expired"; runId: string; approvalId: string; at: string }
  | { type: "agent.completed"; runId: string; result: AgentRunResult; at: string }
  | { type: "agent.failed"; runId: string; error: string; at: string };
