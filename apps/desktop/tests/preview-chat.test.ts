import assert from "node:assert/strict";
import test from "node:test";
import { IPC_COMMANDS, IPC_SCHEMAS } from "@yukinal/shared";
import { nativePreviewChatParams, usesNativePreviewChat } from "../src/lib/preview-chat.js";

test("browser fixtures never invoke native commands and native fixtures forward only chat", () => {
  for (const command of Object.values(IPC_COMMANDS)) assert.equal(usesNativePreviewChat(command, false), false);
  for (const command of ["server_add", "terminal_write", "server_connect"] as const) {
    assert.equal(usesNativePreviewChat(command, true), false);
  }
  assert.equal(usesNativePreviewChat("agent_run_start", true), true);
  assert.equal(usesNativePreviewChat("provider_list", true), true);
});

test("preview chat removes mock targets and enforces no tools at the IPC boundary", () => {
  const input = { sessionId: "real-session", prompt: "hello", taskId: "mock-task", focusServerId: "srv_mock", workspaceId: "mock-workspace", target: { serverId: "srv_mock" }, policyId: "development", mode: "goal", permissionMode: "auto", model: "selected-model", reasoningEffort: "high" };
  const clean = nativePreviewChatParams("agent_run_start", input);
  assert.deepEqual(clean, { sessionId: "real-session", prompt: "hello", mode: "readonly", permissionMode: "ask", model: "selected-model", reasoningEffort: "high", toolAccess: "none" });
  assert.equal(IPC_SCHEMAS.agent_run_start.params.safeParse(clean).success, true);
  assert.equal(input.focusServerId, "srv_mock");
  assert.deepEqual(nativePreviewChatParams("chat_session_create", { title: "hello", serverId: "srv_mock" }), { title: "hello" });
  assert.deepEqual(nativePreviewChatParams("agent_context_reset", { sessionId: "real-session", focusServerId: "srv_mock" }), { sessionId: "real-session" });
  assert.equal(IPC_SCHEMAS.agent_run_start.params.safeParse({ ...clean, toolAccess: "all" }).success, false);
});
