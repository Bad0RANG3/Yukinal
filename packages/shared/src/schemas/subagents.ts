import { z } from "zod";
import { SERVER_ID_SCHEMA } from "./server.js";

export const AgentSubagentConfigSchema = z.strictObject({
  version: z.literal(1),
  servers: z.array(z.strictObject({
    id: SERVER_ID_SCHEMA,
    name: z.string().trim().min(1).max(200),
    environment: z.enum(["local", "development", "staging", "production", "unknown"]),
  })).max(128),
});
