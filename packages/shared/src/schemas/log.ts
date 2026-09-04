import { z } from "zod";

import { LOG_LEVELS, LOG_SOURCES } from "../types/log.js";

export const ServerLogLineSchema = z.strictObject({
  text: z.string().min(1),
  level: z.enum(LOG_LEVELS),
});

export const ServerLogsResponseSchema = z.strictObject({
  source: z.enum(LOG_SOURCES),
  lines: z.array(ServerLogLineSchema),
  message: z.string().min(1).optional(),
});
