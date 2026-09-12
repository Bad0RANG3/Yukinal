/** Runtime gate for agent.stream notifications crossing the sidecar boundary. */

import { z } from "zod";

import { RiskLevelSchema, ToolTargetSchema } from "./server.js";
import { PermissionApprovalSourceSchema, PermissionModeSchema } from "./permission.js";
import { TOOL_RESULT_STATUSES } from "../types/enums.js";

const RunIdSchema = z.string().trim().min(1).max(256);
const TimestampSchema = z.string().min(1).max(80);
const AgentRunStateSchema = z.enum(["idle", "thinking", "running_tool", "waiting_approval", "completed", "failed", "cancelled"]);

const ApprovalRequestSchema = z.strictObject({
  approvalId: z.string().trim().min(1).max(256),
  runId: RunIdSchema,
  toolName: z.string().trim().min(1).max(256),
  input: z.unknown(),
  reason: z.string().max(4_000),
  factsSummary: z.array(z.string().max(1_000)).max(32),
  target: ToolTargetSchema,
  expiresAt: TimestampSchema,
});

/**
 * The loop's outcome for one run.
 *
 * Exported because it is not only embedded in `agent.completed`: a `delivery: "sync"`
 * run request answers with the same object, and that crossing (sidecar -> Rust ->
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
});

/**
 * One schema per event **type**, keyed by the discriminator itself.
 *
 * ## Why this exists instead of only the union
 *
 * These nine members used to be written inline inside `z.discriminatedUnion`, and every
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
 * This is safe to narrow because of how the Rust side emits, verified at
 * `apps/desktop/src-tauri/src/commands/mod.rs:470,504`: the Tauri channel name is read
 * from `params.type` and emitted as `tauri_event_name(event_type)`. The channel and the
 * payload's discriminator are literally the same field, so a payload arriving on
 * `agent.completed` cannot carry `type: "agent.started"`. Per-channel validation
 * therefore cannot reject anything the transport actually delivers — it only stops
 * accepting what the transport cannot produce.
 */
export const AGENT_EVENT_MEMBER_SCHEMAS = {
  "agent.started": z.strictObject({ type: z.literal("agent.started"), runId: RunIdSchema, at: TimestampSchema }),
  "agent.thinking": z.strictObject({ type: z.literal("agent.thinking"), runId: RunIdSchema, textDelta: z.string().max(20_000).optional(), at: TimestampSchema }),
  "agent.text": z.strictObject({ type: z.literal("agent.text"), runId: RunIdSchema, textDelta: z.string().max(20_000), at: TimestampSchema }),
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
    status: z.enum(TOOL_RESULT_STATUSES),
    outputSummary: z.string().max(4_000),
    error: z.string().max(4_000).optional(),
    startedAt: TimestampSchema,
    endedAt: TimestampSchema,
    durationMs: z.number().nonnegative(),
    at: TimestampSchema,
  }),
  "agent.waiting_approval": z.strictObject({ type: z.literal("agent.waiting_approval"), runId: RunIdSchema, approval: ApprovalRequestSchema, at: TimestampSchema }),
  "agent.approval_expired": z.strictObject({ type: z.literal("agent.approval_expired"), runId: RunIdSchema, approvalId: RunIdSchema, at: TimestampSchema }),
  "agent.completed": z.strictObject({ type: z.literal("agent.completed"), runId: RunIdSchema, result: AgentRunResultSchema, at: TimestampSchema }),
  "agent.failed": z.strictObject({ type: z.literal("agent.failed"), runId: RunIdSchema, error: z.string().max(4_000), at: TimestampSchema }),
} as const;

/**
 * Every member of the stream union, in the map's declared order.
 *
 * Note this is the *vocabulary*, not the set of channels: it includes `agent.text`,
 * which is a valid member with a full schema but has no `EVENT_SCHEMAS` entry and no
 * producer (see `event-vocabulary.test.ts`). Callers that mean "channels" must filter.
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
