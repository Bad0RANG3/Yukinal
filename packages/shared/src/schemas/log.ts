import { z } from "zod";

import { LOG_LEVELS, LOG_SOURCES } from "../types/log.js";

const SystemdUnitSchema = z
  .string()
  .trim()
  .regex(/^[A-Za-z0-9][A-Za-z0-9_.@:-]{0,119}\.service$/, "invalid systemd service unit");

export const ServerLogsInputSchema = z.strictObject({
  sinceSeconds: z.number().int().min(1).max(86_400).optional(),
  unit: SystemdUnitSchema.optional(),
});

export const ServerLogLineSchema = z.strictObject({
  text: z.string().min(1),
  level: z.enum(LOG_LEVELS),
});

export const ServerLogsResponseSchema = z.strictObject({
  source: z.enum(LOG_SOURCES),
  lines: z.array(ServerLogLineSchema),
  message: z.string().min(1).optional(),
});
