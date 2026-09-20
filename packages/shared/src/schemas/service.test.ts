import assert from "node:assert/strict";
import test from "node:test";

import {
  ServerServicesInputSchema,
  SystemdInspectInputSchema,
  SystemdInspectResultSchema,
  SystemdRestartInputSchema,
  SystemdRestartResultSchema,
} from "./service.js";

test("service discovery filters remain exact and bounded", () => {
  assert.equal(ServerServicesInputSchema.safeParse({ name: "nginx.service", state: "running" }).success, true);
  assert.equal(ServerServicesInputSchema.safeParse({ name: "api_1", state: "failed" }).success, true);
  assert.equal(ServerServicesInputSchema.safeParse({ name: "api;id" }).success, false);
  assert.equal(ServerServicesInputSchema.safeParse({ state: "unknown" }).success, true);
});

test("systemd service schemas keep unit references bounded", () => {
  assert.equal(SystemdInspectInputSchema.safeParse({ service: "nginx.service" }).success, true);
  assert.equal(SystemdInspectInputSchema.safeParse({ service: "nginx" }).success, false);
  assert.equal(SystemdInspectInputSchema.safeParse({ service: "nginx.service;id" }).success, false);
  assert.equal(SystemdInspectInputSchema.safeParse({ service: "-nginx.service" }).success, false);
  assert.equal(SystemdRestartInputSchema.safeParse({ service: "nginx.service", timeoutSeconds: 120 }).success, true);
  assert.equal(SystemdRestartInputSchema.safeParse({ service: "nginx.service", timeoutSeconds: 121 }).success, false);
});

test("systemd result schemas keep normalized output strict", () => {
  assert.equal(
    SystemdInspectResultSchema.safeParse({
      service: "nginx.service",
      loadState: "loaded",
      activeState: "active",
      subState: "running",
      description: "Nginx",
    }).success,
    true,
  );
  assert.equal(SystemdRestartResultSchema.safeParse({ service: "nginx.service", restarted: true }).success, true);
  assert.equal(SystemdRestartResultSchema.safeParse({ service: "nginx.service", restarted: true, extra: true }).success, false);
});
