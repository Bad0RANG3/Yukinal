import { z } from "zod";

import { SERVICE_SOURCES, SERVICE_STATES } from "../types/service.js";

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
