import assert from "node:assert/strict";
import test from "node:test";
import { renderToStaticMarkup } from "react-dom/server";

import { activeModelChoice, ModelPicker, modelPickerKeyIntent, type ModelPickerChoice } from "../src/features/agent/ModelPicker.js";
import { ReasoningPicker } from "../src/features/agent/ReasoningPicker.js";

const choices: readonly ModelPickerChoice[] = [
  { key: "one:a", label: "Alpha" },
  { key: "one:b", label: "Beta" },
  { key: "two:c", label: "Gamma" },
];
const open = { open: true, inline: false, disabled: false };
const closed = { ...open, open: false };
const embedded = { ...closed, inline: true };

test("the first inline render points keyboard focus to the actual selected option", () => {
  const markup = renderToStaticMarkup(<ModelPicker inline choices={choices} selectedKey="two:c" selectedTitle="Two · Gamma" onSelect={() => {}} />);
  const activeId = /aria-activedescendant="([^"]+)"/.exec(markup)?.[1];
  const selectedId = /<li id="([^"]+)" role="option" aria-selected="true"/.exec(markup)?.[1];
  assert.ok(activeId);
  assert.equal(activeId, selectedId, "the first focused listbox must not briefly announce the first catalog entry");
  assert.match(markup, /tabindex="0"/);
});

test("a disabled inline list is inert, leaves the tab order and exposes its disabled state", () => {
  let calls = 0;
  const markup = renderToStaticMarkup(<ModelPicker inline disabled choices={choices} selectedKey="one:b" selectedTitle={null} onSelect={() => { calls++; }} />);
  assert.match(markup, /role="listbox"/);
  assert.match(markup, /aria-disabled="true"/);
  assert.match(markup, /inert=""/);
  assert.match(markup, /tabindex="-1"/);
  assert.doesNotMatch(markup, /aria-activedescendant=/);
  assert.equal(calls, 0);
});

test("closed comboboxes do not point to an absent list or option", () => {
  const markup = renderToStaticMarkup(<ModelPicker choices={choices} selectedKey="one:b" selectedTitle={null} onSelect={() => {}} />);
  assert.match(markup, /role="combobox"/);
  assert.match(markup, /aria-expanded="false"/);
  assert.doesNotMatch(markup, /aria-controls=|aria-activedescendant=|role="listbox"/);
  assert.match(markup, /class="agent-model-value">Beta<\/span>/);
});

test("highlights follow model keys across reordering and recover from removal before navigating", () => {
  const highlight = { choiceKey: "two:c", selectedKey: "one:b" };
  const reordered = [choices[2]!, choices[0]!, choices[1]!];
  assert.equal(activeModelChoice(reordered, "one:b", highlight)?.key, "two:c");
  assert.deepEqual(modelPickerKeyIntent(reordered, "two:c", "ArrowDown", open), { type: "highlight", key: "one:a" });
  const shortened = choices.slice(0, 2);
  const active = activeModelChoice(shortened, "one:b", highlight);
  assert.equal(active?.key, "one:b");
  assert.deepEqual(modelPickerKeyIntent(shortened, active?.key ?? null, "ArrowDown", open), { type: "highlight", key: "one:a" });
  assert.deepEqual(modelPickerKeyIntent(shortened, active?.key ?? null, "ArrowUp", open), { type: "highlight", key: "one:a" });
  assert.equal(activeModelChoice([], "one:b", highlight), undefined);
});

test("an external selected model takes priority over a highlight from the previous selection", () => {
  const before = structuredClone(choices);
  const highlight = { choiceKey: "one:a", selectedKey: "one:b" };
  assert.equal(activeModelChoice(choices, "two:c", highlight)?.key, "two:c");
  assert.equal(activeModelChoice(choices, null, highlight)?.key, "one:a");
  assert.equal(activeModelChoice(choices, "missing:model", highlight)?.key, "one:a", "a fallback highlight does not select or relabel a missing model");
  assert.deepEqual(choices, before);
});

test("opening keys open without selecting, while Home and End target their real endpoints", () => {
  for (const key of ["Enter", " ", "ArrowDown", "ArrowUp"]) {
    assert.deepEqual(modelPickerKeyIntent(choices, "one:b", key, closed), { type: "open" });
  }
  assert.deepEqual(modelPickerKeyIntent(choices, "one:b", "Home", closed), { type: "highlight", key: "one:a" });
  assert.deepEqual(modelPickerKeyIntent(choices, "one:b", "End", closed), { type: "highlight", key: "two:c" });
  assert.deepEqual(modelPickerKeyIntent(choices, "two:c", "ArrowDown", open), { type: "highlight", key: "one:a" });
  assert.deepEqual(modelPickerKeyIntent(choices, "one:a", "ArrowUp", open), { type: "highlight", key: "two:c" });
});

test("selection requires an interactive list and only uses a key still in the current catalog", () => {
  for (const state of [open, embedded]) {
    for (const key of ["Enter", " "]) {
      assert.deepEqual(modelPickerKeyIntent(choices, "two:c", key, state), { type: "select", key: "two:c" });
    }
  }
  for (const state of [{ ...open, disabled: true }, { ...embedded, disabled: true }]) {
    for (const key of ["Enter", " ", "ArrowDown", "Home", "End"]) {
      assert.deepEqual(modelPickerKeyIntent(choices, "two:c", key, state), { type: "none" });
    }
  }
  for (const key of ["Enter", "ArrowDown", "Home", "End"]) {
    assert.deepEqual(modelPickerKeyIntent([], "removed", key, open), { type: "none" });
  }
});

test("Escape closes only the standalone popup and Tab retains native focus movement", () => {
  assert.deepEqual(modelPickerKeyIntent(choices, "one:b", "Escape", open), { type: "close", reason: "escape" });
  assert.deepEqual(modelPickerKeyIntent(choices, "one:b", "Tab", open), { type: "close", reason: "tab" });
  for (const state of [closed, embedded]) {
    assert.deepEqual(modelPickerKeyIntent(choices, "one:b", "Escape", state), { type: "none" });
    assert.deepEqual(modelPickerKeyIntent(choices, "one:b", "Tab", state), { type: "none" });
  }
});

test("reasoning labels reflect supported levels rather than a stale previous model value", () => {
  const render = (levels: readonly ("none" | "high")[], value: "max" | null) => renderToStaticMarkup(
    <ReasoningPicker choices={choices} selectedModelKey="one:b" selectedModelTitle="Beta" onSelectModel={() => {}} levels={levels} value={value} disabled={false} onChange={() => {}} />,
  );
  assert.match(render(["none", "high"], "max"), /aria-label="模型与推理强度：Beta，默认"/);
  assert.match(render([], "max"), /aria-label="模型与推理强度：Beta"/);
  assert.doesNotMatch(render([], "max"), /Max|agent-effort-value/);
});
