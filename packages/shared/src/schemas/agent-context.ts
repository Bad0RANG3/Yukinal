import { z } from "zod";
import { SERVER_ID_SCHEMA } from "./server.js";

const IdentifierSchema = z.string().trim().min(1).max(256);

export const AgentParticipantSchema = z.strictObject({
  id: IdentifierSchema,
  label: z.string().trim().min(1).max(200),
  kind: z.enum(["agent", "subagent"]),
  parentId: IdentifierSchema.optional(),
}).refine((participant) => participant.kind === "subagent"
  ? Boolean(participant.parentId) && participant.parentId !== participant.id
  : participant.parentId === undefined,
{ message: "a subagent needs a distinct parent; a lead cannot have a parent" });

export const AgentPersonaSchema = z.strictObject({
  id: IdentifierSchema,
  label: z.string().trim().min(1).max(200),
  systemPrompt: z.string().trim().min(1).max(20_000),
});

/**
 * One explicit memory as `context.memorySources` reports it.
 *
 * Scope stays `global | server` on purpose: explicit memories are the only thing the
 * save/list/delete API accepts, and `AgentMemorySchema` below reuses this shape.
 */
export const AgentMemorySourceSchema = z.strictObject({
  id: IdentifierSchema,
  scope: z.enum(["global", "server"]),
  serverId: SERVER_ID_SCHEMA.optional(),
  title: z.string().trim().min(1).max(200),
  updatedAt: z.string().min(1).max(80),
});

/**
 * One curated-corpus passage reported beside explicit memories.
 *
 * `agent/src/yukinal_agent/conversation_store.py` (`memory_context`) emits these with
 * `scope: "knowledge"` plus the passage's `section` and `chunkId`. The scope is a
 * literal so a corpus passage can never pass as, or be promoted into, an explicit
 * memory — the two are different databases with different lifecycles.
 */
export const AgentKnowledgeSourceSchema = z.strictObject({
  id: IdentifierSchema,
  scope: z.literal("knowledge"),
  serverId: SERVER_ID_SCHEMA.optional(),
  title: z.string().trim().min(1).max(200),
  updatedAt: z.string().min(1).max(80),
  section: z.string().trim().min(1).max(2_000),
  chunkId: z.string().trim().min(1).max(512),
});

export const AgentContextSourceSchema = z.union([AgentMemorySourceSchema, AgentKnowledgeSourceSchema]);

export const AgentContextUsageSchema = z.strictObject({
  totalTurns: z.number().int().nonnegative(),
  includedTurns: z.number().int().nonnegative(),
  compactedTurns: z.number().int().nonnegative(),
  inputBytes: z.number().int().nonnegative(),
  maxInputBytes: z.number().int().positive(),
  memorySources: z.array(AgentContextSourceSchema).max(12).optional(),
});

export const AgentMemorySchema = z.strictObject({
  id: IdentifierSchema,
  scope: z.enum(["global", "server"]),
  serverId: SERVER_ID_SCHEMA.optional(),
  title: z.string().trim().min(1).max(200),
  content: z.string().trim().min(1).max(12_000),
  createdAt: z.string().min(1).max(80),
  updatedAt: z.string().min(1).max(80),
});

function validMemoryScope(value: { scope?: "global" | "server"; serverId?: string }): boolean {
  return value.scope === "server" ? Boolean(value.serverId) : value.scope !== "global" || !value.serverId;
}

export const AgentMemoryListInputSchema = z.strictObject({
  scope: z.enum(["global", "server"]).optional(),
  serverId: SERVER_ID_SCHEMA.optional(),
  query: z.string().trim().max(200).optional(),
  limit: z.number().int().min(1).max(100).optional(),
}).refine(validMemoryScope, { message: "server memory requires serverId; global memory cannot have one" });

export const AgentMemorySaveInputSchema = z.strictObject({
  id: IdentifierSchema.optional(),
  scope: z.enum(["global", "server"]),
  serverId: SERVER_ID_SCHEMA.optional(),
  title: z.string().trim().min(1).max(200),
  content: z.string().trim().min(1).max(12_000),
}).refine(validMemoryScope, { message: "server memory requires serverId; global memory cannot have one" });

export const AgentMemoryDeleteInputSchema = z.strictObject({
  id: IdentifierSchema,
  scope: z.enum(["global", "server"]),
  serverId: SERVER_ID_SCHEMA.optional(),
}).refine(validMemoryScope, { message: "server memory requires serverId; global memory cannot have one" });
