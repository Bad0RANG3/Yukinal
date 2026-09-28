import assert from "node:assert/strict";
import test from "node:test";

import type { HostPlanRecordRequest, InvestigationPlan, ToolTarget } from "@yukinal/shared";

import type { HostRpcClient } from "../../transport/host-client.js";
import { ToolFailure, type ToolContext } from "../tool.js";
import { investigationPlaybookTool } from "./investigation-playbook.js";

const target: ToolTarget = { host: "remote", serverId: "srv_playbook", environment: "staging" };

function context(
  taskId = "task_playbook",
  delegation: Pick<ToolContext, "permissionMode" | "mode"> = {},
): ToolContext {
  return {
    callId: "call_playbook",
    traceId: "trace_playbook",
    target,
    taskId,
    signal: new AbortController().signal,
    deadlineAt: Date.now() + 10_000,
    log: () => {},
    ...delegation,
  };
}

function hostThatRecords(seen: InvestigationPlan[]): HostRpcClient {
  return {
    recordPlan: async ({ plan }: HostPlanRecordRequest) => {
      seen.push(plan);
      return { recorded: true, plan };
    },
  } as unknown as HostRpcClient;
}

for (const template of ["readonly_health", "config_edit", "container_restart", "systemd_restart"] as const) {
  test(`investigation.playbook records the bounded ${template} template`, async () => {
    const seen: InvestigationPlan[] = [];
    const tool = investigationPlaybookTool(hostThatRecords(seen));
    const request = template === "config_edit"
      ? { template, path: "/etc/yukinal/app.env" }
      : template === "container_restart"
        ? { template, container: "api_1" }
        : template === "systemd_restart"
          ? { template, service: "nginx.service" }
        : { template };

    const plan = await tool.execute(request, context());

    assert.equal(plan.taskId, "task_playbook");
    assert.equal(plan.status, "active");
    assert.equal(plan.currentStepId, plan.steps[0]?.id);
    assert.equal(seen.length, 1);
    assert.deepEqual(plan, seen[0]);
    assert.deepEqual(plan.steps.map((step) => step.ordinal), plan.steps.map((_step, index) => index));
    assert.equal(plan.steps[0]?.status, "running");
    assert.ok(plan.steps.slice(1).every((step) => step.status === "pending"));

    const action = plan.steps.find((step) => step.kind === "action");
    if (template === "readonly_health") {
      assert.equal(action, undefined);
      assert.equal(plan.steps.at(-1)?.kind, "verification");
      assert.deepEqual(plan.steps.map((step) => step.allowedTools), [
        ["server.info"],
        ["server.logs"],
        ["server.services"],
        ["docker.ps"],
        ["investigation.finding"],
        ["investigation.brief"],
        ["server.info"],
      ]);
    } else {
      assert.ok(action);
      assert.equal(action.requiresBaseline, true);
      assert.equal(action.requiresApproval, true);
      assert.ok(action.preview);
      assert.ok(action.rollback);
      assert.ok(action.verificationCriteria?.length);
      assert.equal(action.maxAttempts, 1);
      assert.equal(action.riskLevel, template === "container_restart" || template === "systemd_restart" ? "high" : "medium");
      assert.equal(action.allowedTools.length, 1);
      assert.equal(plan.steps.at(-1)?.kind, "verification");
      if (template === "config_edit") {
        assert.deepEqual(plan.steps[1]?.inputBindings, { path: "/etc/yukinal/app.env" });
        assert.deepEqual(plan.steps.filter((step) => step.kind === "action").map((step) => step.allowedTools), [
          ["filesystem.backup"],
          ["filesystem.edit"],
        ]);
        assert.deepEqual(plan.steps.filter((step) => step.kind === "action").map((step) => step.inputBindings), [
          { path: "/etc/yukinal/app.env" },
          { path: "/etc/yukinal/app.env" },
        ]);
        assert.deepEqual(plan.steps.at(-1)?.inputBindings, { path: "/etc/yukinal/app.env" });
      } else if (template === "container_restart") {
        assert.deepEqual(plan.steps[0]?.inputBindings, { container: "api_1" });
        assert.deepEqual(plan.steps[1]?.inputBindings, { container: "api_1" });
        assert.deepEqual(action.inputBindings, { container: "api_1" });
        assert.deepEqual(plan.steps.at(-1)?.inputBindings, { container: "api_1" });
      } else {
        assert.deepEqual(plan.steps[0]?.inputBindings, undefined);
        assert.deepEqual(plan.steps[1]?.inputBindings, { service: "nginx.service" });
        assert.deepEqual(action.inputBindings, { service: "nginx.service" });
        assert.deepEqual(plan.steps.at(-1)?.inputBindings, { service: "nginx.service" });
      }
    }
  });
}

