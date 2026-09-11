import assert from "node:assert/strict";
import { test } from "node:test";

import { shortModelLabel } from "../src/features/agent/ModelPicker.js";

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
