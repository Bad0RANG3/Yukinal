import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

import { AgentRunRequestSchema } from "./permission.js";
import { EVENT_SCHEMAS, IPC_SCHEMAS } from "./ipc.js";
import {
  AgentContextSourceSchema,
  AgentContextUsageSchema,
  AgentKnowledgeSourceSchema,
} from "./agent-context.js";

const AGENT_FIXTURE_DIR = new URL("../../fixtures/agent/", import.meta.url);

function agentFixture(name: string): unknown {
  return JSON.parse(readFileSync(new URL(`${name}.json`, AGENT_FIXTURE_DIR), "utf8")) as unknown;
}

test("the active persona crosses both request gates separately from user text", () => {
  const request = {
    runId: "run-1", sessionId: "session-1", prompt: "Review the migration.",
    persona: { id: "reviewer", label: "Reviewer", systemPrompt: "Check evidence and failure paths." },
    participant: { id: "reviewer-1", label: "Reviewer", kind: "subagent", parentId: "lead" },
  };
  assert.deepEqual(AgentRunRequestSchema.parse(request), request);
  assert.deepEqual(IPC_SCHEMAS.agent_run_start.params.parse(request), request);
  assert.equal(AgentRunRequestSchema.safeParse({ ...request, persona: { ...request.persona, systemPrompt: " " } }).success, false);
  assert.equal(AgentRunRequestSchema.safeParse({ ...request, participant: { ...request.participant, kind: "user" } }).success, false);
});

test("saved memory scope is explicit and cannot silently cross servers", () => {
  const save = IPC_SCHEMAS.agent_memory_save.params;
  assert.equal(save.safeParse({ scope: "server", title: "Database", content: "PostgreSQL" }).success, false);
  assert.equal(save.safeParse({ scope: "global", serverId: "srv_alpha", title: "Database", content: "PostgreSQL" }).success, false);
  assert.equal(save.safeParse({ scope: "server", serverId: "srv_alpha", title: "Database", content: "PostgreSQL" }).success, true);
  assert.equal(IPC_SCHEMAS.agent_memory_list.params.safeParse({ serverId: "srv_alpha", query: "Database" }).success, true);
  assert.equal(IPC_SCHEMAS.chat_session_list.params.safeParse({ serverId: null }).success, true);
});

test("memory expiry needs an instant with a time zone and versions name a predecessor", () => {
  const save = IPC_SCHEMAS.agent_memory_save.params;
  const base = { scope: "global", title: "Pool size", content: "48" };
  assert.equal(save.safeParse({ ...base, expiresAt: "2030-01-01T00:00:00Z" }).success, true);
  assert.equal(save.safeParse({ ...base, expiresAt: "2030-01-01T08:00:00+08:00" }).success, true);
  // A naive local instant would compare wrongly against the stored UTC deadline.
  assert.equal(save.safeParse({ ...base, expiresAt: "2030-01-01T00:00:00" }).success, false);
  assert.equal(save.safeParse({ ...base, expiresAt: "tomorrow" }).success, false);
  assert.equal(save.safeParse({ ...base, supersedes: "mem_previous" }).success, true);
  assert.equal(save.safeParse({ ...base, origin: "model" }).success, true);
  assert.equal(save.safeParse({ ...base, origin: "system" }).success, false);

  const memory = IPC_SCHEMAS.agent_memory_save.response;
  const stored = { id: "mem_1", scope: "global", title: "Pool size", content: "48",
    createdAt: "2026-10-17T00:00:00Z", updatedAt: "2026-10-17T00:00:00Z",
    expiresAt: "2030-01-01T00:00:00+00:00", supersedes: "mem_0", origin: "model" };
  assert.equal(memory.safeParse({ memory: stored }).success, true);
  // Older rows predate the columns, so every new field stays optional.
  assert.equal(memory.safeParse({ memory: { id: "mem_2", scope: "global", title: "t", content: "c",
    createdAt: "2026-10-17T00:00:00Z", updatedAt: "2026-10-17T00:00:00Z" } }).success, true);
});

test("restored messages keep speaker and model metadata without a persona prompt", () => {
  const message = { id: "msg-1", sessionId: "session-1", role: "assistant", content: "Reviewed.",
    participant: { id: "reviewer", label: "Reviewer", kind: "agent" }, model: "test-model", createdAt: "2026-10-17T00:00:00Z" };
  assert.deepEqual(IPC_SCHEMAS.chat_message_append.response.parse({ message }), { message });
  assert.equal(IPC_SCHEMAS.chat_message_append.params.safeParse({ ...message, id: undefined }).success, false);
  const { id, ...input } = message;
  assert.equal(IPC_SCHEMAS.chat_message_append.params.safeParse({ ...input, messageId: id }).success, true);
});

