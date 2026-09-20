import { createHash } from "node:crypto";

import {
  INVESTIGATION_LIMITS,
  type Evidence,
  type EvidenceContentType,
  type EvidenceKind,
  type ToolTarget,
} from "@yukinal/shared";

import { redactSensitiveText, redactSensitiveValue } from "../security/sensitive-data.js";

export interface EvidenceInput {
  taskId: string;
  target: ToolTarget;
  toolName: string;
  input: unknown;
  output: unknown;
  collectedAt: string;
}

/**
 * Turn a successful read-only result into the bounded, redacted object the host may persist.
 * The host repeats the size, hash and redaction checks; this adapter keeps oversized results
 * from becoming a second copy of the model context before that boundary is reached.
 */
export function buildEvidence(input: EvidenceInput): Evidence {
  const raw = jsonSafe(input.output);
  const redacted = jsonSafe(redactSensitiveValue(raw));
  const redactionStatus = JSON.stringify(raw) === JSON.stringify(redacted) ? "clean" : "redacted";
  const initialSerialized = stableJson(redacted);
  const initialBytes = byteLength(initialSerialized);
  const truncated = initialBytes > INVESTIGATION_LIMITS.maxEvidenceSerializedBytes;
  const content = truncated
    ? {
        preview: redactSensitiveText(summarizeForEvidence(redacted)),
        originalBytes: initialBytes,
        truncated: true,
      }
    : redacted;
  const serialized = stableJson(content);

  return {
    id: `ev_${randomId()}`,
    taskId: input.taskId,
    scope: input.target,
    kind: evidenceKind(input.toolName),
    sourceTool: input.toolName,
    collectedAt: input.collectedAt,
    inputSummary: summarizeForEvidence(redactSensitiveValue(input.input)),
    contentType: (typeof content === "string" ? "text" : "json") satisfies EvidenceContentType,
    content,
    contentHash: createHash("sha256").update(serialized).digest("hex"),
    truncated,
    redactionStatus,
  };
}

function evidenceKind(toolName: string): EvidenceKind {
  if (toolName === "server.info" || toolName === "server.snapshot") return "snapshot";
  if (toolName.includes("logs")) return "log";
  if (toolName.includes("service")) return "service";
  if (toolName.startsWith("docker.")) return "container";
  if (toolName.startsWith("filesystem.")) return "file";
  return "tool_result";
}

function jsonSafe(value: unknown): unknown {
  if (value === undefined) return null;
  try {
    return JSON.parse(JSON.stringify(value)) as unknown;
  } catch {
    return { error: "output was not JSON serializable" };
  }
}

/**
 * Keep the hash independent of JavaScript object insertion order. Rust's serde_json map orders
 * String keys by their UTF-8 bytes; using localeCompare here would make the digest locale-
 * dependent and could disagree for non-ASCII keys, so the comparator deliberately follows byte
 * order.
 */
function stableJson(value: unknown): string {
  return JSON.stringify(canonicalize(value)) ?? "null";
}

function canonicalize(value: unknown): unknown {
  if (Array.isArray(value)) return value.map(canonicalize);
  if (value !== null && typeof value === "object") {
    return Object.fromEntries(
      Object.entries(value as Record<string, unknown>)
        .sort(([left], [right]) => Buffer.from(left, "utf8").compare(Buffer.from(right, "utf8")))
        .map(([key, nested]) => [key, canonicalize(nested)]),
    );
  }
  return value;
}

function summarizeForEvidence(value: unknown): string {
  const serialized = typeof value === "string" ? value : JSON.stringify(value) ?? String(value);
  return serialized.length > INVESTIGATION_LIMITS.maxEvidenceInputSummaryChars
    ? `${serialized.slice(0, INVESTIGATION_LIMITS.maxEvidenceInputSummaryChars)}…[truncated]`
    : serialized;
}

function byteLength(value: string): number {
  return Buffer.byteLength(value, "utf8");
}

function randomId(): string {
  return createHash("sha256")
    .update(`${Date.now()}-${Math.random()}-${Math.random()}`)
    .digest("hex")
    .slice(0, 24);
}
