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
    assert.equal(missing.selectedModelLabel, null);
  }
  assert.deepEqual(resolveAgentModels([], null, null).modelChoices, []);
});