test("investigation.playbook compiles a bounded multi-action deployment sequence", async () => {
  const seen: InvestigationPlan[] = [];
  const tool = investigationPlaybookTool(hostThatRecords(seen));
  const plan = await tool.execute(
    {
      template: "deploy_sequence",
      steps: [
        { operation: "edit_file", path: "/etc/yukinal/app.env" },
        { operation: "restart_container", container: "api_1" },
      ],
    },
    context(),
  );

  assert.equal(seen.length, 1);
  assert.equal(plan.steps.length, 11);
  assert.deepEqual(
    plan.steps.map((step) => step.allowedTools),
    [
      ["server.info"],
      ["filesystem.read"],
      ["docker.inspect"],
      ["docker.logs"],
      ["investigation.finding"],
      ["investigation.brief"],
      ["filesystem.backup"],
      ["filesystem.edit"],
      ["filesystem.read"],
      ["docker.restart"],
      ["docker.inspect"],
    ],
  );
  const actions = plan.steps.filter((step) => step.kind === "action");
  assert.equal(actions.length, 3);
  assert.deepEqual(actions.map((step) => step.inputBindings), [
    { path: "/etc/yukinal/app.env" },
    { path: "/etc/yukinal/app.env" },
    { container: "api_1" },
  ]);
  assert.deepEqual(actions.map((step) => step.allowedTools), [["filesystem.backup"], ["filesystem.edit"], ["docker.restart"]]);
  assert.deepEqual(actions.map((step) => step.riskLevel), ["medium", "medium", "high"]);
  assert.ok(actions.every((step) => step.requiresBaseline && step.requiresApproval && step.preview && step.rollback));
  assert.equal(plan.steps.at(-1)?.kind, "verification");
});

test("investigation.playbook delegates only medium configuration actions in an auto goal", async () => {
  const seen: InvestigationPlan[] = [];
  const tool = investigationPlaybookTool(hostThatRecords(seen));
  const plan = await tool.execute(
    { template: "config_edit", path: "/etc/yukinal/app.env" },
    context("task_auto", { permissionMode: "auto", mode: "goal" }),
  );

  assert.deepEqual(
    plan.steps.filter((step) => step.kind === "action").map((step) => step.requiresApproval),
    [false, false],
  );

  const planMode = await tool.execute(
    { template: "config_edit", path: "/etc/yukinal/app.env" },
    context("task_plan", { permissionMode: "auto", mode: "plan" }),
  );
  assert.equal(planMode.steps.find((step) => step.kind === "action")?.requiresApproval, true);
});

test("investigation.playbook compiles a package operation inside a deployment sequence", async () => {
  const seen: InvestigationPlan[] = [];
  const plan = await investigationPlaybookTool(hostThatRecords(seen)).execute(
    {
      template: "deploy_sequence",
      steps: [{ operation: "install_package", manager: "dnf", package: "nginx", version: "1.27.0-1" }],
    },
    context(),
  );

  assert.deepEqual(plan.steps.map((step) => step.allowedTools), [
    ["server.info"],
    ["package.inspect"],
    ["investigation.finding"],
    ["investigation.brief"],
    ["package.install"],
    ["package.inspect"],
  ]);
  assert.deepEqual(plan.steps.find((step) => step.kind === "action")?.inputBindings, {
    manager: "dnf",
    package: "nginx",
    version: "1.27.0-1",
  });
});

