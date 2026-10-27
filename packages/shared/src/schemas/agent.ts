/** Runtime gate for agent.stream notifications crossing the sidecar boundary. */

import { z } from "zod";

import { RiskLevelSchema, ToolTargetSchema } from "./server.js";
import { PermissionApprovalSourceSchema, PermissionModeSchema, ToolOriginSchema } from "./permission.js";
import { AGENT_STAGES, TOOL_RESULT_STATUSES } from "../types/enums.js";
import { TOOL_ERROR_CODES } from "../types/tool.js";
import { AgentContextUsageSchema } from "./agent-context.js";

const RunIdSchema = z.string().trim().min(1).max(256);
const TimestampSchema = z.string().min(1).max(80);
const AgentRunStateSchema = z.enum(["idle", "thinking", "running_tool", "waiting_approval", "completed", "failed", "cancelled"]);
/** The stage enum lives in `types/enums.ts` so the wire schema and the union agree. */
const AgentStageSchema = z.enum(AGENT_STAGES);

const ApprovalRequestSchema = z.strictObject({
  approvalId: z.string().trim().min(1).max(256),
  runId: RunIdSchema,
  traceId: RunIdSchema.optional(),
  callId: RunIdSchema.optional(),
  inputFingerprint: z.string().regex(/^[a-fA-F0-9]{64}$/).optional(),
  toolName: z.string().trim().min(1).max(256),
  input: z.unknown(),
  reason: z.string().max(4_000),
  factsSummary: z.array(z.string().max(1_000)).max(32),
  target: ToolTargetSchema,
  expiresAt: TimestampSchema,
  planId: RunIdSchema.optional(),
  planStepId: RunIdSchema.optional(),
  evidenceIds: z.array(RunIdSchema).max(256).optional(),
  sessionGrantable: z.boolean().optional(),
  /**
   * The run is in auto-approval mode: the host minted its one-shot ticket and
   * accepted it without a person. The UI shows this as an executed step, not a
   * pending decision, and never offers buttons for it.
   */
  autoApproved: z.boolean().optional(),
}).superRefine((approval, context) => {
  if (approval.toolName !== "server.exec") return;
  for (const field of ["traceId", "callId", "inputFingerprint"] as const) {
    if (approval[field] === undefined) {
      context.addIssue({
        code: "custom",
        path: [field],
        message: `server.exec approval requires ${field} for host ticket binding`,
      });
    }
  }
  // A durable-task call binds its plan and step; a conversation call has neither.
  // Partial is always wrong: it would be ambiguous which shape the ticket is.
  const hasPlan = approval.planId !== undefined;
  const hasStep = approval.planStepId !== undefined;
  if (hasPlan !== hasStep) {
    context.addIssue({
      code: "custom",
      path: [hasPlan ? "planStepId" : "planId"],
      message: "planId and planStepId must be present together or absent together",
    });
  }
});

const TokenCountSchema = z.number().int().nonnegative().max(Number.MAX_SAFE_INTEGER);
const AgentTokenUsageSchema = z.strictObject({
  inputTokens: TokenCountSchema,
  outputTokens: TokenCountSchema,
  reportedRequests: TokenCountSchema.optional(),
  unreportedRequests: TokenCountSchema.optional(),
  reasoningTokens: TokenCountSchema.optional(),
  reasoningReportedRequests: TokenCountSchema.optional(),
  cacheReadTokens: TokenCountSchema.optional(),
  cacheMissTokens: TokenCountSchema.optional(),
  cacheReportedRequests: TokenCountSchema.optional(),
  lastRequest: z.strictObject({
    durationMs: z.number().finite().nonnegative(),
    firstChunkMs: z.number().finite().nonnegative().optional(),
    firstAnswerMs: z.number().finite().nonnegative().optional(),
    model: z.string().max(256).optional(),
    systemFingerprint: z.string().max(256).optional(),
    finishReason: z.string().max(256).optional(),
    effort: z.enum(["none", "low", "high", "max"]).optional(),
    routing: z.enum(["auto", "manual", "provider"]).optional(),
  }).optional(),
});

export const AgentSubagentTaskSchema = z.strictObject({
  id: RunIdSchema, runId: RunIdSchema, sessionId: RunIdSchema, parentRunId: RunIdSchema,
  title: z.string().min(1).max(160),
  serverId: z.string().regex(/^srv_[a-z0-9]+$/).nullable(),
  access: z.enum(["read", "write"]),
  state: z.enum(["queued", "running", "waiting_approval", "completed", "failed", "cancelled", "interrupted"]),
  createdAt: TimestampSchema, updatedAt: TimestampSchema,
  revision: z.number().int().nonnegative(), lastTool: z.string().max(256).optional(),
  result: z.string().max(12000).optional(), error: z.string().max(4000).optional(),
  usage: AgentTokenUsageSchema.optional(),
  approval: ApprovalRequestSchema.optional(),
});

