import assert from "node:assert/strict";
import test from "node:test";

import {
  HOST_METHODS,
  type DecisionBrief,
  type Finding,
  type HostContextRequest,
  type HostEvidenceRecordRequest,
  type InvestigationArtifact,
  type InvestigationPlan,
} from "@yukinal/shared";

import { HostRpcClient } from "./host-client.js";

const request = {
  callId: "call_1",
  traceId: "trace_1",
  toolName: "server.info",
  input: {},
  target: { host: "remote" as const, serverId: "srv_01abc", environment: "staging" as const },
};

test("host client sends a typed tool request and resolves its response", async () => {
  const frames: string[] = [];
  const client = new HostRpcClient((frame) => frames.push(frame));
  const pending = client.execute(request);
  const frame = JSON.parse(frames[0] ?? "{}") as { id: number; method: string; params: unknown };

  assert.equal(frame.method, HOST_METHODS.toolExecute);
  assert.deepEqual(frame.params, request);
  assert.equal(client.handleIncoming({ jsonrpc: "2.0", id: frame.id, result: { status: "success", output: { ok: true } } }), true);
  assert.deepEqual(await pending, { status: "success", output: { ok: true } });
});

test("host client sends a typed context request and resolves its response", async () => {
  const frames: string[] = [];
  const client = new HostRpcClient((frame) => frames.push(frame));
  const request: HostContextRequest = { kind: "server", id: "srv_01abc" };
  const pending = client.fetchContext(request);
  const frame = JSON.parse(frames[0] ?? "{}") as { id: number; method: string; params: unknown };

  assert.equal(frame.method, HOST_METHODS.contextFetch);
  assert.deepEqual(frame.params, request);
  assert.equal(
    client.handleIncoming({ jsonrpc: "2.0", id: frame.id, result: { status: "success", data: { id: request.id } } }),
    true,
  );
  assert.deepEqual(await pending, { status: "success", data: { id: request.id } });
});

test("host client records a bounded evidence envelope through the investigation method", async () => {
  const frames: string[] = [];
  const client = new HostRpcClient((frame) => frames.push(frame));
  const request: HostEvidenceRecordRequest = {
    evidence: {
      id: "ev_1",
      taskId: "task_1",
      scope: { host: "remote", serverId: "srv_01abc", environment: "staging" },
      kind: "snapshot",
      sourceTool: "server.info",
      collectedAt: "2026-09-19T00:00:00.000Z",
      inputSummary: "{}",
      contentType: "json",
      content: { os: "linux" },
      contentHash: "a".repeat(64),
      truncated: false,
      redactionStatus: "clean",
    },
  };
  const pending = client.recordEvidence(request);
  const frame = JSON.parse(frames[0] ?? "{}") as { id: number; method: string; params: unknown };

  assert.equal(frame.method, HOST_METHODS.evidenceRecord);
  assert.deepEqual(frame.params, request);
  assert.equal(client.handleIncoming({ jsonrpc: "2.0", id: frame.id, result: { recorded: true } }), true);
  assert.deepEqual(await pending, { recorded: true });
});

test("host client retrieves evidence only through the typed fetch method", async () => {
  const frames: string[] = [];
  const client = new HostRpcClient((frame) => frames.push(frame));
  const pending = client.fetchEvidence({ taskId: "task_1", evidenceId: "ev_1" });
  const frame = JSON.parse(frames[0] ?? "{}") as { id: number; method: string; params: unknown };

  assert.equal(frame.method, HOST_METHODS.evidenceFetch);
  assert.deepEqual(frame.params, { taskId: "task_1", evidenceId: "ev_1" });
  const evidence = {
    id: "ev_1",
    taskId: "task_1",
    scope: { host: "remote", serverId: "srv_01abc", environment: "staging" },
    kind: "snapshot",
    sourceTool: "server.info",
    collectedAt: "2026-09-19T00:00:00.000Z",
    inputSummary: "{}",
    contentType: "json",
    content: { os: "linux" },
    contentHash: "a".repeat(64),
    truncated: false,
    redactionStatus: "clean",
  };
  assert.equal(client.handleIncoming({ jsonrpc: "2.0", id: frame.id, result: { status: "success", evidence } }), true);
  assert.deepEqual((await pending).status, "success");
});

