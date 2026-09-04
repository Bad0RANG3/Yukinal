import { z } from "zod";

import { ACTIVITY_OUTCOMES, ACTIVITY_SOURCES, ACTIVITY_TYPES } from "../types/activity.js";

export const ActivitySchema = z.strictObject({
  id: z.string().min(1),
  serverId: z.string().min(1).optional(),
  workspaceId: z.string().min(1).optional(),
  type: z.enum(ACTIVITY_TYPES),
  title: z.string().min(1),
  description: z.string().optional(),
  source: z.enum(ACTIVITY_SOURCES),
  actor: z.string().min(1),
  reason: z.string().optional(),
  outcome: z.enum(ACTIVITY_OUTCOMES).optional(),
  traceId: z.string().min(1).optional(),
  createdAt: z.string().min(1),
});