/**
 * The payloads below were captured from the Python producer
 * (`agent/src/yukinal_agent/conversation_store.py` `memory_context()`) against a real
 * temporary corpus, not hand-written. `agent/tests/test_knowledge_runtime.py` re-runs the
 * producer and asserts the knowledge entry still equals this fixture, so the two gates
 * here cannot drift away from what the sidecar actually sends.
 */
test("a corpus source crosses the sync run response and the completed event", () => {
  const response = IPC_SCHEMAS.agent_run_start.response.safeParse(agentFixture("run_start_knowledge_sync"));
  assert.equal(response.success, true, "agent_run_start sync response must accept a knowledge source");

  const event = EVENT_SCHEMAS["agent.completed"].safeParse(agentFixture("completed_knowledge_event"));
  assert.equal(event.success, true, "agent.completed must accept the same corpus source");

  const sources = agentFixture("context_knowledge_sources");
  assert.ok(Array.isArray(sources));
  const parsed = AgentContextUsageSchema.parse({
    totalTurns: 2, includedTurns: 2, compactedTurns: 0, inputBytes: 4096, maxInputBytes: 65536,
    memorySources: sources,
  });
  const knowledge = parsed.memorySources?.find((source) => source.scope === "knowledge");
  assert.equal(knowledge?.title, "Nginx timeout guide");
  assert.equal(knowledge?.scope === "knowledge" ? knowledge.chunkId : undefined, "nginx/timeout.md#chunk-0000");
});

test("only the knowledge scope carries section and chunkId", () => {  const sources = agentFixture("context_knowledge_sources") as Array<Record<string, unknown>>;
  const knowledge = sources.find((source) => source.scope === "knowledge")!;
  assert.deepEqual(AgentContextSourceSchema.parse(knowledge), knowledge);
  assert.equal(AgentKnowledgeSourceSchema.safeParse({ ...knowledge, scope: "global" }).success, false);
  assert.equal(AgentKnowledgeSourceSchema.safeParse({ ...knowledge, section: " " }).success, false);
  assert.equal(AgentKnowledgeSourceSchema.safeParse({ ...knowledge, chunkId: "" }).success, false);
  assert.equal(AgentKnowledgeSourceSchema.safeParse({ ...knowledge, text: "leaked body" }).success, false);

  // The corpus scope cannot enter the explicit-memory API, and an explicit memory keeps
  // its two scopes: the union is on the context report, not on memory save/list/delete.
  assert.equal(IPC_SCHEMAS.agent_memory_save.params.safeParse({ scope: "knowledge", title: "t", content: "c" }).success, false);
  assert.equal(IPC_SCHEMAS.agent_memory_list.params.safeParse({ scope: "knowledge" }).success, false);
  assert.equal(AgentContextSourceSchema.safeParse({ id: "mem-1", scope: "global", title: "t", updatedAt: "2026-11-25" }).success, true);
});

/**
 * `memoryRetrieval` exists so a silently disabled lookup cannot look like "no relevant
 * memory": the mode names what ran, and `projected`/`degraded` name the two ways the
 * lookup is not the plain hybrid one.
 */
test("the retrieval report distinguishes projection, degradation and no context", () => {
  const base = { totalTurns: 1, includedTurns: 1, compactedTurns: 0, inputBytes: 10, maxInputBytes: 100 };
  const projected = AgentContextUsageSchema.parse({
    ...base,
    memoryRetrieval: {
      mode: "hybrid", queryChars: 214, usedChars: 52, projected: true, degraded: false,
    },
  });
  assert.equal(projected.memoryRetrieval?.mode, "hybrid");
  assert.equal(projected.memoryRetrieval?.projected, true);

  const degraded = AgentContextUsageSchema.parse({
    ...base,
    memoryRetrieval: {
      mode: "recent-low-confidence", queryChars: 40, usedChars: 40, projected: false,
      degraded: true, reason: "DatabaseError",
    },
  });
  assert.equal(degraded.memoryRetrieval?.reason, "DatabaseError");

  assert.equal(AgentContextUsageSchema.safeParse({
    ...base, memoryRetrieval: { mode: "disabled", queryChars: 1, usedChars: 0, projected: false, degraded: false },
  }).success, false);
  assert.equal(AgentContextUsageSchema.safeParse({
    ...base, memoryRetrieval: { mode: "none", queryChars: 1, usedChars: 0, projected: false, degraded: false, items: [] },
  }).success, false, "the report is strict; a positive field cannot be smuggled in");
  assert.equal(AgentContextUsageSchema.safeParse({
    ...base, memoryRetrieval: { mode: "hybrid", queryChars: 1, usedChars: 0, projected: false, degraded: true, reason: " " },
  }).success, false);
});