/**
 * The loop's outcome for one run.
 *
 * Exported because it is not only embedded in `agent.completed`: a `delivery: "sync"`
 * run request answers with the same object, and that crossing (sidecar -> host ->
 * IPC) needs the same runtime gate as the streaming one.
 */
export const AgentRunResultSchema = z.strictObject({
  runId: RunIdSchema,
  state: AgentRunStateSchema,
  text: z.string().max(200_000),
  steps: z.number().int().nonnegative(),
  toolCalls: z.number().int().nonnegative(),
  error: z.string().max(4_000).optional(),
  // Optional on purpose: the loop always sets it, but an older sidecar build does not,
  // and the UI has to keep parsing the events of whatever sidecar is running.
  traceId: z.string().trim().min(1).max(256).optional(),
  context: AgentContextUsageSchema.optional(),
});

/**
 * One schema per event **type**, keyed by the discriminator itself.
 *
 * ## Why this exists instead of only the union
 *
 * These ten members used to be written inline inside `z.discriminatedUnion`, and every
 * `EVENT_SCHEMAS` channel pointed at the whole union. The gate could therefore only ask
 * "is this *some* valid agent event", never "is this *the* event this channel promises".
 * `DesktopEventPayload<"agent.completed">` was consequently the full union, so the UI had
 * to cast nine times to reach the member it knew it was handling — and a cast is exactly
 * where a payload that passed the coarse gate gets handed to a handler as a shape nobody
 * validated (`event.result` is `undefined` for any other member).
 *
 * Deriving the union **from** this map, rather than carving members out of the union,
 * keeps one source of truth: a channel schema and the union member are the same object.
 *
 * This is safe to narrow because of how the emitter names the channel. The Python sidecar
 * publishes an `agent.stream` notification whose `type` is the discriminator
 * (`agent/src/yukinal_agent/sidecar.py:1015-1019`), and the C++ host routes it under that
 * same string (`native/desktop_host/src/ipc_router.cpp:8988-8990`) without rewriting it.
 * The channel and the payload's discriminator are literally the same field, so a payload
 * arriving on `agent.completed` cannot carry `type: "agent.started"`. Per-channel
 * validation therefore cannot reject anything the transport actually delivers — it only
 * stops accepting what the transport cannot produce.
 */
