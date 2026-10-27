/**
 * 偏好迁移不许偷换用户的选择。
 *
 * v2 的 migrate 把 `agentPermissionMode` 无条件改回默认值（auto）。后果是：
 * 任何升级到 v2 的安装，用户手动选的「每次询问」都会在没有提示的情况下
 * 变成「自动批准」—— 一个安全相关的默认值被静默放宽。这里把它钉住。
 */

import assert from "node:assert/strict";
import test from "node:test";

import { sanitizePreferences } from "../src/stores/preferences-store.js";
import { DEFAULT_UI_PREFERENCES } from "../src/stores/preferences-store.js";

test("a stored ask choice survives sanitisation unchanged", () => {
  const sanitized = sanitizePreferences({
    ...DEFAULT_UI_PREFERENCES,
    agentPermissionMode: "ask",
  });
  assert.equal(sanitized.agentPermissionMode, "ask");
});

test("a stored auto choice survives sanitisation unchanged", () => {
  const sanitized = sanitizePreferences({
    ...DEFAULT_UI_PREFERENCES,
    agentPermissionMode: "auto",
  });
  assert.equal(sanitized.agentPermissionMode, "auto");
});

test("only an unreadable approval mode falls back to the default", () => {
  for (const value of [undefined, null, "", "off", 1, {}, [], "ASK"]) {
    const sanitized = sanitizePreferences({ agentPermissionMode: value });
    assert.equal(
      sanitized.agentPermissionMode,
      DEFAULT_UI_PREFERENCES.agentPermissionMode,
      `value ${JSON.stringify(value)} should fall back`,
    );
  }
});

test("the default approval mode is stated explicitly, not inherited by accident", () => {
  // The product default is documented as "变更不再逐条打断你"; the desktop ships
  // that default. A future change to it must be a deliberate edit here too.
  assert.equal(DEFAULT_UI_PREFERENCES.agentPermissionMode, "auto");
});
