import { z } from "zod";

import { HOST_METHODS, type HostToolExecuteResponse } from "../types/host.js";
import { ToolTargetSchema } from "./server.js";

export const HostToolExecuteRequestSchema = z.strictObject({
  callId: z.string().min(1),
  traceId: z.string().min(1),
  toolName: z.string().min(1),
  input: z.unknown(),
  target: ToolTargetSchema,
});

const ToolErrorSchema = z.strictObject({
  code: z.enum([
    "invalid_input",
    "denied_by_policy",
    "approval_rejected",
    "approval_timeout",
    "timeout",
    "cancelled",
    "not_found",
    "transport",
    "execution_failed",
    "internal",
  ]),
  message: z.string(),
  detail: z.unknown().optional(),
  retryable: z.boolean(),
});

export const HostToolExecuteResponseSchema = z.discriminatedUnion("status", [
  z.strictObject({ status: z.literal("success"), output: z.unknown().optional() }),
  z.strictObject({ status: z.literal("failed"), error: ToolErrorSchema }),
  z.strictObject({ status: z.literal("cancelled"), error: ToolErrorSchema.optional() }),
]) satisfies z.ZodType<HostToolExecuteResponse>;

export const HOST_TOOL_EXECUTE_METHOD = HOST_METHODS.toolExecute;
