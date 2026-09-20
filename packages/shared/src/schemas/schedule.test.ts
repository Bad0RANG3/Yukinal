import assert from "node:assert/strict";
import test from "node:test";

import {
  InvestigationScheduleSchema,
  InvestigationScheduleUpdateInputSchema,
} from "./schedule.js";

const schedule = {
  id: "schedule_01",
  taskId: "task_01",
  status: "active" as const,
  intervalSeconds: 300,
  cooldownSeconds: 120,
  dedupeWindowSeconds: 300,
  maxConcurrentRuns: 1,
  budget: { maxSteps: 25, maxRunMs: 900_000, maxAttempts: 3 },
  notificationPolicy: "on_change" as const,
  nextRunAt: "2026-09-20T00:05:00Z",
  lastRunAt: "2026-09-20T00:00:00Z",
  lastOutcome: "no_change",
  createdAt: "2026-09-20T00:00:00Z",
  updatedAt: "2026-09-20T00:01:00Z",
};

test("schedule responses and updates carry an optional explicit baseline", () => {
  const parsed = InvestigationScheduleSchema.parse({
    ...schedule,
    baselineRunId: "schedrun_01",
  });
  assert.equal(parsed.baselineRunId, "schedrun_01");

  const update = InvestigationScheduleUpdateInputSchema.parse({
    scheduleId: schedule.id,
    baselineRunId: "schedrun_01",
  });
  assert.equal(update.baselineRunId, "schedrun_01");
  const automatic = InvestigationScheduleUpdateInputSchema.parse({
    scheduleId: schedule.id,
    baselineRunId: null,
  });
  assert.equal(automatic.baselineRunId, null);
  assert.throws(() => InvestigationScheduleUpdateInputSchema.parse({
    scheduleId: schedule.id,
    baselineRunId: "",
  }));
});
