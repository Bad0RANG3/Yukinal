import assert from "node:assert/strict";
import test from "node:test";

import {
  shouldAutoRecoverDelegatedTask,
  shouldAutoStartRecoveredTask,
} from "../src/features/investigations/recovery.js";
import {
  canDelegateAgentAuto,
  effectiveTaskPermissionMode,
  shouldAutoStartCreatedTask,
} from "../src/features/investigations/auto-delegation.js";
import {
  notificationPolicyLabel,
  scheduleIntervalLabel,
  scheduleIntervalOptions,
  scheduleStatusLabel,
} from "../src/features/investigations/schedule-ui.js";

const staging = { host: "remote" as const, environment: "staging" as const };

test("retry-like recovery auto-starts only after the host clears the active run", () => {
  assert.equal(shouldAutoStartRecoveredTask({ status: "investigating", activeRunId: undefined }), true);
  assert.equal(shouldAutoStartRecoveredTask({ status: "investigating", activeRunId: "run_old" }), false);
  assert.equal(shouldAutoStartRecoveredTask({ status: "waiting_user", activeRunId: undefined }), false);
  assert.equal(shouldAutoStartRecoveredTask({ status: "stopped", activeRunId: undefined }), false);
});

test("only retryable delegated goals auto-recover within their attempt budget", () => {
  const base = {
    status: "failed" as const,
    activeRunId: undefined,
    scope: staging,
    mode: "goal" as const,
    permissionMode: "auto" as const,
    automationLevel: "execute" as const,
    budget: { maxAttempts: 3 },
    lastFailure: { retryable: true, attempt: 1, code: "transport" as const },
  };
  assert.equal(shouldAutoRecoverDelegatedTask(base), true);
  assert.equal(shouldAutoRecoverDelegatedTask({ ...base, activeRunId: "run_live" }), false);
  assert.equal(shouldAutoRecoverDelegatedTask({ ...base, permissionMode: "ask" }), false);
  assert.equal(shouldAutoRecoverDelegatedTask({ ...base, lastFailure: { ...base.lastFailure, retryable: false } }), false);
  assert.equal(shouldAutoRecoverDelegatedTask({ ...base, lastFailure: { ...base.lastFailure, attempt: 3 } }), false);
  assert.equal(shouldAutoRecoverDelegatedTask({
    ...base,
    scope: { host: "remote", environment: "production" },
  }), false);
});

test("task auto delegation is available only for executable remote development/staging goals", () => {
  assert.equal(canDelegateAgentAuto({ scope: staging, mode: "goal", automationLevel: "execute" }), true);
  assert.equal(canDelegateAgentAuto({ scope: staging, mode: "plan", automationLevel: "execute" }), false);
  assert.equal(canDelegateAgentAuto({ scope: staging, mode: "goal", automationLevel: "propose" }), false);
  assert.equal(canDelegateAgentAuto({ scope: { host: "local", environment: "local" }, mode: "goal", automationLevel: "execute" }), false);
  assert.equal(canDelegateAgentAuto({ scope: { host: "remote", environment: "production" }, mode: "goal", automationLevel: "execute" }), false);
  assert.equal(canDelegateAgentAuto({ scope: { host: "remote", environment: "unknown" }, mode: "goal", automationLevel: "execute" }), false);
});

test("a stale auto selection is normalized back to ask outside the delegation boundary", () => {
  assert.equal(effectiveTaskPermissionMode("auto", { scope: staging, mode: "goal", automationLevel: "execute" }), "auto");
  assert.equal(effectiveTaskPermissionMode("auto", { scope: staging, mode: "plan", automationLevel: "execute" }), "ask");
  assert.equal(effectiveTaskPermissionMode("auto", { scope: { host: "remote", environment: "production" }, mode: "goal", automationLevel: "execute" }), "ask");
  assert.equal(effectiveTaskPermissionMode("ask", { scope: staging, mode: "goal", automationLevel: "execute" }), "ask");
});

test("only readonly tasks or an explicit eligible auto goal start after creation", () => {
  assert.equal(shouldAutoStartCreatedTask({
    scope: { host: "local", environment: "local" },
    mode: "readonly",
    automationLevel: "readonly",
    permissionMode: "ask",
  }), true);
  assert.equal(shouldAutoStartCreatedTask({
    scope: staging,
    mode: "goal",
    automationLevel: "execute",
    permissionMode: "auto",
  }), true);
  assert.equal(shouldAutoStartCreatedTask({
    scope: staging,
    mode: "goal",
    automationLevel: "execute",
    permissionMode: "ask",
  }), false);
  assert.equal(shouldAutoStartCreatedTask({
    scope: staging,
    mode: "plan",
    automationLevel: "execute",
    permissionMode: "auto",
  }), false);
  assert.equal(shouldAutoStartCreatedTask({
    scope: { host: "remote", environment: "production" },
    mode: "goal",
    automationLevel: "execute",
    permissionMode: "auto",
  }), false);
});

test("schedule controls keep persisted intervals and expose localized policy labels", () => {
  assert.equal(scheduleIntervalLabel(300), "每 5 分钟");
  assert.equal(scheduleIntervalLabel(90), "每 90 秒");
  assert.equal(scheduleIntervalLabel(86_400), "每 1 天");
  assert.deepEqual(scheduleIntervalOptions(300).map((option) => option.seconds), [60, 300, 900, 1_800, 3_600, 21_600, 86_400]);
  assert.equal(scheduleIntervalOptions(42)[0]?.label, "每 42 秒（自定义）");
  assert.equal(notificationPolicyLabel("on_change"), "仅有变化");
  assert.equal(scheduleStatusLabel("revoked"), "已撤销");
});