test("host client searches evidence metadata without requesting a body", async () => {
  const frames: string[] = [];
  const client = new HostRpcClient((frame) => frames.push(frame));
  const pending = client.searchEvidence({
    taskId: "task_1",
    sourceTool: "docker.logs",
    kind: "log",
    from: "2026-09-19T00:00:00.000Z",
    to: "2026-09-19T01:00:00.000Z",
    limit: 8,
  });
  const frame = JSON.parse(frames[0] ?? "{}") as { id: number; method: string; params: unknown };

  assert.equal(frame.method, HOST_METHODS.evidenceSearch);
  assert.deepEqual(frame.params, {
    taskId: "task_1",
    sourceTool: "docker.logs",
    kind: "log",
    from: "2026-09-19T00:00:00.000Z",
    to: "2026-09-19T01:00:00.000Z",
    limit: 8,
  });
  const summary = {
    id: "ev_1",
    taskId: "task_1",
    scope: { host: "remote", serverId: "srv_01abc", environment: "staging" },
    kind: "log",
    sourceTool: "docker.logs",
    collectedAt: "2026-09-19T00:30:00.000Z",
    inputSummary: "tail=100",
    contentType: "text",
    contentHash: "a".repeat(64),
    truncated: false,
    redactionStatus: "clean",
  };
  assert.equal(client.handleIncoming({ jsonrpc: "2.0", id: frame.id, result: { status: "success", evidence: [summary] } }), true);
  assert.deepEqual(await pending, { status: "success", evidence: [summary] });
});

test("host client compares two evidence ids through the bounded read contract", async () => {
  const frames: string[] = [];
  const client = new HostRpcClient((frame) => frames.push(frame));
  const pending = client.compareEvidence({ taskId: "task_1", leftEvidenceId: "ev_1", rightEvidenceId: "ev_2" });
  const frame = JSON.parse(frames[0] ?? "{}") as { id: number; method: string; params: unknown };

  assert.equal(frame.method, HOST_METHODS.evidenceCompare);
  assert.deepEqual(frame.params, { taskId: "task_1", leftEvidenceId: "ev_1", rightEvidenceId: "ev_2" });
  const summary = (id: string, hash: string) => ({
    id,
    taskId: "task_1",
    scope: { host: "remote" as const, serverId: "srv_01abc", environment: "staging" as const },
    kind: "snapshot" as const,
    sourceTool: "server.info",
    collectedAt: "2026-09-19T00:00:00.000Z",
    inputSummary: "{}",
    contentType: "json" as const,
    contentHash: hash,
    truncated: false,
    redactionStatus: "clean" as const,
  });
  const result = {
    status: "success" as const,
    comparison: {
      status: "changed" as const,
      shape: "json" as const,
      left: summary("ev_1", "a".repeat(64)),
      right: summary("ev_2", "b".repeat(64)),
      changedPaths: ["$.status"],
      changedPathCount: 1,
      diffTruncated: false,
      warnings: [],
    },
  };
  assert.equal(client.handleIncoming({ jsonrpc: "2.0", id: frame.id, result }), true);
  assert.deepEqual(await pending, result);
});

test("host client correlates evidence through the host-owned metadata contract", async () => {
  const frames: string[] = [];
  const client = new HostRpcClient((frame) => frames.push(frame));
  const pending = client.correlateEvidence({ taskId: "task_1", anchorEvidenceId: "ev_1", limit: 8 });
  const frame = JSON.parse(frames[0] ?? "{}") as { id: number; method: string; params: unknown };

  assert.equal(frame.method, HOST_METHODS.evidenceCorrelate);
  assert.deepEqual(frame.params, { taskId: "task_1", anchorEvidenceId: "ev_1", limit: 8 });
  const summary = (id: string, sourceTool: string, kind: "snapshot" | "log") => ({
    id,
    taskId: "task_1",
    runId: "run_1",
    scope: { host: "remote" as const, serverId: "srv_01abc", environment: "staging" as const },
    kind,
    sourceTool,
    collectedAt: "2026-09-19T00:00:00.000Z",
    inputSummary: "bounded",
    contentType: "json" as const,
    contentHash: "a".repeat(64),
    truncated: false,
    redactionStatus: "clean" as const,
  });
  const result = {
    status: "success" as const,
    correlation: {
      anchor: summary("ev_1", "server.info", "snapshot"),
      evidence: [summary("ev_2", "server.logs", "log")],
      matchedBy: "same_run" as const,
      windowSeconds: 300,
      sourceTools: ["server.info", "server.logs"],
      warnings: [],
    },
  };
  assert.equal(client.handleIncoming({ jsonrpc: "2.0", id: frame.id, result }), true);
  assert.deepEqual(await pending, result);
});

