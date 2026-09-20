import assert from "node:assert/strict";
import test from "node:test";

import {
  DecisionBriefSchema,
  EvidenceSchema,
  EvidenceSearchInputSchema,
  EvidenceSearchResultSchema,
  EvidenceComparisonInputSchema,
  EvidenceComparisonResultSchema,
  EvidenceCorrelationInputSchema,
  EvidenceCorrelationResultSchema,
  EvidenceSummarySchema,
  InvestigationArtifactSchema,
  InvestigationBriefResponseSchema,
  InvestigationContextSchema,
  InvestigationFailureSchema,
  InvestigationObservationWindowSchema,
  InvestigationPlanSchema,
  InvestigationTaskSchema,
  InvestigationTaskStopInputSchema,
} from "./investigation.js";

const scope = { host: "remote" as const, serverId: "srv_demo", environment: "staging" as const };

test("accepts a bounded task and evidence envelope", () => {
    const task = InvestigationTaskSchema.parse({
        id: "task_1",
        serverId: "srv_demo",
        objective: "Find the cause of elevated API latency",
        successCriteria: ["Produce one evidence-backed cause", "State a verification command"],
        scope,
        mode: "readonly",
        permissionMode: "ask",
        automationLevel: "readonly",
        createdBy: "user",
        phase: "investigating",
        status: "pending",
        budget: { maxSteps: 25, maxRunMs: 600_000, maxAttempts: 3 },
        createdAt: "2026-09-19T00:00:00.000Z",
        updatedAt: "2026-09-19T00:00:00.000Z",
      });
    assert.equal(task.id, "task_1");
    assert.deepEqual(task.scope, scope);
    assert.deepEqual(task.guardrails, { forbiddenTools: [], forbiddenPathPrefixes: [] });

    const evidence = EvidenceSchema.parse({
        id: "ev_1",
        taskId: "task_1",
        scope,
        kind: "log",
        sourceTool: "server.logs",
        collectedAt: "2026-09-19T00:00:01.000Z",
        inputSummary: "last 100 lines of api service logs",
        contentType: "text",
        content: "request latency p95=1200ms",
        contentHash: "a".repeat(64),
        truncated: false,
        redactionStatus: "clean",
    });
    assert.equal(evidence.taskId, "task_1");
    assert.equal(evidence.contentHash, "a".repeat(64));
  });

test("accepts explicit task guardrails and keeps them bounded", () => {
  const result = InvestigationTaskSchema.safeParse({
    id: "task_guardrails",
    objective: "inspect staging",
    successCriteria: ["collect evidence"],
    scope,
    guardrails: {
      notBeforeAt: "2026-09-20T01:00:00Z",
      expiresAt: "2026-09-20T02:00:00Z",
      forbiddenTools: ["filesystem.write", "docker.restart"],
      forbiddenPathPrefixes: ["/srv/app/secrets", "/etc/private"],
    },
    mode: "readonly",
    permissionMode: "ask",
    automationLevel: "readonly",
    createdBy: "user",
    phase: "investigating",
    status: "pending",
    budget: { maxSteps: 25, maxRunMs: 600_000, maxAttempts: 3 },
    createdAt: "2026-09-19T00:00:00.000Z",
    updatedAt: "2026-09-19T00:00:00.000Z",
  });
  assert.equal(result.success, true);
  assert.equal(InvestigationTaskSchema.safeParse({
    id: "task_guardrails",
    objective: "inspect staging",
    successCriteria: ["collect evidence"],
    scope,
    guardrails: { forbiddenTools: ["x".repeat(257)], forbiddenPathPrefixes: [] },
    mode: "readonly",
    permissionMode: "ask",
    automationLevel: "readonly",
    createdBy: "user",
    phase: "investigating",
    status: "pending",
    budget: { maxSteps: 25, maxRunMs: 600_000, maxAttempts: 3 },
    createdAt: "2026-09-19T00:00:00.000Z",
    updatedAt: "2026-09-19T00:00:00.000Z",
  }).success, false);
});

