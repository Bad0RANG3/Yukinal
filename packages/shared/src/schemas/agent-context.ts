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

/**
 * What the memory/knowledge retrieval actually did for this turn.
 *
 * The agent projects a long instruction down to a searchable query instead of
 * failing the lookup, and it degrades to recent explicit memories when the index is
 * unusable. Both are legitimate, but neither may be silent: without this the UI can
 * only show *which* sources were used, never that retrieval was projected or
 * degraded — so "no relevant memory" and "retrieval switched off" look identical.
 */
export const AgentMemoryRetrievalSchema = z.strictObject({
  mode: z.enum(["none", "hybrid", "recent-low-confidence"]),
  queryChars: z.number().int().nonnegative(),
  usedChars: z.number().int().nonnegative(),
  projected: z.boolean(),
  degraded: z.boolean(),
  /** Exception class name when `degraded` is true; never a message with data in it. */
  reason: z.string().trim().min(1).max(200).optional(),
});

export const AgentContextUsageSchema = z.strictObject({
  totalTurns: z.number().int().nonnegative(),
  includedTurns: z.number().int().nonnegative(),
  compactedTurns: z.number().int().nonnegative(),
  inputBytes: z.number().int().nonnegative(),
  maxInputBytes: z.number().int().positive(),
  memorySources: z.array(AgentContextSourceSchema).max(12).optional(),
  memoryRetrieval: AgentMemoryRetrievalSchema.optional(),
});

export const AgentMemorySchema = z.strictObject({
  id: IdentifierSchema,
  scope: z.enum(["global", "server"]),
  serverId: SERVER_ID_SCHEMA.optional(),
  title: z.string().trim().min(1).max(200),
  content: z.string().trim().min(1).max(12_000),
  createdAt: z.string().min(1).max(80),
  updatedAt: z.string().min(1).max(80),
  /** Retrieval deadline; absent means the memory never expires and stays listed. */
  expiresAt: z.string().min(1).max(80).optional(),
  /** The memory this row replaced, so a correction keeps its history. */
  supersedes: IdentifierSchema.optional(),
  /** `user` wrote it directly, `model` means the operator approved a proposal. */
  origin: z.enum(["user", "model"]).optional(),
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
  /** A time zone is mandatory: a naive instant would silently never expire. */
  expiresAt: z.string().datetime({ offset: true }).optional(),
  supersedes: IdentifierSchema.optional(),
  origin: z.enum(["user", "model"]).optional(),
}).refine(validMemoryScope, { message: "server memory requires serverId; global memory cannot have one" });

export const AgentMemoryDeleteInputSchema = z.strictObject({
  id: IdentifierSchema,
  scope: z.enum(["global", "server"]),
  serverId: SERVER_ID_SCHEMA.optional(),
}).refine(validMemoryScope, { message: "server memory requires serverId; global memory cannot have one" });
