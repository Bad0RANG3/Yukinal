import { z } from "zod";

import {
  HOST_CONTEXT_KINDS,
  HOST_METHODS,
  type HostToolCancelResponse,
  type HostContextResponse,
  type HostToolExecuteResponse,
} from "../types/host.js";
import { TOOL_ERROR_CODES } from "../types/tool.js";
import { ToolTargetSchema } from "./server.js";

export const HostToolExecuteRequestSchema = z.strictObject({
  callId: z.string().min(1),
  traceId: z.string().min(1),
  toolName: z.string().min(1),
  input: z.unknown(),
  target: ToolTargetSchema,
});

const ToolErrorSchema = z.strictObject({
  code: z.enum(TOOL_ERROR_CODES),
  message: z.string(),
  detail: z.unknown().optional(),
  retryable: z.boolean(),
});

export const HostToolExecuteResponseSchema = z.discriminatedUnion("status", [
  z.strictObject({ status: z.literal("success"), output: z.unknown().optional() }),
  z.strictObject({ status: z.literal("failed"), error: ToolErrorSchema }),
  z.strictObject({ status: z.literal("cancelled"), error: ToolErrorSchema.optional() }),
]) satisfies z.ZodType<HostToolExecuteResponse>;

export const HostToolCancelRequestSchema = z.strictObject({
  requestId: z.number().int().positive(),
});

export const HostToolCancelResponseSchema = z.strictObject({
  cancelled: z.boolean(),
}) satisfies z.ZodType<HostToolCancelResponse>;

export const HostContextRequestSchema = z.strictObject({
  kind: z.enum(HOST_CONTEXT_KINDS),
  id: z.string().min(1).max(160),
});

export const HostContextResponseSchema = z.discriminatedUnion("status", [
  z.strictObject({ status: z.literal("success"), data: z.unknown() }),
  z.strictObject({ status: z.literal("not_found") }),
  z.strictObject({ status: z.literal("failed"), error: ToolErrorSchema }),
]) satisfies z.ZodType<HostContextResponse>;

export const HOST_TOOL_EXECUTE_METHOD = HOST_METHODS.toolExecute;
export const HOST_CONTEXT_FETCH_METHOD = HOST_METHODS.contextFetch;