test("rejects a task that tries to use an invalid state or target", () => {
    assert.throws(() =>
      InvestigationTaskSchema.parse({
        id: "task_1",
        objective: "inspect",
        successCriteria: ["x"],
        scope: { host: "remote", environment: "staging" },
        mode: "readonly",
        permissionMode: "ask",
        automationLevel: "readonly",
        createdBy: "user",
        phase: "investigating",
        status: "running",
        budget: { maxSteps: 25, maxRunMs: 600_000, maxAttempts: 3 },
        createdAt: "2026-09-19T00:00:00.000Z",
        updatedAt: "2026-09-19T00:00:00.000Z",
      }),
    );
  });

test("requires evidence references in decision options to remain bounded ids", () => {
    assert.doesNotThrow(() =>
      DecisionBriefSchema.parse({
        id: "brief_1",
        taskId: "task_1",
        generatedAt: "2026-09-19T00:00:00.000Z",
        status: "presented",
        findingIds: ["finding_1"],
        options: [
          {
            id: "opt_1",
            title: "Inspect",
            summary: "Collect one more read-only sample",
            impact: "No remote state change",
            riskLevel: "read",
            evidenceIds: ["ev_1"],
            findingIds: ["finding_1"],
            verification: "Compare the next sample",
            requiresApproval: false,
            status: "available",
            continuation: "continue_readonly",
          },
        ],
      }),
    );
});

test("evidence search stays metadata-only and bounds its filters", () => {
  assert.equal(
    EvidenceSearchInputSchema.safeParse({
      sourceTool: "docker.logs",
      kind: "log",
      from: "2026-09-19T00:00:00.000Z",
      to: "2026-09-19T01:00:00.000Z",
      target: scope,
      limit: 8,
    }).success,
    true,
  );
  assert.equal(EvidenceSearchInputSchema.safeParse({ limit: 65 }).success, false);
  assert.equal(
    EvidenceSummarySchema.safeParse({
      id: "ev_1",
      taskId: "task_1",
      scope,
      kind: "log",
      sourceTool: "docker.logs",
      collectedAt: "2026-09-19T00:00:00.000Z",
      inputSummary: "tail=100",
      contentType: "text",
      contentHash: "a".repeat(64),
      truncated: false,
      redactionStatus: "clean",
    }).success,
    true,
  );
  assert.equal(
    EvidenceSummarySchema.safeParse({
      id: "ev_1",
      taskId: "task_1",
      scope,
      kind: "log",
      sourceTool: "docker.logs",
      collectedAt: "2026-09-19T00:00:00.000Z",
      inputSummary: "tail=100",
      contentType: "text",
      content: "raw body must not cross the search contract",
      contentHash: "a".repeat(64),
      truncated: false,
      redactionStatus: "clean",
    }).success,
    false,
  );
  assert.doesNotThrow(() => EvidenceSearchResultSchema.parse({ evidence: [] }));
});

test("evidence comparison stays bounded and does not admit raw bodies", () => {
  assert.equal(
    EvidenceComparisonInputSchema.safeParse({ leftEvidenceId: "ev_1", rightEvidenceId: "ev_2" }).success,
    true,
  );
  assert.equal(EvidenceComparisonInputSchema.safeParse({ leftEvidenceId: "ev_1" }).success, false);
  assert.doesNotThrow(() => EvidenceComparisonResultSchema.parse({
    comparison: {
      status: "changed",
      shape: "json",
      left: {
        id: "ev_1", taskId: "task_1", scope, kind: "snapshot", sourceTool: "server.info",
        collectedAt: "2026-09-19T00:00:00.000Z", inputSummary: "{}", contentType: "json",
        contentHash: "a".repeat(64), truncated: false, redactionStatus: "clean",
      },
      right: {
        id: "ev_2", taskId: "task_1", scope, kind: "snapshot", sourceTool: "server.info",
        collectedAt: "2026-09-19T00:05:00.000Z", inputSummary: "{}", contentType: "json",
        contentHash: "b".repeat(64), truncated: false, redactionStatus: "clean",
      },
      changedPaths: ["$.status"], changedPathCount: 1, diffTruncated: false, warnings: [],
    },
  }));
});

