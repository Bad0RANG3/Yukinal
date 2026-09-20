import assert from "node:assert/strict";
import test from "node:test";

import type { InvestigationContext, Server, ServerSnapshot, Workspace } from "@yukinal/shared";

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
    async investigation() {
      return undefined;
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
  assert.match(bundle.rendered, /Focused server: none.*answer general questions directly/);
});

test("an oversized context block is truncated, and says so", async () => {
  const engine = new ContextEngine(source(), { maxRenderedChars: 40 });
  const bundle = await engine.build({ ...baseRequest, focusServerId: server.id });
  assert.equal(bundle.truncated, true);
  assert.ok(bundle.rendered.length <= 40);
});

test("renders prior investigation findings without injecting raw evidence bodies", async () => {
  const investigation: InvestigationContext = {
    task: {
      id: "task_1",
      createdBy: "user",
      phase: "decision",
      objective: "确认 API 延迟原因",
      successCriteria: ["找到可复核证据"],
      scope: { host: "remote", serverId: server.id, environment: "production" },
      guardrails: { forbiddenTools: [], forbiddenPathPrefixes: [] },
      mode: "readonly",
      permissionMode: "ask",
      automationLevel: "readonly",
      status: "waiting_user",
      budget: { maxSteps: 25, maxRunMs: 900_000, maxAttempts: 3 },
      createdAt: "2026-01-01T00:00:00.000Z",
      updatedAt: "2026-01-01T00:01:00.000Z",
      lastFailure: {
        code: "command_failed",
        message: "验证失败",
        retryable: false,
        attempt: 1,
        at: "2026-01-01T00:01:00.000Z",
        detail: { selectedOption: "rollback", rollbackRequested: true, requiresFreshBaseline: true },
      },
    },
    evidence: [{
      id: "ev_1",
      taskId: "task_1",
      scope: { host: "remote", serverId: server.id, environment: "production" },
      kind: "log",
      sourceTool: "docker.logs",
      collectedAt: "2026-01-01T00:01:00.000Z",
      inputSummary: "tail=100",
      contentType: "json",
      contentHash: "a".repeat(64),
      truncated: true,
      redactionStatus: "redacted",
      freshness: {
        status: "expired",
        policy: "default-v1",
        evaluatedAt: "2026-01-02T00:00:00Z",
        ageSeconds: 86_401,
        staleAfterSeconds: 900,
        expiresAfterSeconds: 86_400,
      },
    }],
    findings: [{
      id: "finding_1",
      taskId: "task_1",
      title: "错误率升高",
      kind: "fact",
      statement: "日志样本出现连续超时",
      evidenceIds: ["ev_1"],
      confidence: "high",
      createdAt: "2026-01-01T00:01:01.000Z",
    }],
    artifacts: [],
    runs: [],
    steps: [],
  };
  const engine = new ContextEngine(source({ investigation: async () => investigation }));
  const bundle = await engine.build({ ...baseRequest, taskId: investigation.task.id });

  assert.match(bundle.rendered, /Investigation: 确认 API 延迟原因/);
  assert.match(bundle.rendered, /Finding \(high\): 错误率升高/);
  assert.match(bundle.rendered, /separately gated inverse plan/);
  assert.match(bundle.rendered, /previous approval, change baseline and running observation window are invalid/);
  assert.match(bundle.rendered, /freshness=expired/);
  assert.match(bundle.rendered, /stale\/expired evidence is historical context only/);
  assert.doesNotMatch(bundle.rendered, /redacted/);
});
