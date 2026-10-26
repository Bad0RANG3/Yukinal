import assert from "node:assert/strict";
import { test } from "node:test";

import { shortModelLabel } from "../src/features/agent/ModelPicker.js";
import { resolveAgentModels } from "../src/features/agent/useAgentModels.js";
import type { AiProviderConfig } from "@yukinal/shared";

/**
 * 关闭态的模型名是底栏最容易被挤坏的一格：它必须在短名里把
 * "vendor/" 前缀去掉，否则原生长度会直接把这一排顶到第二行。
 */
test("shortModelLabel strips the vendor prefix so the closed control stays readable", () => {
  assert.equal(shortModelLabel("anthropic/claude-sonnet-4.5"), "claude-sonnet-4.5");
  assert.equal(shortModelLabel("google/gemini-3-pro-preview"), "gemini-3-pro-preview");
  assert.equal(shortModelLabel("deepseek/deepseek-v4-flash"), "deepseek-v4-flash");
});

test("shortModelLabel keeps names without a vendor prefix intact", () => {
  assert.equal(shortModelLabel("glm-5.2"), "glm-5.2");
  assert.equal(shortModelLabel("Kimi K3 (2x usage)"), "Kimi K3 (2x usage)");
});

test("shortModelLabel takes the segment after the last slash", () => {
  assert.equal(shortModelLabel("a/b/c"), "c");
});

/** 空名字绝不能被喂给按钮：那会渲染出一个读不出内容的控件。 */
test("shortModelLabel falls back to the original when stripping yields nothing", () => {
  assert.equal(shortModelLabel(""), "");
  assert.equal(shortModelLabel("vendor/"), "vendor/");
});

test("model resolution keeps enabled provider order, catalog labels and legacy fallback", () => {
  const provider = (id: string, enabled: boolean, models?: AiProviderConfig["models"]): AiProviderConfig => ({
    id, enabled, models, kind: "openai-compatible", label: id, model: "legacy:model",
    baseUrl: "http://localhost/v1", createdAt: "", updatedAt: "",
  });
  const disabled = provider("disabled", false);
  const legacy = provider("legacy", true, []);
  const catalog = provider("catalog", true, [
    { id: "model:one", label: "Friendly name", supportsStreaming: true, supportsToolCalling: true },
    { id: "model:two", label: "", supportsStreaming: true, supportsToolCalling: false },
  ]);
  const providers = [disabled, legacy, catalog];
  const result = resolveAgentModels(providers, "catalog", "model:one");
  assert.equal(result.fallback, legacy);
  assert.equal(result.selectedProvider, catalog);
  assert.equal(result.selectedModelLabel, "catalog · Friendly name");
  assert.deepEqual(result.modelChoices.map(({ key, label }) => [key, label]), [
    ["legacy:legacy:model", "legacy:model"], ["catalog:model:one", "Friendly name"], ["catalog:model:two", "model:two"],
  ]);
  for (const id of [null, "deleted", "disabled"]) {
    const missing = resolveAgentModels(providers, id, "legacy:model");
    assert.equal(missing.selectedProvider, undefined);
    assert.equal(missing.selectedModelKey, null);
    assert.equal(missing.selectedModelLabel, null);
  }
  assert.deepEqual(resolveAgentModels([], null, null).modelChoices, []);
});

const deepseek: AiProviderConfig = {
  id: "deepseek", enabled: true, kind: "openai-compatible", label: "DeepSeek", model: "deepseek-chat",
  baseUrl: "https://api.example.test/v1", createdAt: "", updatedAt: "",
  models: [{ id: "deepseek-flash", label: "deepseek-flash", supportsStreaming: true, supportsToolCalling: true }],
};

test("a current model outside a refreshed catalog remains the same request, key and display without changing saved providers", () => {
  const providers = [structuredClone(deepseek)];
  const before = structuredClone(providers);
  const modelOverride = "deepseek-chat";
  const result = resolveAgentModels(providers, "deepseek", modelOverride);
  assert.equal(result.selectedModelKey, "deepseek:deepseek-chat");
  assert.equal(result.selectedModelLabel, "DeepSeek · deepseek-chat");
  assert.equal(result.modelChoices.find((choice) => choice.key === result.selectedModelKey)?.model, modelOverride ?? result.selectedProvider?.model);
  assert.deepEqual(result.modelChoices.map((choice) => choice.model), ["deepseek-flash", "deepseek-chat"]);
  assert.deepEqual(providers, before, "the temporary current option must not write or mutate the provider catalog");
});

test("a null override displays the actual provider default rather than the first catalog item", () => {
  const result = resolveAgentModels([deepseek], "deepseek", null);
  assert.equal(result.selectedModelKey, "deepseek:deepseek-chat");
  assert.equal(result.selectedModelLabel, "DeepSeek · deepseek-chat");
  assert.equal(result.modelChoices.find((choice) => choice.key === result.selectedModelKey)?.model, result.selectedProvider?.model);
  const known = resolveAgentModels([deepseek], "deepseek", "deepseek-flash");
  assert.equal(known.modelChoices.length, 1, "an existing catalog item must not be duplicated");
  assert.equal(known.selectedModelLabel, "DeepSeek · deepseek-flash");
});

test("transient overrides bind to the exact enabled provider and retain model IDs containing colons", () => {
  const other = { ...deepseek, id: "other", label: "Other" };
  const result = resolveAgentModels([deepseek, other], "other", "vendor/custom:rev2");
  const selected = result.modelChoices.find((choice) => choice.key === result.selectedModelKey);
  assert.equal(selected?.providerId, "other");
  assert.equal(selected?.model, "vendor/custom:rev2");
  assert.equal(result.selectedModelLabel, "Other · vendor/custom:rev2");
  for (const id of ["deleted", "deepseek"]) {
    const unavailable = resolveAgentModels([{ ...deepseek, enabled: false }, other], id, "old-model");
    assert.equal(unavailable.selectedModelKey, null);
    assert.equal(unavailable.modelChoices.some((choice) => choice.model === "old-model"), false);
  }
});

test("the live conversation model may sit outside the refreshed catalog without changing the saved provider", () => {
  // The role catalog's separate binding rules were removed with role routing, but this
  // property still matters: `resolveAgentModels` must keep naming the model the user
  // actually selected even when the provider's refreshed list no longer includes it,
  // and it must not mutate or extend the provider record to do so.
  const before = structuredClone(deepseek);
  const current = resolveAgentModels([deepseek], "deepseek", "deepseek-chat");
  assert.ok(current.modelChoices.some((choice) => choice.model === "deepseek-chat"));
  assert.deepEqual(deepseek, before, "reading a selection must not rewrite the saved catalog");
});