test("evidence correlation stays metadata-only and bounds the window", () => {
  assert.equal(EvidenceCorrelationInputSchema.safeParse({ anchorEvidenceId: "ev_1", windowSeconds: 300, limit: 8 }).success, true);
  assert.equal(EvidenceCorrelationInputSchema.safeParse({ anchorEvidenceId: "ev_1", windowSeconds: 3_601 }).success, false);
  assert.doesNotThrow(() => EvidenceCorrelationResultSchema.parse({
    correlation: {
      anchor: {
        id: "ev_1", taskId: "task_1", runId: "run_1", scope, kind: "snapshot", sourceTool: "server.info",
        collectedAt: "2026-09-19T00:00:00.000Z", inputSummary: "health", contentType: "json",
        contentHash: "a".repeat(64), truncated: false, redactionStatus: "clean",
      },
      evidence: [],
      matchedBy: "same_run",
      windowSeconds: 300,
      sourceTools: ["server.info"],
      warnings: [],
    },
  }));
});

test("freshness metadata is bounded and remains host-derived", () => {
  const freshness = {
    status: "expired",
    policy: "default-v1",
    evaluatedAt: "2026-09-20T00:00:00Z",
    ageSeconds: 86_401,
    staleAfterSeconds: 900,
    expiresAfterSeconds: 86_400,
  };
  assert.equal(
    EvidenceSummarySchema.safeParse({
      id: "ev_1",
      taskId: "task_1",
      scope,
      kind: "snapshot",
      sourceTool: "server.info",
      collectedAt: "2026-09-18T00:00:00Z",
      inputSummary: "{}",
      contentType: "json",
      contentHash: "a".repeat(64),
      truncated: false,
      redactionStatus: "clean",
      freshness,
    }).success,
    true,
  );
  assert.equal(
    EvidenceSummarySchema.safeParse({
      id: "ev_1",
      taskId: "task_1",
      scope,
      kind: "snapshot",
      sourceTool: "server.info",
      collectedAt: "2026-09-18T00:00:00Z",
      inputSummary: "{}",
      contentType: "json",
      contentHash: "a".repeat(64),
      truncated: false,
      redactionStatus: "clean",
      freshness: { ...freshness, status: "not-a-status" },
    }).success,
    false,
  );
});

test("brief selection makes continuation explicit", () => {
  const brief = {
    id: "brief_1",
    taskId: "task_1",
    generatedAt: "2026-09-19T00:00:00.000Z",
    status: "selected" as const,
    findingIds: [],
    options: [],
    selectedOptionId: "option_1",
  };
  assert.equal(
    InvestigationBriefResponseSchema.safeParse({ brief, continuation: "continue_readonly" }).success,
    true,
  );
  assert.equal(InvestigationBriefResponseSchema.safeParse({ brief }).success, false);
});

test("task stop input is a strict, bounded task reference", () => {
  assert.equal(InvestigationTaskStopInputSchema.safeParse({ taskId: "task_1" }).success, true);
  assert.equal(InvestigationTaskStopInputSchema.safeParse({ taskId: "task_1", reason: "done" }).success, false);
  assert.equal(InvestigationTaskStopInputSchema.safeParse({ taskId: "" }).success, false);
});

test("Agent investigation context excludes evidence and artifact bodies", () => {
  const context = {
    task: {
      id: "task_1",
      createdBy: "user",
      phase: "investigating",
      objective: "inspect",
      successCriteria: ["collect evidence"],
      scope,
      mode: "readonly",
      permissionMode: "ask",
      automationLevel: "readonly",
      status: "pending",
      budget: { maxSteps: 25, maxRunMs: 600_000, maxAttempts: 3 },
      createdAt: "2026-09-19T00:00:00.000Z",
      updatedAt: "2026-09-19T00:00:00.000Z",
    },
    evidence: [{
      id: "ev_1",
      taskId: "task_1",
      scope,
      kind: "log",
      sourceTool: "server.logs",
      collectedAt: "2026-09-19T00:00:01.000Z",
      inputSummary: "tail=100",
      contentType: "text",
      contentHash: "a".repeat(64),
      truncated: false,
      redactionStatus: "clean",
    }],
    findings: [],
    artifacts: [],
    runs: [],
    steps: [],
  };
  assert.doesNotThrow(() => InvestigationContextSchema.parse(context));
  assert.equal(
    InvestigationContextSchema.safeParse({
      ...context,
      evidence: [{ ...context.evidence[0], content: "body must be fetched by id" }],
    }).success,
    false,
  );
  assert.equal(
    InvestigationContextSchema.safeParse({
      ...context,
      artifacts: [{
        id: "artifact_1",
        taskId: "task_1",
        phase: "investigating",
        kind: "evidence_set",
        status: "ready",
        title: "sample",
        summary: "metadata only",
        content: { raw: "body must not cross context" },
        evidenceIds: ["ev_1"],
        createdAt: "2026-09-19T00:00:00.000Z",
        updatedAt: "2026-09-19T00:00:00.000Z",
      }],
    }).success,
    false,
  );
});

