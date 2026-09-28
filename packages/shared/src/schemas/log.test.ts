import assert from "node:assert/strict";
import test from "node:test";

import { ServerLogsInputSchema } from "./log.js";

test("log discovery queries keep the time window and unit bounded", () => {
  assert.equal(ServerLogsInputSchema.safeParse({ sinceSeconds: 3_600, unit: "nginx.service" }).success, true);
  assert.equal(ServerLogsInputSchema.safeParse({ sinceSeconds: 604_800 }).success, true);
  assert.equal(ServerLogsInputSchema.safeParse({ sinceSeconds: 604_801 }).success, false);
  assert.equal(ServerLogsInputSchema.safeParse({ sinceSeconds: 0 }).success, false);
  assert.equal(ServerLogsInputSchema.safeParse({ unit: "nginx.service;id" }).success, false);
  assert.equal(ServerLogsInputSchema.safeParse({ unit: "nginx" }).success, false);
});
