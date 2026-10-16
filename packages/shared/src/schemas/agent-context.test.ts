import assert from "node:assert/strict";
import test from "node:test";

import { AgentRunRequestSchema } from "./permission.js";
import { IPC_SCHEMAS } from "./ipc.js";

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

test("restored messages keep speaker and model metadata without a persona prompt", () => {
  const message = { id: "msg-1", sessionId: "session-1", role: "assistant", content: "Reviewed.",
    participant: { id: "reviewer", label: "Reviewer", kind: "agent" }, model: "test-model", createdAt: "2026-10-17T00:00:00Z" };
  assert.deepEqual(IPC_SCHEMAS.chat_message_append.response.parse({ message }), { message });
  assert.equal(IPC_SCHEMAS.chat_message_append.params.safeParse({ ...message, id: undefined }).success, false);
  const { id, ...input } = message;
  assert.equal(IPC_SCHEMAS.chat_message_append.params.safeParse({ ...input, messageId: id }).success, true);
});
