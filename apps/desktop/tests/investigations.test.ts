import assert from "node:assert/strict";
import test from "node:test";

import { shouldAutoStartRecoveredTask } from "../src/features/investigations/recovery.js";
import { LOCAL_TASK_TARGET, resolveInvestigationTaskTarget } from "../src/features/investigations/task-target.js";
import { describeInvestigationFailure } from "../src/features/investigations/error-display.js";
import {
  canDelegateAgentAuto,
  effectiveTaskPermissionMode,
  resolveTaskDelegation,
  shouldAutoStartCreatedTask,
} from "../src/features/investigations/auto-delegation.js";
import {
  notificationPolicyLabel,
  scheduleIntervalLabel,
  scheduleIntervalOptions,
  scheduleStatusLabel,
} from "../src/features/investigations/schedule-ui.js";

const staging = { host: "remote" as const, environment: "staging" as const };
const development = { host: "remote" as const, environment: "development" as const };
const configuredServer = {
  id: "srv_staging",
  name: "Staging API",
  connection: { host: "staging.example.test", port: 2222, username: "ops" },
  capabilities: {},
  status: "connected" as const,
  metadata: { environment: "staging" as const },
  createdAt: "2026-09-30T00:00:00.000Z",
  updatedAt: "2026-09-30T00:00:00.000Z",
};

test("goal target requires an explicit configured server or explicit local choice", () => {
  assert.equal(resolveInvestigationTaskTarget("", [configuredServer]), undefined);
  assert.deepEqual(resolveInvestigationTaskTarget("srv_missing", [configuredServer]), undefined);
  assert.deepEqual(resolveInvestigationTaskTarget("srv_staging", [configuredServer]), {
    host: "remote",
    serverId: "srv_staging",
    environment: "staging",
  });
  assert.deepEqual(resolveInvestigationTaskTarget(LOCAL_TASK_TARGET, [configuredServer]), {
    host: "local",
    environment: "local",
  });
});

test("retry-like recovery auto-starts only after the host clears the active run", () => {
  assert.equal(shouldAutoStartRecoveredTask({ status: "investigating", activeRunId: undefined }), true);
  assert.equal(shouldAutoStartRecoveredTask({ status: "investigating", activeRunId: "run_old" }), false);
  assert.equal(shouldAutoStartRecoveredTask({ status: "waiting_user", activeRunId: undefined }), false);
  assert.equal(shouldAutoStartRecoveredTask({ status: "stopped", activeRunId: undefined }), false);
});

test("a failure is presented by its category, not its message text", () => {
  const transport = describeInvestigationFailure({ code: "transport" });
  assert.equal(transport.label, "连接中断");
  assert.ok(transport.nextStep.length > 0);

  // Two different codes in the same category present identically; the message is
  // never consulted for the next step.
  assert.deepEqual(
    describeInvestigationFailure({ code: "approval_rejected" }),
    describeInvestigationFailure({ code: "approval_required" }),
  );
  assert.notDeepEqual(
    describeInvestigationFailure({ code: "transport" }),
    describeInvestigationFailure({ code: "permission_denied" }),
  );
});

test("task auto delegation is available only for executable remote development/staging goals", () => {
  assert.equal(canDelegateAgentAuto({ scope: staging, mode: "goal", automationLevel: "execute" }), true);
  assert.equal(canDelegateAgentAuto({ scope: development, mode: "goal", automationLevel: "execute" }), true);
  assert.equal(canDelegateAgentAuto({ scope: staging, mode: "plan", automationLevel: "execute" }), false);
  assert.equal(canDelegateAgentAuto({ scope: staging, mode: "goal", automationLevel: "propose" }), false);
  assert.equal(canDelegateAgentAuto({ scope: { host: "local", environment: "local" }, mode: "goal", automationLevel: "execute" }), false);
  assert.equal(canDelegateAgentAuto({ scope: { host: "remote", environment: "production" }, mode: "goal", automationLevel: "execute" }), false);
  assert.equal(canDelegateAgentAuto({ scope: { host: "remote", environment: "unknown" }, mode: "goal", automationLevel: "execute" }), false);
});

test("one bounded opt-in enables both auto permission and the command grant only for eligible tasks", () => {
  const ask = resolveTaskDelegation({
    requested: false,
    scope: staging,
    mode: "goal",
    automationLevel: "execute",
  });
  assert.deepEqual(ask, { eligible: true, permissionMode: "ask", grantTaskCommands: false });

  const delegated = resolveTaskDelegation({
    requested: true,
    scope: staging,
    mode: "goal",
    automationLevel: "execute",
  });
  assert.deepEqual(delegated, { eligible: true, permissionMode: "auto", grantTaskCommands: true });
  assert.deepEqual(resolveTaskDelegation({
    requested: true,
    scope: development,
    mode: "goal",
    automationLevel: "execute",
  }), { eligible: true, permissionMode: "auto", grantTaskCommands: true });

  for (const input of [
    { scope: { host: "remote" as const, environment: "production" as const }, mode: "goal" as const, automationLevel: "execute" as const },
    { scope: { host: "remote" as const, environment: "unknown" as const }, mode: "goal" as const, automationLevel: "execute" as const },
    { scope: { host: "local" as const, environment: "local" as const }, mode: "goal" as const, automationLevel: "execute" as const },
    { scope: staging, mode: "plan" as const, automationLevel: "execute" as const },
    { scope: staging, mode: "goal" as const, automationLevel: "propose" as const },
  ]) {
    assert.deepEqual(resolveTaskDelegation({ requested: true, ...input }), {
      eligible: false,
      permissionMode: "ask",
      grantTaskCommands: false,
    });
  }
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
