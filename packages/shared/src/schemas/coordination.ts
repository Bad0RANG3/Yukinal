import { z } from "zod";
import { SERVER_ID_SCHEMA } from "./server.js";

const TextId = z.string().trim().min(1).max(256);
export const AgentCoordinationConfigSchema = z.strictObject({
  version: z.literal(1),
  roles: z.array(z.strictObject({
    id: TextId,
    team: z.enum(["blue", "red", "purple", "white", "yellow", "orange", "green"]),
    label: z.string().trim().min(1).max(200),
    systemPrompt: z.string().trim().min(1).max(6000),
    providerId: TextId.optional(),
    model: TextId.optional(),
  })).min(1).max(32),
  servers: z.array(z.strictObject({
    id: SERVER_ID_SCHEMA,
    name: z.string().trim().min(1).max(200),
    environment: z.enum(["local", "development", "staging", "production", "unknown"]),
  })).max(128),
  preferredRoleId: TextId.optional(),
});