test("investigation.playbook adds a separately approved systemd restart with state verification", async () => {
  const seen: InvestigationPlan[] = [];
  const tool = investigationPlaybookTool(hostThatRecords(seen));
  const plan = await tool.execute(
    {
      template: "deploy_sequence",
      steps: [{ operation: "restart_service", service: "nginx.service" }],
    },
    context(),
  );

  assert.deepEqual(plan.steps.map((step) => step.allowedTools), [
    ["server.info"],
    ["systemd.inspect"],
    ["investigation.finding"],
    ["investigation.brief"],
    ["systemd.restart"],
    ["systemd.inspect"],
  ]);
  const action = plan.steps.find((step) => step.kind === "action");
  assert(action);
  assert.equal(action.riskLevel, "high");
  assert.equal(action.requiresApproval, true);
  assert.deepEqual(action.inputBindings, { service: "nginx.service" });
  assert.deepEqual(plan.steps[1]?.inputBindings, { service: "nginx.service" });
  assert.deepEqual(plan.steps.at(-1)?.inputBindings, { service: "nginx.service" });
});

test("investigation.playbook adds a separately approved package install with version verification", async () => {
  const seen: InvestigationPlan[] = [];
  const tool = investigationPlaybookTool(hostThatRecords(seen));
  const plan = await tool.execute(
    {
      template: "package_install",
      manager: "apt",
      package: "nginx",
      version: "1.27.0-1",
    },
    context(),
  );

  assert.deepEqual(plan.steps.map((step) => step.allowedTools), [
    ["server.info"],
    ["package.inspect"],
    ["investigation.finding"],
    ["investigation.brief"],
    ["package.install"],
    ["package.inspect"],
  ]);
  const action = plan.steps.find((step) => step.kind === "action");
  assert(action);
  assert.equal(action.riskLevel, "high");
  assert.equal(action.requiresApproval, true);
  assert.deepEqual(plan.steps[1]?.inputBindings, { manager: "apt", package: "nginx" });
  assert.deepEqual(action.inputBindings, { manager: "apt", package: "nginx", version: "1.27.0-1" });
  assert.deepEqual(plan.steps.at(-1)?.inputBindings, { manager: "apt", package: "nginx" });
});

test("investigation.playbook adds a separately approved backup cleanup with a ledger verification", async () => {
  const seen: InvestigationPlan[] = [];
  const plan = await investigationPlaybookTool(hostThatRecords(seen)).execute(
    {
      template: "backup_cleanup",
      path: "/etc/yukinal/app.env",
      backupPath: "/etc/.yukinal-backup-0123456789abcdef-0123456789abcdef0123456789abcdef",
      expectedRevision: "a".repeat(64),
    },
    context(),
  );

  assert.deepEqual(plan.steps.map((step) => step.allowedTools), [
    ["filesystem.backup.list"],
    ["investigation.finding"],
    ["investigation.brief"],
    ["filesystem.backup.cleanup"],
    ["filesystem.backup.list"],
  ]);
  const action = plan.steps[3];
  assert(action);
  assert.equal(action.riskLevel, "medium");
  assert.equal(action.requiresApproval, true);
  assert.deepEqual(action.inputBindings, {
    path: "/etc/yukinal/app.env",
    backupPath: "/etc/.yukinal-backup-0123456789abcdef-0123456789abcdef0123456789abcdef",
    expectedRevision: "a".repeat(64),
  });
  assert.deepEqual(plan.steps[0]?.inputBindings, { path: "/etc/yukinal/app.env", status: "available" });
  assert.deepEqual(plan.steps[4]?.inputBindings, { path: "/etc/yukinal/app.env", status: "available" });
});

