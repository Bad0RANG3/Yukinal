/** Sidecar requests that are executed by the Rust host, never by Node. */

import type {
  Evidence,
  EvidenceCorrelationInput,
  EvidenceCorrelationResult,
  EvidenceComparisonInput,
  EvidenceComparisonResult,
  EvidenceSearchInput,
  EvidenceSearchResult,
  InvestigationArtifact,
  InvestigationPlan,
  InvestigationPlanDeviation,
  PlanStepKind,
} from "./investigation.js";
import type { InvestigationRetentionPreview, InvestigationRetentionPreviewInput } from "./retention.js";
import type { ToolError, ToolTarget } from "./tool.js";

export const HOST_METHODS = {
  toolExecute: "host.tool.execute",
  contextFetch: "host.context.fetch",
  toolCancel: "host.tool.cancel",
  /**
   * The MCP tool catalog (ADR 0014). The host owns every child process, so the sidecar
   * cannot discover MCP servers itself: it asks for `host.mcp.catalog` and registers what
   * comes back.
   *
   * A request has no parameters (the host reads its own `mcp_servers` table), and the
   * response is `HostMcpCatalogResponse`.
   */
  mcpCatalog: "host.mcp.catalog",
  evidenceRecord: "host.investigation.evidence.record",
  evidenceFetch: "host.investigation.evidence.fetch",
  evidenceSearch: "host.investigation.evidence.search",
  evidenceCorrelate: "host.investigation.evidence.correlate",
  evidenceCompare: "host.investigation.evidence.compare",
  retentionPreview: "host.investigation.retention.preview",
  findingRecord: "host.investigation.finding.record",
  briefRecord: "host.investigation.brief.record",
  planRecord: "host.investigation.plan.record",
  planCheck: "host.investigation.plan.check",
  planStepResult: "host.investigation.plan.step_result",
  artifactRecord: "host.investigation.artifact.record",
} as const;

export interface HostToolExecuteRequest {
  callId: string;
  /** Present on current sidecars; optional for compatibility with an older sidecar. */
  runId?: string;
  traceId: string;
  toolName: string;
  input: unknown;
  target: ToolTarget;
  taskId?: string;
  planId?: string;
  planStepId?: string;
  evidenceIds?: string[];
  /** Per-call user approval for exact-input tools; task grants are checked host-side. */
  approvalId?: string;
}

/** Cancels a previously sent host.tool.execute request by its JSON-RPC id. */
export interface HostToolCancelRequest {
  requestId: number;
}

export interface HostToolCancelResponse {
  cancelled: boolean;
}

export type HostToolExecuteResponse =
  | { status: "success"; output?: unknown }
  | { status: "failed"; error: ToolError }
  | { status: "cancelled"; error?: ToolError };

export interface HostEvidenceRecordRequest {
  evidence: Evidence;
}

export type HostEvidenceRecordResponse =
  | { recorded: true; evidenceId?: string; reused?: boolean }
  | { recorded: false; error: ToolError };

export interface HostEvidenceFetchRequest {
  taskId: string;
  evidenceId: string;
}

export type HostEvidenceFetchResponse =
  | { status: "success"; evidence: Evidence }
  | { status: "not_found" }
  | { status: "failed"; error: ToolError };

export interface HostEvidenceSearchRequest extends EvidenceSearchInput {
  taskId: string;
}

export type HostEvidenceSearchResponse =
  | ({ status: "success" } & EvidenceSearchResult)
  | { status: "failed"; error: ToolError };

export interface HostEvidenceCorrelationRequest extends EvidenceCorrelationInput {
  taskId: string;
}

export type HostEvidenceCorrelationResponse =
  | ({ status: "success" } & EvidenceCorrelationResult)
  | { status: "failed"; error: ToolError };

export interface HostEvidenceCompareRequest extends EvidenceComparisonInput {
  taskId: string;
}

export type HostEvidenceCompareResponse =
  | ({ status: "success" } & EvidenceComparisonResult)
  | { status: "failed"; error: ToolError };

export type HostRetentionPreviewRequest = InvestigationRetentionPreviewInput;

export type HostRetentionPreviewResponse =
  | { status: "success"; preview: InvestigationRetentionPreview }
  | { status: "failed"; error: ToolError };

export interface HostFindingRecordRequest {
  finding: import("./investigation.js").Finding;
}

export type HostFindingRecordResponse =
  | { recorded: true; finding: import("./investigation.js").Finding }
  | { recorded: false; error: ToolError };

export interface HostBriefRecordRequest {
  brief: import("./investigation.js").DecisionBrief;
}

export type HostBriefRecordResponse =
  | { recorded: true; brief: import("./investigation.js").DecisionBrief }
  | { recorded: false; error: ToolError };

export interface HostPlanRecordRequest {
  plan: InvestigationPlan;
}

export type HostPlanRecordResponse =
  | { recorded: true; plan: InvestigationPlan }
  | { recorded: false; error: ToolError };

export interface HostPlanCheckRequest {
  taskId: string;
  toolName: string;
  input: unknown;
  target: ToolTarget;
}

export type HostPlanCheckResponse =
  | {
      status: "allowed";
      planId: string;
      stepId: string;
      stepKind: PlanStepKind;
      evidenceIds: string[];
      requiresApproval: boolean;
    }
  | { status: "deviation"; deviation: InvestigationPlanDeviation }
  | { status: "failed"; error: ToolError };