test("accepts a bounded active investigation plan", () => {
  const plan = InvestigationPlanSchema.parse({
    id: "plan_1",
    taskId: "task_1",
    revision: 1,
    status: "active",
    createdAt: "2026-09-19T00:00:00.000Z",
    updatedAt: "2026-09-19T00:00:00.000Z",
    currentStepId: "step_1",
    steps: [
      {
        id: "step_1",
        ordinal: 0,
        kind: "evidence",
        title: "Read service state",
        purpose: "Establish a baseline",
        allowedTools: ["server.info"],
        evidenceIds: [],
        successCriteria: ["A service state is recorded"],
        requiresApproval: false,
        maxAttempts: 2,
        attempts: 0,
        status: "running",
      },
    ],
  });
  assert.equal(plan.steps[0]?.allowedTools[0], "server.info");
});

test("accepts exact scalar input bindings and caps their cardinality", () => {
  const baseStep = {
    id: "step_1",
    ordinal: 0,
    kind: "evidence" as const,
    title: "Read configuration",
    purpose: "Establish a bounded file baseline",
    allowedTools: ["filesystem.read"],
    evidenceIds: [],
    successCriteria: ["A file revision is recorded"],
    requiresApproval: false,
    maxAttempts: 1,
    attempts: 0,
    status: "running" as const,
  };
  const parsed = InvestigationPlanSchema.parse({
    id: "plan_bindings",
    taskId: "task_1",
    revision: 1,
    status: "active",
    createdAt: "2026-09-19T00:00:00.000Z",
    updatedAt: "2026-09-19T00:00:00.000Z",
    currentStepId: "step_1",
    steps: [{ ...baseStep, inputBindings: { path: "/etc/app.env" } }],
  });
  assert.deepEqual(parsed.steps[0]?.inputBindings, { path: "/etc/app.env" });
  assert.throws(() =>
    InvestigationPlanSchema.parse({
      id: "plan_too_many_bindings",
      taskId: "task_1",
      revision: 1,
      status: "active",
      createdAt: "2026-09-19T00:00:00.000Z",
      updatedAt: "2026-09-19T00:00:00.000Z",
      currentStepId: "step_1",
      steps: [{
        ...baseStep,
        inputBindings: Object.fromEntries(Array.from({ length: 9 }, (_value, index) => [`field${index}`, "value"])),
      }],
    }),
  );
});

test("keeps failure choices and phase artifacts bounded", () => {
  const failure = InvestigationFailureSchema.parse({
    code: "transport",
    message: "sidecar disconnected",
    retryable: true,
    attempt: 1,
    at: "2026-09-20T00:00:00.000Z",
    options: [{
      id: "retry",
      action: "retry",
      title: "重试当前阶段",
      description: "重新校验目标后再尝试一次",
      requiresApproval: false,
    }],
  });
  assert.equal(failure.options?.[0]?.action, "retry");
  const artifact = InvestigationArtifactSchema.parse({
    id: "artifact_1",
    taskId: "task_1",
    phase: "verification",
    kind: "verification",
    status: "succeeded",
    title: "验证结果",
    summary: "服务健康检查通过",
    content: { checks: [{ name: "health", passed: true }] },
    evidenceIds: ["ev_1"],
    createdAt: "2026-09-20T00:00:00.000Z",
    updatedAt: "2026-09-20T00:00:01.000Z",
  });
  assert.equal(artifact.kind, "verification");
});

test("does not accept an observation interval beyond its window", () => {
  assert.throws(() =>
    InvestigationObservationWindowSchema.parse({
      durationSeconds: 60,
      intervalSeconds: 61,
      allowedTools: ["server.info"],
      successCriteria: ["health remains good"],
      status: "pending",
      sampleCount: 0,
    }),
  );
});
