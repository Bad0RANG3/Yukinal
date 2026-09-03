import assert from "node:assert/strict";
import test from "node:test";

import type { Server, ServerSnapshot, Workspace } from "@yukinal/shared";

import { ContextEngine, type ContextSource } from "./context-engine.js";

const server: Server = {
  id: "srv_api01",
  name: "Production API",
  connection: { host: "api.internal", port: 22, username: "deploy" },
  capabilities: { linux: true, docker: true },
  status: "connected",
  metadata: { environment: "production", region: "Singapore", hostname: "api-01" },
  createdAt: "2026-01-01T00:00:00.000Z",
  updatedAt: "2026-01-01T00:00:00.000Z",
};

const snapshot: ServerSnapshot = {
  id: "snp_1",
  serverId: "srv_api01",
  collectedAt: "2026-01-02T00:00:00.000Z",
  health: "warning",
  os: { distribution: "Ubuntu", version: "24.04", hostname: "api-01", kernel: "6.8", arch: "x86_64" },
  cpu: { model: "x86", cores: 8, usagePercent: 32.4, loadAverage: [1, 1, 1] },
  memory: { totalBytes: 16, usedBytes: 8, availableBytes: 8, usagePercent: 48.2 },
  disks: [{ device: "/dev/sda1", mountPoint: "/", totalBytes: 100, usedBytes: 61, usagePercent: 61.3 }],
  docker: {
    available: true,
    containers: [
      { name: "api", image: "registry/api:1.2", state: "restarting", status: "Restarting (1) 2 seconds ago", restartCount: 7 },
      { name: "nginx", image: "nginx:1.27", state: "running", status: "Up 3 weeks", restartCount: 0 },
    ],
  },
  capabilities: server.capabilities,
};

const workspace: Workspace = {
  id: "wsp_shop",
  name: "E-commerce Production",
  serverIds: ["srv_api01"],
  repositories: [],
  providerIds: [],
  defaultEnvironment: "production",
};

function source(overrides: Partial<ContextSource> = {}): ContextSource {
  return {
    async server(id) {
      return id === server.id ? server : undefined;
    },
    async snapshot(id) {
      return id === server.id ? snapshot : undefined;
    },
    async workspace(id) {
      return id === workspace.id ? workspace : undefined;
    },
    ...overrides,
  };
}

const baseRequest = {
  runId: "run_1",
  sessionId: "ses_1",
  prompt: "why is the order api slow",
};

test("assembles only the layers the request can justify", async () => {
  const engine = new ContextEngine(source());
  const bundle = await engine.build({
    ...baseRequest,
    workspaceId: workspace.id,
    focusServerId: server.id,
  });

  assert.deepEqual(bundle.layers, ["global", "task", "workspace", "server"]);
  assert.equal(bundle.server?.server.id, "srv_api01");
  assert.equal(bundle.server?.health, "warning");
  assert.equal(bundle.server?.metrics.cpu, 32);
  assert.equal(bundle.server?.metrics.disk, 61);
  assert.equal(bundle.server?.runtime.docker, true);
  assert.equal(bundle.workspace?.name, "E-commerce Production");
});

test("renders server identity with its environment, never just a host", async () => {
  const engine = new ContextEngine(source());
  const bundle = await engine.build({ ...baseRequest, focusServerId: server.id });

  assert.match(bundle.rendered, /Production API \[srv_api01\] \(production\)/);
  assert.match(bundle.rendered, /api.*restarting/s);
  assert.equal(bundle.truncated, false);
});

test("no focused server is stated explicitly so the agent must resolve a target", async () => {
  const engine = new ContextEngine(source());
  const bundle = await engine.build(baseRequest);

  assert.deepEqual(bundle.layers, ["global", "task"]);
  assert.match(bundle.rendered, /Focused server: none/);
});

test("an oversized context block is truncated, and says so", async () => {
  const engine = new ContextEngine(source(), { maxRenderedChars: 40 });
  const bundle = await engine.build({ ...baseRequest, focusServerId: server.id });
  assert.equal(bundle.truncated, true);
  assert.ok(bundle.rendered.length <= 40);
});