export interface HostPlanStepResultRequest {
  taskId: string;
  planId: string;
  stepId: string;
  status: "success" | "failed" | "cancelled";
  retryable: boolean;
  outputSummary?: string;
}

export type HostPlanStepResultResponse =
  | {
      recorded: true;
      plan: InvestigationPlan;
      observation?: "running" | "failed";
      sampleAccepted?: boolean;
      nextSampleAt?: string;
    }
  | { recorded: false; error: ToolError };

export interface HostArtifactRecordRequest {
  artifact: InvestigationArtifact;
  planId?: string;
  planStepId?: string;
  evidenceIds?: string[];
}

export type HostArtifactRecordResponse =
  | { recorded: true; artifact: InvestigationArtifact }
  | { recorded: false; error: ToolError };

/**
 * The tuple exists so `schemas/host.ts` can write `z.enum(HOST_CONTEXT_KINDS)`.
 *
 * It was previously a bare union here plus a hand-written `z.enum([...])` there — two
 * copies of one list with nothing pinning them. Adding a kind to the union would
 * compile, and every real payload carrying it would then be rejected at the IPC gate
 * as a *runtime* failure rather than a compile error. This is the same pattern
 * `types/enums.ts` and `types/risk.ts` already use.
 */
export const HOST_CONTEXT_KINDS = ["server", "snapshot", "workspace", "investigation"] as const;
export type HostContextKind = (typeof HOST_CONTEXT_KINDS)[number];

export interface HostContextRequest {
  kind: HostContextKind;
  /** For snapshot, this is the server id whose latest snapshot is requested. */
  id: string;
}

export type HostContextResponse =
  | { status: "success"; data: unknown }
  | { status: "not_found" }
  | { status: "failed"; error: ToolError };

/* ── MCP catalog (ADR 0014) ────────────────────────────────────────────── */

/**
 * Why one MCP server is not usable right now.
 *
 * This tuple is the TypeScript half of `McpFailureCode::ALL` in
 * `apps/desktop/src-tauri/src/commands/mcp.rs`; a Rust test asserts the two lists agree
 * word for word. Kept as a tuple (not a bare union) for the reason spelled out above
 * `HOST_CONTEXT_KINDS`: `z.enum` needs a value at runtime.
 */
export const MCP_CATALOG_FAILURE_CODES = [
  "disabled",
  "invalid_config",
  "launch_failed",
  /** The process is dead. It is **not** restarted automatically. */
  "exited",
  /** It did not come up inside the catalog request's budget. */
  "timeout",
  "request_failed",
] as const;
export type McpCatalogFailureCode = (typeof MCP_CATALOG_FAILURE_CODES)[number];

/**
 * One MCP tool, as declared by the server over the wire.
 *
 * `name` is the internal name from ADR 0004 (`mcp.<server>.<tool>`) and is what the agent
 * registers; `serverId` is the identity the call is attributed to; `tool` is the name
 * `tools/call` needs. `description` and `inputSchema` are *untrusted* remote declarations —
 * the MCP boundary's rule that description text is always untrusted data (the repository
 * `docs/boundaries/mcp.md`, 「边界：外部工具（MCP）」) is why they are carried through unchanged, never
 * executed or trusted.
 */
export interface HostMcpCatalogTool {
  name: string;
  serverId: string;
  tool: string;
  /** Present only when the remote spelling had to be rewritten; says what the server calls it. */
  remoteName?: string;
  description: string;
  inputSchema: unknown;
  /**
   * The host-owned effective risk (ADR 0074). Optional because an older host does not
   * send it; the agent then keeps every MCP tool at `critical`.
   */
  risk?: "low" | "medium" | "high" | "critical";
}

export interface HostMcpCatalogServer {
  serverId: string;
  /** The normalized name segment inside `name` (ADR 0004). */
  segment: string;
  label: string;
  tools: HostMcpCatalogTool[];
}

export interface HostMcpCatalogFailure {
  serverId: string;
  code: McpCatalogFailureCode;
  message: string;
}

export interface HostMcpCatalogResponse {
  servers: HostMcpCatalogServer[];
  failures: HostMcpCatalogFailure[];
}

/** Body of a successful `mcp.<server>.<tool>` call through `host.tool.execute`. */
export interface HostMcpToolCallOutput {
  serverId: string;
  tool: string;
  /**
   * The tool ran and reported an error. Distinct from a failed call (`HostToolExecuteResponse`
   * `status: "failed"`): the agent adapter turns this into a `ToolFailure`, the host does not.
   */
  isError: boolean;
  /** Text blocks of `content`, joined with a newline. Untrusted. */
  text: string;
  content: HostMcpContentBlock[];
  structuredContent?: unknown;
}

/**
 * A result content block, mirroring `McpContentBlock`
 * (`crates/core/src/mcp/descriptor.rs`, `#[serde(tag = "type")]`).
 *
 * Blocks this repo does not model (image / audio / resource / resource_link) arrive as
 * `other` with the raw value: the host moves content, the adapter decides what to show.
 */
export type HostMcpContentBlock =
  | { type: "text"; text: string }
  | { type: "other"; value: unknown };
