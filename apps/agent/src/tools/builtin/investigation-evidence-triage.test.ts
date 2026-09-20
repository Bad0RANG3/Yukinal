import assert from "node:assert/strict";
import test from "node:test";

import type { Evidence, ToolTarget } from "@yukinal/shared";

import { ToolFailure, type ToolContext } from "../tool.js";
import { investigationEvidenceTriageTool } from "./investigation-evidence-triage.js";

const target: ToolTarget = { host: "remote", serverId: "srv_triage", environment: "staging" };
const hash = "a".repeat(64);

function evidence(
  id: string,
  sourceTool: string,
  kind: Evidence["kind"],
  content: unknown,
  overrides: Partial<Evidence> = {},
): Evidence {
  return {
    id,
    taskId: "task_triage",
    runId: "run_triage",
    scope: target,
    kind,
    sourceTool,
    collectedAt: "2026-09-20T00:00:00.000Z",
    inputSummary: "{}",
    contentType: typeof content === "string" ? "text" : "json",
    content,
    contentHash: hash,
    truncated: false,
    redactionStatus: "clean",
    ...overrides,
  };
}

function context(taskId = "task_triage"): ToolContext {
  return {
    callId: "call_triage",
    traceId: "trace_triage",
    target,
    taskId,
    signal: new AbortController().signal,
    deadlineAt: Date.now() + 15_000,
    log: () => {},
  };
}

test("evidence triage returns bounded candidates without raw log lines", async () => {
  const rows = new Map<string, Evidence>([
    ["ev_snapshot", evidence("ev_snapshot", "server.info", "snapshot", {
      id: "snapshot_1",
      serverId: "srv_triage",
      collectedAt: "2026-09-20T00:00:00.000Z",
      health: "critical",
      cpu: { usagePercent: 96 },
      memory: { usagePercent: 91 },
      collectors: [{ collectorId: "docker", collectedAt: "2026-09-20T00:00:00.000Z", ok: false }],
      capabilities: { linux: true },
    })],
    ["ev_logs", evidence("ev_logs", "server.logs", "log", {
      source: "journalctl",
      lines: [
        { text: "private fixture log: connection refused", level: "error" },
        { text: "request timed out", level: "warning" },
      ],
    })],
    ["ev_services", evidence("ev_services", "server.services", "service", {
      source: "systemd",
      services: [{ name: "api.service", state: "failed", status: "failed/failed" }],
    })],
    ["ev_containers", evidence("ev_containers", "docker.ps", "container", {
      available: true,
      containers: [{ name: "api", image: "fixture", state: "exited", status: "Exited", restartCount: 3 }],
    })],
  ]);
  const requested: string[] = [];
  const tool = investigationEvidenceTriageTool({
    fetchEvidence: async ({ evidenceId }) => {
      requested.push(evidenceId);
      const row = rows.get(evidenceId);
      return row ? { status: "success", evidence: row } : { status: "not_found" };
    },
  });

  const result = await tool.execute(
    { evidenceIds: [...rows.keys()], focus: "API 延迟" },
    context(),
  );

  assert.equal(tool.risk, "read");
  assert.equal(result.notCausal, true);
  assert.equal(result.evaluatedEvidence, 4);
  assert.deepEqual(requested.sort(), [...rows.keys()].sort());
  assert.equal(result.hypotheses.some((item) => item.code === "resource_pressure_candidate"), true);
  assert.equal(result.hypotheses.some((item) => item.code === "service_availability_candidate"), true);
  assert.equal(result.hypotheses.some((item) => item.code === "error_burst_candidate"), true);
  assert.equal(result.hypotheses.some((item) => item.code === "service_error_alignment_candidate"), true);
  assert.equal(result.hypotheses.every((item) => item.evidenceIds.length >= 1), true);
  assert.equal(result.signals.some((item) => item.code === "log_connection_refused"), true);
  assert.equal(result.signals.some((item) => item.summary.includes("private fixture log")), false);
  assert.equal(JSON.stringify(result).includes("connection refused"), false);
});

test("evidence triage keeps partial retrieval warnings and freshness boundaries visible", async () => {
  const row = evidence("ev_stale", "server.logs", "log", { lines: [] }, { truncated: true, freshness: {
    status: "expired",
    policy: "default-v1",
    evaluatedAt: "2026-09-20T00:00:00.000Z",
    ageSeconds: 90_000,
    staleAfterSeconds: 900,
    expiresAfterSeconds: 86_400,
  } });
  const tool = investigationEvidenceTriageTool({
    fetchEvidence: async ({ evidenceId }) => evidenceId === row.id
      ? { status: "success", evidence: row }
      : { status: "not_found" },
  });

  const result = await tool.execute({ evidenceIds: [row.id, "ev_missing"] }, context());

  assert.equal(result.signals.some((item) => item.code === "evidence_truncated"), true);
  assert.equal(result.warnings.some((item) => item.includes("expired")), true);
  assert.equal(result.warnings.some((item) => item.includes("ev_missing")), true);
  assert.equal(result.hypotheses.some((item) => item.code === "incomplete_observation_candidate"), true);
});

test("evidence triage requires a durable task and at least one retrievable item", async () => {
  const tool = investigationEvidenceTriageTool({
    fetchEvidence: async () => ({ status: "not_found" }),
  });

  await assert.rejects(
    tool.execute({ evidenceIds: ["ev_missing"] }, context("")),
    (error: unknown) => error instanceof ToolFailure && error.code === "invalid_input",
  );
  await assert.rejects(
    tool.execute({ evidenceIds: ["ev_missing"] }, context()),
    (error: unknown) => error instanceof ToolFailure && error.code === "not_found",
  );
});