test("host client previews retention without exposing a prune method", async () => {
  const frames: string[] = [];
  const client = new HostRpcClient((frame) => frames.push(frame));
  const pending = client.previewRetention({ taskId: "task_1", limit: 8 });
  const frame = JSON.parse(frames[0] ?? "{}") as { id: number; method: string; params: unknown };

  assert.equal(frame.method, HOST_METHODS.retentionPreview);
  assert.deepEqual(frame.params, { taskId: "task_1", limit: 8 });
  const result = {
    status: "success" as const,
    preview: {
      taskId: "task_1",
      cutoffAt: "2026-02-01T00:00:00Z",
      candidates: [],
      protectedCount: 0,
      candidateBytes: 0,
      truncated: false,
    },
  };
  assert.equal(client.handleIncoming({ jsonrpc: "2.0", id: frame.id, result }), true);
  assert.deepEqual(await pending, result);
});

test("host client records and checks a durable investigation plan", async () => {
  const frames: string[] = [];
  const client = new HostRpcClient((frame) => frames.push(frame));
  const plan: InvestigationPlan = {
    id: "plan_1",
    taskId: "task_1",
    revision: 1,
    status: "active",
    createdAt: "2026-09-19T00:00:00.000Z",
    updatedAt: "2026-09-19T00:00:00.000Z",
    currentStepId: "plan_step_1",
    steps: [
      {
        id: "plan_step_1",
        ordinal: 0,
        kind: "evidence",
        title: "Read service state",
        purpose: "Establish a baseline",
        allowedTools: ["server.info"],
        evidenceIds: [],
        successCriteria: ["A service state is recorded"],
        requiresApproval: false,
        maxAttempts: 1,
        attempts: 0,
        status: "running",
      },
    ],
  };
  const recorded = client.recordPlan({ plan });
  const recordFrame = JSON.parse(frames[0] ?? "{}") as { id: number; method: string; params: unknown };
  assert.equal(recordFrame.method, HOST_METHODS.planRecord);
  assert.deepEqual(recordFrame.params, { plan });
  assert.equal(client.handleIncoming({ jsonrpc: "2.0", id: recordFrame.id, result: { recorded: true, plan } }), true);
  assert.deepEqual(await recorded, { recorded: true, plan });

  const checked = client.checkPlan({
    taskId: "task_1",
    toolName: "server.info",
    input: {},
    target: { host: "remote", serverId: "srv_01abc", environment: "staging" },
  });
  const checkFrame = JSON.parse(frames[1] ?? "{}") as { id: number; method: string; params: unknown };
  assert.equal(checkFrame.method, HOST_METHODS.planCheck);
  assert.deepEqual(checkFrame.params, {
    taskId: "task_1",
    toolName: "server.info",
    input: {},
    target: { host: "remote", serverId: "srv_01abc", environment: "staging" },
  });
  const response = {
    status: "allowed",
    planId: plan.id,
    stepId: "plan_step_1",
    stepKind: "evidence",
    evidenceIds: [],
    requiresApproval: false,
  } as const;
  assert.equal(client.handleIncoming({ jsonrpc: "2.0", id: checkFrame.id, result: response }), true);
  assert.deepEqual(await checked, response);

  const stepResult = client.recordPlanStepResult({
    taskId: "task_1",
    planId: plan.id,
    stepId: "plan_step_1",
    status: "success",
    retryable: false,
    outputSummary: "baseline recorded",
  });
  const resultFrame = JSON.parse(frames[2] ?? "{}") as { id: number; method: string; params: unknown };
  assert.equal(resultFrame.method, HOST_METHODS.planStepResult);
  assert.deepEqual(resultFrame.params, {
    taskId: "task_1",
    planId: plan.id,
    stepId: "plan_step_1",
    status: "success",
    retryable: false,
    outputSummary: "baseline recorded",
  });
  assert.equal(client.handleIncoming({ jsonrpc: "2.0", id: resultFrame.id, result: { recorded: true, plan } }), true);
  assert.deepEqual(await stepResult, { recorded: true, plan });
});

