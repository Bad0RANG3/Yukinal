import assert from "node:assert/strict";
import test from "node:test";
import { renderToStaticMarkup } from "react-dom/server";
import type { AiProviderConfig } from "@yukinal/shared";

import { ModelPicker } from "../src/features/agent/ModelPicker.js";
import { ReasoningPicker } from "../src/features/agent/ReasoningPicker.js";
import { resolveAgentModels } from "../src/features/agent/useAgentModels.js";

const provider: AiProviderConfig = {
  id: "deepseek", enabled: true, kind: "openai-compatible", label: "DeepSeek", model: "deepseek-chat",
  baseUrl: "https://api.example.test/v1", createdAt: "", updatedAt: "",
  models: [{ id: "deepseek-flash", label: "deepseek-flash", supportsStreaming: true, supportsToolCalling: true }],
};

test("the real reasoning button and inline model list agree with an out-of-catalog request model", () => {
  for (const override of ["deepseek-chat", null]) {
    const resolved = resolveAgentModels([provider], provider.id, override);
    const calls: string[] = [];
    const trigger = renderToStaticMarkup(<ReasoningPicker choices={resolved.modelChoices} selectedModelKey={resolved.selectedModelKey} selectedModelTitle={resolved.selectedModelLabel} onSelectModel={(key) => calls.push(key)} levels={[]} value={null} disabled={false} onChange={() => {}} />);
    assert.match(trigger, /class="agent-model-value">deepseek-chat<\/span>/);
    assert.match(trigger, /aria-label="模型与推理强度：deepseek-chat"/);
    assert.match(trigger, /title="DeepSeek · deepseek-chat"/);
    assert.doesNotMatch(trigger, /deepseek-flash/);
    const options = renderToStaticMarkup(<ModelPicker inline choices={resolved.modelChoices} selectedKey={resolved.selectedModelKey} selectedTitle={resolved.selectedModelLabel} onSelect={(key) => calls.push(key)} />);
    assert.match(options, /<li[^>]*aria-selected="true"[^>]*>[\s\S]*?deepseek-chat/);
    assert.match(options, /<li[^>]*aria-selected="false"[^>]*>[\s\S]*?deepseek-flash/);
    assert.equal((options.match(/aria-selected="true"/g) ?? []).length, 1);
    assert.deepEqual(calls, [], "rendering must not select or replace a model");
  }
});

test("unmatched or missing keys never advertise the first choice as selected", () => {
  const choices = [{ key: "deepseek:deepseek-flash", label: "deepseek-flash" }];
  for (const selectedKey of ["deepseek:old-model", null]) {
    const button = renderToStaticMarkup(<ModelPicker choices={choices} selectedKey={selectedKey} selectedTitle={null} onSelect={() => {}} />);
    assert.match(button, /class="agent-model-value">选择模型<\/span>/);
    assert.doesNotMatch(button, /deepseek-flash/);
    const inline = renderToStaticMarkup(<ModelPicker inline choices={choices} selectedKey={selectedKey} selectedTitle={null} onSelect={() => {}} />);
    assert.doesNotMatch(inline, /aria-selected="true"/);
    assert.match(inline, /aria-selected="false"/);
    const reasoning = renderToStaticMarkup(<ReasoningPicker choices={choices} selectedModelKey={selectedKey} selectedModelTitle={null} onSelectModel={() => {}} levels={[]} value={null} disabled={false} onChange={() => {}} />);
    assert.match(reasoning, /模型与推理强度：选择模型/);
    assert.doesNotMatch(reasoning, /deepseek-flash/);
  }
});

test("a same-named model under another provider is not marked as the selected option", () => {
  const choices = [{ key: "first:shared:model", label: "vendor/shared", providerLabel: "First" }, { key: "second:shared:model", label: "vendor/shared", providerLabel: "Second" }];
  const markup = renderToStaticMarkup(<ModelPicker inline choices={choices} selectedKey="second:shared:model" selectedTitle="Second · shared:model" onSelect={() => {}} />);
  const rows = markup.match(/<li\b[\s\S]*?<\/li>/g) ?? [];
  assert.equal(rows.length, 2);
  assert.match(rows[0]!, /aria-selected="false"/);
  assert.match(rows[1]!, /aria-selected="true"/);
});

test("empty catalogs retain the existing hidden-picker behavior", () => {
  assert.equal(renderToStaticMarkup(<ModelPicker choices={[]} selectedKey={null} selectedTitle={null} onSelect={() => {}} />), "");
  assert.equal(renderToStaticMarkup(<ReasoningPicker choices={[]} selectedModelKey={null} selectedModelTitle={null} onSelectModel={() => {}} levels={[]} value={null} disabled={false} onChange={() => {}} />), "");
});