export const AGENT_EVENT_MEMBER_SCHEMAS = {
  "agent.subagent_task": z.strictObject({ type: z.literal("agent.subagent_task"), runId: RunIdSchema, task: AgentSubagentTaskSchema, at: TimestampSchema }),
  "agent.started": z.strictObject({ type: z.literal("agent.started"), runId: RunIdSchema, taskId: RunIdSchema.optional(), at: TimestampSchema }),
  /**
   * Emitted only when the derived stage changes. `previous` is what the run was in
   * before, so a consumer can reconcile a missed frame without replaying history.
   */
  "agent.stage": z.strictObject({
    type: z.literal("agent.stage"),
    runId: RunIdSchema,
    /** Present when the run is bound to a durable investigation task. */
    taskId: RunIdSchema.optional(),
    stage: AgentStageSchema,
    previous: AgentStageSchema,
    at: TimestampSchema,
  }),
  "agent.thinking": z.strictObject({ type: z.literal("agent.thinking"), runId: RunIdSchema, textDelta: z.string().max(20_000).optional(), at: TimestampSchema }),
  "agent.text": z.strictObject({ type: z.literal("agent.text"), runId: RunIdSchema, textDelta: z.string().max(20_000), at: TimestampSchema }),
  "agent.usage": z.strictObject({
    type: z.literal("agent.usage"),
    runId: RunIdSchema,
    usage: AgentTokenUsageSchema,
    at: TimestampSchema,
  }),
  "agent.tool_call": z.strictObject({
    type: z.literal("agent.tool_call"),
    runId: RunIdSchema,
    traceId: z.string().trim().min(1).max(256),
    stepId: z.string().trim().min(1).max(256),
    callId: z.string().trim().min(1).max(256),
    toolName: z.string().trim().min(1).max(256),
    input: z.unknown(),
    target: ToolTargetSchema,
    riskLevel: RiskLevelSchema,
    decision: PermissionModeSchema,
    approvedBy: PermissionApprovalSourceSchema.optional(),
    // Optional on purpose: the loop always sets it, but an older sidecar build does
    // not send it, and the UI has to keep parsing the events of whatever sidecar is
    // running (same reasoning as `traceId` on `AgentRunResultSchema`).
    policyId: z.string().trim().min(1).max(256).optional(),
    /**
     * Where the tool came from. Optional for the same reason `policyId` is: an older
     * sidecar build does not send it. A consumer that *needs* to distinguish a
     * third-party tool from a built-in one must therefore treat "absent" as "unknown"
     * rather than as "builtin".
     */
    origin: ToolOriginSchema.optional(),
    planId: z.string().trim().min(1).max(256).optional(),
    planStepId: z.string().trim().min(1).max(256).optional(),
    evidenceIds: z.array(z.string().trim().min(1).max(256)).max(256).optional(),
    at: TimestampSchema,
  }),
  "agent.tool_result": z.strictObject({
    type: z.literal("agent.tool_result"),
    runId: RunIdSchema,
    traceId: z.string().trim().min(1).max(256),
    stepId: z.string().trim().min(1).max(256),
    callId: z.string().trim().min(1).max(256),
    toolName: z.string().trim().min(1).max(256),
    input: z.unknown(),
    target: ToolTargetSchema,
    riskLevel: RiskLevelSchema,
    decision: PermissionModeSchema,
    approvedBy: PermissionApprovalSourceSchema.optional(),
    /** The policy this decision was made under; see `agent.tool_call` above. */
    policyId: z.string().trim().min(1).max(256).optional(),
    /** See `agent.tool_call` above. */
    origin: ToolOriginSchema.optional(),
    planId: z.string().trim().min(1).max(256).optional(),
    planStepId: z.string().trim().min(1).max(256).optional(),
    evidenceIds: z.array(z.string().trim().min(1).max(256)).max(256).optional(),
    errorCode: z.enum(TOOL_ERROR_CODES).optional(),
    executionState: z.enum(["timed_out", "cancelled", "result_unknown"]).optional(),
    status: z.enum(TOOL_RESULT_STATUSES),
    outputSummary: z.string().max(4_000),
    error: z.string().max(4_000).optional(),
    startedAt: TimestampSchema,
    endedAt: TimestampSchema,
    durationMs: z.number().nonnegative(),
    approvalWaitMs: z.number().nonnegative().optional(),
    at: TimestampSchema,
  }),
  "agent.waiting_approval": z.strictObject({ type: z.literal("agent.waiting_approval"), runId: RunIdSchema, taskId: RunIdSchema.optional(), approval: ApprovalRequestSchema, at: TimestampSchema }),
  "agent.approval_expired": z.strictObject({ type: z.literal("agent.approval_expired"), runId: RunIdSchema, approvalId: RunIdSchema, at: TimestampSchema }),
  "agent.completed": z.strictObject({ type: z.literal("agent.completed"), runId: RunIdSchema, taskId: RunIdSchema.optional(), result: AgentRunResultSchema, at: TimestampSchema }),
  "agent.failed": z.strictObject({ type: z.literal("agent.failed"), runId: RunIdSchema, taskId: RunIdSchema.optional(), error: z.string().max(4_000), at: TimestampSchema }),
} as const;

/**
 * Every member of the stream union, in the map's declared order.
 *
 * Every member has an `EVENT_SCHEMAS` channel, but *not* every member has a producer: this
 * is the shape vocabulary, not a producer census. The Python sidecar emits eight of the
 * ten (`agent.thinking` and `agent.usage` have no emitter anywhere), which the per-name
 * annotations in `../events/index.ts` record and `event-vocabulary.test.ts` pins. The
 * vocabulary remains a separate view so consumers that need the union do not depend on the
 * UI subscription table.
 */
export const AGENT_EVENT_TYPES = Object.keys(AGENT_EVENT_MEMBER_SCHEMAS) as Array<
  keyof typeof AGENT_EVENT_MEMBER_SCHEMAS
>;

/**
 * The whole stream as one discriminated union.
 *
 * Still the right gate for the *sidecar transport*, which receives a notification whose
 * type it does not yet know (`packages/agent-sdk` parses `agent.stream` params before
 * dispatching). Channels use the per-member schemas instead.
 *
 * Built from the map's values, so a member can never exist in one and not the other.
 */
export const AgentStreamEventSchema = z.discriminatedUnion(
  "type",
  Object.values(AGENT_EVENT_MEMBER_SCHEMAS) as [
    (typeof AGENT_EVENT_MEMBER_SCHEMAS)[keyof typeof AGENT_EVENT_MEMBER_SCHEMAS],
    ...Array<(typeof AGENT_EVENT_MEMBER_SCHEMAS)[keyof typeof AGENT_EVENT_MEMBER_SCHEMAS]>,
  ],
);