test("host client records evidence-linked findings and decision briefs", async () => {
  const frames: string[] = [];
  const client = new HostRpcClient((frame) => frames.push(frame));
  const finding: Finding = {
    id: "finding_1",
    taskId: "task_1",
    title: "Latency is elevated",
    kind: "fact",
    statement: "The API p95 is 1200 ms.",
    evidenceIds: ["ev_1"],
    confidence: "high",
    createdAt: "2026-09-19T00:00:00.000Z",
  };
  const findingPending = client.recordFinding({ finding });
  const findingFrame = JSON.parse(frames[0] ?? "{}") as { id: number; method: string; params: unknown };
  assert.equal(findingFrame.method, HOST_METHODS.findingRecord);
  assert.deepEqual(findingFrame.params, { finding });
  assert.equal(
    client.handleIncoming({ jsonrpc: "2.0", id: findingFrame.id, result: { recorded: true, finding } }),
    true,
  );
  assert.deepEqual(await findingPending, { recorded: true, finding });

  const brief: DecisionBrief = {
    id: "brief_1",
    taskId: "task_1",
    generatedAt: "2026-09-19T00:01:00.000Z",
    status: "presented",
    findingIds: [finding.id],
    options: [
      {
        id: "option_1",
        title: "Collect another sample",
        summary: "Stay read-only.",
        impact: "No remote state change.",
        riskLevel: "read",
        evidenceIds: ["ev_1"],
        findingIds: [finding.id],
        verification: "The next sample is comparable.",
        requiresApproval: false,
        status: "available",
      },
    ],
  };
  const briefPending = client.recordBrief({ brief });
  const briefFrame = JSON.parse(frames[1] ?? "{}") as { id: number; method: string; params: unknown };
  assert.equal(briefFrame.method, HOST_METHODS.briefRecord);
  assert.deepEqual(briefFrame.params, { brief });
  assert.equal(
    client.handleIncoming({ jsonrpc: "2.0", id: briefFrame.id, result: { recorded: true, brief } }),
    true,
  );
  assert.deepEqual(await briefPending, { recorded: true, brief });
});

test("host client records a phase artifact with the plan binding", async () => {
  const frames: string[] = [];
  const client = new HostRpcClient((frame) => frames.push(frame));
  const artifact: InvestigationArtifact = {
    id: "artifact_1",
    taskId: "task_1",
    phase: "verification",
    kind: "verification",
    status: "succeeded",
    title: "验证结果",
    summary: "fixture 健康检查通过",
    content: { passed: true },
    evidenceIds: ["ev_1"],
    createdAt: "2026-09-20T00:00:00.000Z",
    updatedAt: "2026-09-20T00:00:01.000Z",
  };
  const request = {
    artifact,
    planId: "plan_1",
    planStepId: "step_1",
    evidenceIds: ["ev_1"],
  };
  const pending = client.recordArtifact(request);
  const frame = JSON.parse(frames[0] ?? "{}") as { id: number; method: string; params: unknown };
  assert.equal(frame.method, HOST_METHODS.artifactRecord);
  assert.deepEqual(frame.params, request);
  const response = { recorded: true, artifact } as const;
  assert.equal(client.handleIncoming({ jsonrpc: "2.0", id: frame.id, result: response }), true);
  assert.deepEqual(await pending, response);
});

test("aborting a host request removes it and rejects promptly", async () => {
  const frames: string[] = [];
  const client = new HostRpcClient((frame) => frames.push(frame));
  const controller = new AbortController();
  const pending = client.execute(request, controller.signal);
  const requestFrame = JSON.parse(frames[0] ?? "{}") as { id: number; method: string; params: unknown };
  controller.abort();

  await assert.rejects(pending, /host request cancelled/);
  const cancelFrame = JSON.parse(frames[1] ?? "{}") as {
    id: number;
    method: string;
    params: unknown;
  };
  assert.equal(requestFrame.method, HOST_METHODS.toolExecute);
  assert.equal(cancelFrame.method, HOST_METHODS.toolCancel);
  assert.deepEqual(cancelFrame.params, { requestId: requestFrame.id });
  assert.equal(client.handleIncoming({ jsonrpc: "2.0", id: requestFrame.id, result: { status: "success" } }), true);
});

test("closing the host client rejects every in-flight request after disconnect", async () => {
  const frames: string[] = [];
  const client = new HostRpcClient((frame) => frames.push(frame));
  const controller = new AbortController();
  const toolPending = client.execute(request, controller.signal);
  const catalogPending = client.fetchMcpCatalog();
  const toolFrame = JSON.parse(frames[0] ?? "{}") as { id: number; method: string };
  const catalogFrame = JSON.parse(frames[1] ?? "{}") as { id: number; method: string };

  client.close();

  await Promise.all([
    assert.rejects(toolPending, /host connection closed/),
    assert.rejects(catalogPending, /host connection closed/),
  ]);
  assert.equal(toolFrame.method, HOST_METHODS.toolExecute);
  assert.equal(catalogFrame.method, HOST_METHODS.mcpCatalog);

  const cancelFrame = JSON.parse(frames[2] ?? "{}") as {
    id: number;
    method: string;
    params: unknown;
  };
  assert.equal(cancelFrame.method, HOST_METHODS.toolCancel);
  assert.deepEqual(cancelFrame.params, { requestId: toolFrame.id });

  // Closing detaches abort listeners and makes any late host response harmless.
  controller.abort();
  assert.equal(frames.length, 3);
  assert.equal(
    client.handleIncoming({ jsonrpc: "2.0", id: toolFrame.id, result: { status: "success" } }),
    true,
  );
});