test("investigation.playbook compiles a batch backup rotation into one always-approved step", async () => {
  const seen: InvestigationPlan[] = [];
  const items = [
    {
      path: "/etc/yukinal/app.env",
      backupPath: "/etc/.yukinal-backup-0123456789abcdef-0123456789abcdef0123456789abcdef",
      expectedRevision: "a".repeat(64),
    },
    {
      path: "/etc/yukinal/db.env",
      backupPath: "/etc/.yukinal-backup-fedcba9876543210-fedcba9876543210fedcba9876543210",
      expectedRevision: "b".repeat(64),
    },
  ];
  const plan = await investigationPlaybookTool(hostThatRecords(seen)).execute(
    { template: "backup_rotation", items },
    context(),
  );

  assert.deepEqual(plan.steps.map((step) => step.allowedTools), [
    ["filesystem.backup.retention"],
    ["investigation.finding"],
    ["investigation.brief"],
    ["filesystem.backup.cleanup"],
    ["filesystem.backup.list"],
  ]);
  const action = plan.steps[3];
  assert(action);
  assert.equal(action.kind, "action");
  assert.equal(action.riskLevel, "medium");
  assert.equal(action.requiresApproval, true);
  assert.deepEqual(action.inputBindings, { items: JSON.stringify(items) });
  assert.equal(seen.length, 1);
});

test("investigation.playbook attaches a host-owned observation window to final verification", async () => {
  const seen: InvestigationPlan[] = [];
  const plan = await investigationPlaybookTool(hostThatRecords(seen)).execute(
    {
      template: "container_restart",
      container: "api_1",
      observationWindow: {
        durationSeconds: 120,
        intervalSeconds: 20,
        allowedTools: ["docker.inspect"],
        successCriteria: ["container remains running and restart count does not increase"],
      },
    },
    context(),
  );

  assert.deepEqual(plan.observationWindow, {
    durationSeconds: 120,
    intervalSeconds: 20,
    allowedTools: ["docker.inspect"],
    successCriteria: ["container remains running and restart count does not increase"],
    status: "pending",
    sampleCount: 0,
  });
  assert.equal(plan.steps.at(-1)?.kind, "verification");
  assert.deepEqual(plan.steps.at(-1)?.allowedTools, ["docker.inspect"]);
  assert.deepEqual(plan, seen[0]);
});

test("investigation.playbook rejects an observation window that expands final verification scope", async () => {
  const seen: InvestigationPlan[] = [];
  const tool = investigationPlaybookTool(hostThatRecords(seen));

  await assert.rejects(
    tool.execute(
      {
        template: "config_edit",
        path: "/etc/yukinal/app.env",
        observationWindow: {
          durationSeconds: 60,
          intervalSeconds: 10,
          allowedTools: ["server.info"],
          successCriteria: ["server remains healthy"],
        },
      },
      context(),
    ),
    (error: unknown) => error instanceof ToolFailure && error.code === "invalid_input",
  );
  assert.equal(seen.length, 0);
});

