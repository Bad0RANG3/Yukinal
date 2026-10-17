import assert from "node:assert/strict";
import test from "node:test";
import { IPC_SCHEMAS, AgentRunRequestSchema, type AiProviderConfig } from "@yukinal/shared";
import { reasoningLevels, requestReasoningEffort } from "../src/features/agent/reasoning.js";

const deepseek = { kind: "openai-compatible", baseUrl: "https://api.deepseek.com", wireApi: "chat" } as AiProviderConfig;
test("reasoning controls follow the selected endpoint, protocol and model", () => {
  assert.deepEqual(reasoningLevels(deepseek, "deepseek-flash"), ["none", "low", "high", "max"]);
  assert.deepEqual(reasoningLevels(deepseek, "deepseek-v4-pro"), ["none", "low", "high", "max"]);
  for (const provider of [{ ...deepseek, baseUrl: "http://127.0.0.1:1234/v1" }, { ...deepseek, wireApi: "responses" as const }, { ...deepseek, kind: "anthropic" as const }]) assert.deepEqual(reasoningLevels(provider, "deepseek-flash"), []);
  assert.deepEqual(reasoningLevels(deepseek, "unknown-model"), []);
  assert.deepEqual(reasoningLevels({ ...deepseek, baseUrl: "http://api.deepseek.com" }, "deepseek-flash"), []);
});
test("default effort and unsupported model switches omit the API override", () => {
  assert.equal(requestReasoningEffort(deepseek, "deepseek-flash", null), undefined);
  assert.equal(requestReasoningEffort(deepseek, "deepseek-flash", "none"), "none");
  assert.equal(requestReasoningEffort(deepseek, "deepseek-flash", "max"), "max");
  assert.equal(requestReasoningEffort(deepseek, "other", "max"), undefined);
});

test("run contracts preserve supported effort levels and reject invalid overrides", () => {
  const request = { runId: "run_effort", sessionId: "ses_effort", prompt: "hello" };
  for (const schema of [AgentRunRequestSchema, IPC_SCHEMAS.agent_run_start.params]) {
    for (const reasoningEffort of ["none", "low", "high", "max"] as const) assert.equal(schema.parse({ ...request, reasoningEffort }).reasoningEffort, reasoningEffort);
    for (const reasoningEffort of [null, "ultra", 1, {}, []]) assert.equal(schema.safeParse({ ...request, reasoningEffort }).success, false);
    assert.equal(schema.parse(request).reasoningEffort, undefined);
  }
});
