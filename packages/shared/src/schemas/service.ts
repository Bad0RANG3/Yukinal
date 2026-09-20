import { z } from "zod";

import { SERVICE_SOURCES, SERVICE_STATES } from "../types/service.js";

export const ServerServicesInputSchema = z.strictObject({
  name: z.string().trim().min(1).max(128).regex(/^[A-Za-z0-9][A-Za-z0-9_.@:/-]{0,127}$/, "invalid service or container name").optional(),
  state: z.enum(SERVICE_STATES).optional(),
});

export const ServerServiceSchema = z.strictObject({
  name: z.string().min(1),
  state: z.enum(SERVICE_STATES),
  status: z.string().min(1),
  description: z.string().min(1).optional(),
});

export const ServerServicesResponseSchema = z.strictObject({
  source: z.enum(SERVICE_SOURCES),
  services: z.array(ServerServiceSchema),
  message: z.string().min(1).optional(),
});

const SystemdServiceRefSchema = z
  .string()
  .regex(/^[A-Za-z0-9][A-Za-z0-9_.@:-]{0,119}\.service$/, "invalid systemd service reference");

export const SystemdInspectInputSchema = z.strictObject({
  service: SystemdServiceRefSchema,
});

export const SystemdInspectResultSchema = z.strictObject({
  service: SystemdServiceRefSchema,
  loadState: z.string().min(1),
  activeState: z.string().min(1),
  subState: z.string().min(1),
  description: z.string().min(1).optional(),
});

export const SystemdRestartInputSchema = z.strictObject({
  service: SystemdServiceRefSchema,
  timeoutSeconds: z.number().int().min(1).max(120).optional(),
});

export const SystemdRestartResultSchema = z.strictObject({
  service: SystemdServiceRefSchema,
  restarted: z.boolean(),
});
