import { z } from "zod";

import type { RestartRecord } from "../types/lifecycle.js";

export const RestartRecordSchema = z.strictObject({
  attempt: z.number().int().positive(),
  maxAttempts: z.number().int().positive(),
  exhausted: z.boolean(),
  at: z.string().min(1),
}) satisfies z.ZodType<RestartRecord>;