test("investigation.playbook rejects template parameters that would be ambiguous", () => {
  const tool = investigationPlaybookTool(hostThatRecords([]));

  assert.equal(tool.input.safeParse({ template: "config_edit" }).success, false);
  assert.equal(tool.input.safeParse({ template: "config_edit", path: "relative.env" }).success, false);
  assert.equal(tool.input.safeParse({ template: "container_restart" }).success, false);
  assert.equal(tool.input.safeParse({ template: "systemd_restart" }).success, false);
  assert.equal(tool.input.safeParse({ template: "systemd_restart", service: "nginx" }).success, false);
  assert.equal(tool.input.safeParse({ template: "systemd_restart", service: "nginx.service;id" }).success, false);
  assert.equal(tool.input.safeParse({ template: "package_install" }).success, false);
  assert.equal(tool.input.safeParse({ template: "package_install", manager: "apk", package: "nginx" }).success, false);
  assert.equal(tool.input.safeParse({ template: "package_install", manager: "apt", package: "nginx;id" }).success, false);
  assert.equal(tool.input.safeParse({ template: "package_install", manager: "apt", package: "nginx", version: "1;id" }).success, false);
  assert.equal(tool.input.safeParse({ template: "deploy_sequence" }).success, false);
  assert.equal(tool.input.safeParse({ template: "deploy_sequence", steps: [{ operation: "edit_file", path: "relative.env" }] }).success, false);
  assert.equal(tool.input.safeParse({ template: "deploy_sequence", steps: [{ operation: "restart_container", container: "api;rm -rf /" }] }).success, false);
  assert.equal(tool.input.safeParse({ template: "deploy_sequence", steps: [{ operation: "restart_service", service: "nginx" }] }).success, false);
  assert.equal(tool.input.safeParse({ template: "deploy_sequence", steps: [{ operation: "restart_service", service: "nginx.service;id" }] }).success, false);
  assert.equal(tool.input.safeParse({ template: "deploy_sequence", steps: [{ operation: "install_package", manager: "apk", package: "nginx" }] }).success, false);
  assert.equal(tool.input.safeParse({ template: "deploy_sequence", steps: [{ operation: "install_package", manager: "apt", package: "nginx;id" }] }).success, false);
  assert.equal(tool.input.safeParse({ template: "readonly_health", path: "/tmp/x" }).success, false);
  assert.equal(tool.input.safeParse({ template: "config_edit", steps: [{ operation: "edit_file", path: "/tmp/x" }] }).success, false);
  assert.equal(tool.input.safeParse({ template: "backup_rotation" }).success, false);
  assert.equal(tool.input.safeParse({ template: "backup_rotation", items: [] }).success, false);
  assert.equal(tool.input.safeParse({ template: "backup_rotation", items: [{ path: "relative", backupPath: "/etc/.b", expectedRevision: "a".repeat(64) }] }).success, false);
  assert.equal(tool.input.safeParse({ template: "backup_rotation", items: [{ path: "/etc/a", backupPath: "/etc/.b", expectedRevision: "bad" }] }).success, false);
  assert.equal(tool.input.safeParse({ template: "readonly_health", items: [{ path: "/etc/a", backupPath: "/etc/.b", expectedRevision: "a".repeat(64) }] }).success, false);
  assert.equal(tool.input.safeParse({ template: "backup_rotation", items: [{ path: "/etc/a", backupPath: "/etc/.b", expectedRevision: "a".repeat(64) }, { path: "/etc/a", backupPath: "/etc/.b", expectedRevision: "a".repeat(64) }] }).success, false);
  assert.equal(tool.input.safeParse({ template: "container_restart", container: "api;rm -rf /" }).success, false);
  assert.equal(tool.input.safeParse({ template: "readonly_health", service: "nginx.service" }).success, false);
  assert.equal(
    tool.input.safeParse({
      template: "readonly_health",
      observationWindow: {
        durationSeconds: 30,
        intervalSeconds: 31,
        allowedTools: ["server.info"],
        successCriteria: ["health remains stable"],
      },
    }).success,
    false,
  );
  assert.equal(tool.input.safeParse({ template: "readonly_health", extra: true }).success, false);
});

test("investigation.playbook requires a durable task and keeps host failures", async () => {
  const tool = investigationPlaybookTool(hostThatRecords([]));

  await assert.rejects(
    tool.execute({ template: "readonly_health" }, context("")),
    (error: unknown) => error instanceof ToolFailure && error.code === "invalid_input",
  );

  const failingHost = {
    recordPlan: async () => ({
      recorded: false as const,
      error: { code: "transport" as const, message: "host unavailable", retryable: true },
    }),
  } as unknown as HostRpcClient;
  await assert.rejects(
    investigationPlaybookTool(failingHost).execute({ template: "readonly_health" }, context()),
    (error: unknown) => error instanceof ToolFailure && error.code === "transport" && error.retryable,
  );

  await assert.rejects(
    investigationPlaybookTool(hostThatRecords([])).execute(
      { template: "readonly_health" },
      { ...context(), target: { host: "local", environment: "local" } },
    ),
    (error: unknown) => error instanceof ToolFailure && error.code === "unsupported" && !error.retryable,
  );
});
