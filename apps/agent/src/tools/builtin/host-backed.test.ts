import assert from "node:assert/strict";
import test from "node:test";

import type { HostToolExecuteRequest, HostToolExecuteResponse, ToolTarget } from "@yukinal/shared";

import { dockerPsTool } from "./docker-ps.js";
import { dockerRestartTool } from "./docker-restart.js";
import { filesystemBackupTool } from "./filesystem-backup.js";
import { filesystemBackupListTool } from "./filesystem-backup-list.js";
import { filesystemBackupRetentionTool } from "./filesystem-backup-retention.js";
import { filesystemBackupCleanupTool } from "./filesystem-backup-cleanup.js";
import { filesystemEditTool } from "./filesystem-edit.js";
import { filesystemReadTool } from "./filesystem-read.js";
import { filesystemRestoreTool } from "./filesystem-restore.js";
import { filesystemWriteTool } from "./filesystem-write.js";
import { investigationEvidenceSearchTool } from "./investigation-evidence-search.js";
import { investigationEvidenceCompareTool } from "./investigation-evidence-compare.js";
import { investigationEvidenceCorrelateTool } from "./investigation-evidence-correlate.js";
import { investigationRetentionPreviewTool } from "./investigation-retention-preview.js";
import { packageInspectTool } from "./package-inspect.js";
import { packageInstallTool } from "./package-install.js";
import { serverInfoTool } from "./server-info.js";
import { serverLogsTool } from "./server-logs.js";
import { serverServicesTool } from "./server-services.js";
import { systemdInspectTool } from "./systemd-inspect.js";
import { systemdRestartTool } from "./systemd-restart.js";
import { ToolFailure, type ToolContext } from "../tool.js";
import type { HostToolExecutor } from "./host-backed.js";

const target: ToolTarget = { host: "remote", serverId: "srv_01abc", environment: "staging" };
const context: ToolContext = {
  callId: "call_1",
  traceId: "trace_1",
  target,
  signal: new AbortController().signal,
  deadlineAt: Date.now() + 10_000,
  log: () => {},
};

function fakeHost(response: HostToolExecuteResponse, seen: HostToolExecuteRequest[] = []): HostToolExecutor {
  return {
    execute: async (request) => {
      seen.push(request);
      return response;
    },
  };
}

test("host-backed tools forward the resolved target and validate structured output", async () => {
  const seen: HostToolExecuteRequest[] = [];
  const tool = dockerPsTool(
    fakeHost(
      {
        status: "success",
        output: { available: true, containers: [{ name: "web", image: "nginx:1.27", state: "running", status: "Up 1h", restartCount: 0 }] },
      },
      seen,
    ),
  );

  const output = await tool.execute({ all: false }, context);

  assert.deepEqual(output.containers[0]?.name, "web");
  assert.equal(seen[0]?.toolName, "docker.ps");
  assert.deepEqual(seen[0]?.target, target);
});

test("host-backed failures retain the shared error code", async () => {
  const tool = serverInfoTool(
    fakeHost({
      status: "failed",
      error: { code: "transport", message: "server unavailable", retryable: true },
    }),
  );

  await assert.rejects(
    tool.execute({}, context),
    (error: unknown) => error instanceof ToolFailure && error.code === "transport" && error.retryable,
  );
});

test("server logs and services stay structured at the host boundary", async () => {
  const seen: HostToolExecuteRequest[] = [];
  const logs = serverLogsTool(
    fakeHost(
      {
        status: "success",
        output: {
          source: "journalctl",
          lines: [{ text: "app: failed to connect", level: "error" }],
        },
      },
      seen,
    ),
  );
  const services = serverServicesTool(
    fakeHost({
      status: "success",
      output: {
        source: "systemd",
        services: [{ name: "nginx.service", state: "failed", status: "failed/failed" }],
      },
    }, seen),
  );

  const logOutput = await logs.execute({ sinceSeconds: 3_600, unit: "nginx.service" }, context);
  const serviceOutput = await services.execute({ name: "nginx.service", state: "failed" }, context);

  assert.equal(logOutput.source, "journalctl");
  assert.equal(logOutput.lines[0]?.level, "error");
  assert.equal(serviceOutput.services[0]?.state, "failed");
  assert.deepEqual(seen.map((request) => request.toolName), ["server.logs", "server.services"]);
  assert.deepEqual(seen[0]?.input, { sinceSeconds: 3_600, unit: "nginx.service" });
  assert.deepEqual(seen[1]?.input, { name: "nginx.service", state: "failed" });
});

const REVISION = "a".repeat(64);

test("investigation evidence search returns metadata and keeps raw bodies out", async () => {
  const tool = investigationEvidenceSearchTool({
    searchEvidence: async (request) => {
      assert.equal(request.taskId, "task_1");
      return {
        status: "success",
        evidence: [{
          id: "ev_1",
          taskId: "task_1",
          scope: target,
          kind: "log",
          sourceTool: "docker.logs",
          collectedAt: "2026-09-19T00:00:00.000Z",
          inputSummary: "tail=100",
          contentType: "text",
          contentHash: REVISION,
          truncated: false,
          redactionStatus: "clean",
        }],
      };
    },
  });
  const output = await tool.execute({ sourceTool: "docker.logs", limit: 4 }, { ...context, taskId: "task_1" });
  assert.equal(tool.risk, "read");
  assert.equal(output.evidence[0]?.id, "ev_1");
  assert.equal("content" in (output.evidence[0] ?? {}), false);
});

test("investigation evidence compare returns bounded differences without raw bodies", async () => {
  const tool = investigationEvidenceCompareTool({
    compareEvidence: async (request) => {
      assert.deepEqual(request, { taskId: "task_1", leftEvidenceId: "ev_1", rightEvidenceId: "ev_2" });
      return {
        status: "success",
        comparison: {
          status: "changed",
          shape: "json",
          left: {
            id: "ev_1", taskId: "task_1", scope: target, kind: "snapshot", sourceTool: "server.info",
            collectedAt: "2026-09-19T00:00:00.000Z", inputSummary: "{}", contentType: "json",
            contentHash: REVISION, truncated: false, redactionStatus: "clean",
          },
          right: {
            id: "ev_2", taskId: "task_1", scope: target, kind: "snapshot", sourceTool: "server.info",
            collectedAt: "2026-09-19T00:05:00.000Z", inputSummary: "{}", contentType: "json",
            contentHash: "b".repeat(64), truncated: false, redactionStatus: "clean",
          },
          changedPaths: ["$.status"],
          changedPathCount: 1,
          diffTruncated: false,
          warnings: [],
        },
      };
    },
  });
  const output = await tool.execute(
    { leftEvidenceId: "ev_1", rightEvidenceId: "ev_2" },
    { ...context, taskId: "task_1" },
  );
  assert.equal(tool.risk, "read");
  assert.deepEqual(output.comparison.changedPaths, ["$.status"]);
  assert.equal("content" in output.comparison.left, false);
  assert.equal("content" in output.comparison.right, false);
});

test("investigation evidence correlation returns same-run metadata without raw bodies", async () => {
  const tool = investigationEvidenceCorrelateTool({
    correlateEvidence: async (request) => {
      assert.deepEqual(request, { taskId: "task_1", anchorEvidenceId: "ev_1", limit: 8 });
      return {
        status: "success",
        correlation: {
          anchor: {
            id: "ev_1", taskId: "task_1", runId: "run_1", scope: target, kind: "snapshot", sourceTool: "server.info",
            collectedAt: "2026-09-19T00:00:00.000Z", inputSummary: "health", contentType: "json",
            contentHash: REVISION, truncated: false, redactionStatus: "clean",
          },
          evidence: [{
            id: "ev_2", taskId: "task_1", runId: "run_1", scope: target, kind: "log", sourceTool: "server.logs",
            collectedAt: "2026-09-19T00:00:01.000Z", inputSummary: "tail=20", contentType: "text",
            contentHash: "b".repeat(64), truncated: false, redactionStatus: "clean",
          }],
          matchedBy: "same_run",
          windowSeconds: 300,
          sourceTools: ["server.info", "server.logs"],
          warnings: [],
        },
      };
    },
  });
  const output = await tool.execute({ anchorEvidenceId: "ev_1", limit: 8 }, { ...context, taskId: "task_1" });
  assert.equal(tool.risk, "read");
  assert.equal(output.correlation.matchedBy, "same_run");
  assert.equal("content" in (output.correlation.anchor ?? {}), false);
});

test("investigation retention preview is read-only and requires a durable task", async () => {
  const tool = investigationRetentionPreviewTool({
    previewRetention: async (request) => {
      assert.deepEqual(request, { taskId: "task_1", limit: 8 });
      return {
        status: "success",
        preview: {
          taskId: "task_1",
          cutoffAt: "2026-02-01T00:00:00Z",
          candidates: [{
            id: "ev_old",
            taskId: "task_1",
            kind: "evidence",
            createdAt: "2026-01-01T00:00:00Z",
            bytes: 12,
            reason: "unreferenced_evidence",
          }],
          protectedCount: 1,
          candidateBytes: 12,
          truncated: false,
        },
      };
    },
  });
  const output = await tool.execute({ limit: 8 }, { ...context, taskId: "task_1" });
  assert.equal(tool.name, "investigation.retention.preview");
  assert.equal(tool.risk, "read");
  assert.equal(output.candidates[0]?.id, "ev_old");
  await assert.rejects(
    tool.execute({ limit: 8 }, context),
    (error: unknown) => error instanceof ToolFailure && error.code === "invalid_input",
  );
});

test("filesystem tools validate bounded output and declare writes as medium risk", async () => {
  const read = filesystemReadTool(
    fakeHost({
      status: "success",
      output: { path: "/etc/app.env", content: "PORT=8080", truncated: false, revision: REVISION },
    }),
  );
  const readOutput = await read.execute({ path: "/etc/app.env", maxBytes: 4096 }, context);
  assert.equal(readOutput.content, "PORT=8080");
  assert.equal(readOutput.revision, REVISION);

  const write = filesystemWriteTool(
    fakeHost({ status: "success", output: { path: "/etc/app.env", bytesWritten: 9 } }),
  );
  const writeOutput = await write.execute({ path: "/etc/app.env", content: "PORT=8080" }, context);
  assert.equal(write.risk, "medium");
  assert.equal(write.effectful, true);
  assert.equal(writeOutput.bytesWritten, 9);
});

test("filesystem.backup.list is read-only, task-scoped metadata", async () => {
  const seen: HostToolExecuteRequest[] = [];
  const tool = filesystemBackupListTool(
    fakeHost({
      status: "success",
      output: {
        backups: [{
          id: "backup_1",
          serverId: "srv_01abc",
          taskId: "task_1",
          path: "/etc/app.env",
          backupPath: "/etc/.yukinal-backup-0123456789abcdef-0123456789abcdef0123456789abcdef",
          revision: REVISION,
          bytesBackedUp: 12,
          status: "available",
          createdAt: "2026-09-20T00:00:00Z",
          updatedAt: "2026-09-20T00:00:00Z",
        }],
        truncated: false,
      },
    }, seen),
  );
  const output = await tool.execute({ status: "available", limit: 8 }, { ...context, taskId: "task_1" });
  assert.equal(tool.risk, "read");
  assert.equal(output.backups[0]?.backupPath.includes(".yukinal-backup-"), true);
  assert.equal(seen[0]?.toolName, "filesystem.backup.list");
  assert.equal(seen[0]?.taskId, "task_1");
});

test("filesystem.read output without a revision is rejected instead of reaching the model", async () => {
  // The revision is the precondition of every edit: a read result that silently loses it would
  // leave the model with no way to cite one, so the schema refuses the payload outright.
  const read = filesystemReadTool(
    fakeHost({ status: "success", output: { path: "/etc/app.env", content: "PORT=8080", truncated: false } }),
  );
  await assert.rejects(
    read.execute({ path: "/etc/app.env" }, context),
    (error: unknown) => error instanceof ToolFailure && error.code === "internal",
  );
});

test("filesystem.edit forwards the guard inputs and returns the new revision", async () => {
  const seen: HostToolExecuteRequest[] = [];
  const tool = filesystemEditTool(
    fakeHost({ status: "success", output: { path: "/etc/app.env", revision: REVISION, bytesBefore: 10, bytesAfter: 20, lineDelta: 1 } }, seen),
  );

  const output = await tool.execute(
    { path: "/etc/app.env", expectedRevision: REVISION, oldString: "PORT=8080", newString: "PORT=9090\nexport PORT" },
    context,
  );

  assert.equal(tool.name, "filesystem.edit");
  assert.equal(tool.risk, "medium");
  assert.equal(output.revision, REVISION);
  assert.equal(output.bytesAfter, 20);
  assert.equal(output.lineDelta, 1);
  assert.equal(seen[0]?.toolName, "filesystem.edit");
  assert.deepEqual(seen[0]?.target, target);
  assert.deepEqual(seen[0]?.input, {
    path: "/etc/app.env",
    expectedRevision: REVISION,
    oldString: "PORT=8080",
    newString: "PORT=9090\nexport PORT",
  });
});

test("filesystem.edit rejects an input the guard could never satisfy", () => {
  // These are the shapes the model must not be able to send at all: a revision that is not a
  // revision, an empty oldString (it occurs everywhere), and unknown fields.
  const input = filesystemEditTool(fakeHost({ status: "success", output: {} })).input;
  const valid = { path: "/etc/app.env", expectedRevision: REVISION, oldString: "a", newString: "b" };

  assert.equal(input.safeParse(valid).success, true);
  assert.equal(input.safeParse({ ...valid, expectedRevision: "not-a-revision" }).success, false);
  assert.equal(input.safeParse({ ...valid, oldString: "" }).success, false);
  assert.equal(input.safeParse({ ...valid, path: "relative/app.env" }).success, false);
  assert.equal(input.safeParse({ ...valid, extra: true }).success, false);
});

test("a stale revision survives the host boundary as a retryable invalid_input", async () => {
  // The code the edit guard's mismatch maps to on the Rust side (`filesystem_failure`). The Agent
  // must see it as a fixable input problem — re-read, then retry — with the drifted revisions in
  // `detail`, not as a transport failure or a dead end.
  const tool = filesystemEditTool(
    fakeHost({
      status: "failed",
      error: {
        code: "invalid_input",
        message: "the file is not the revision that was read: expected aa, the file is now bb; re-read the file and retry",
        retryable: true,
        detail: { expectedRevision: "aa", actualRevision: "bb" },
      },
    }),
  );

  await assert.rejects(
    tool.execute({ path: "/etc/app.env", expectedRevision: REVISION, oldString: "a", newString: "b" }, context),
    (error: unknown) =>
      error instanceof ToolFailure &&
      error.code === "invalid_input" &&
      error.retryable &&
      (error.detail as { actualRevision?: string }).actualRevision === "bb",
  );
});

test("a file over the edit cap is reported as unfixable, not as something to retry", async () => {
  const tool = filesystemEditTool(
    fakeHost({
      status: "failed",
      error: {
        code: "invalid_input",
        message: "the file is larger than the 524288-byte cap this tool can read in full",
        retryable: false,
        detail: { maxEditableBytes: 524_288 },
      },
    }),
  );

  await assert.rejects(
    tool.execute({ path: "/etc/app.env", expectedRevision: REVISION, oldString: "a", newString: "b" }, context),
    (error: unknown) =>
      error instanceof ToolFailure &&
      error.code === "invalid_input" &&
      !error.retryable &&
      (error.detail as { maxEditableBytes?: number }).maxEditableBytes === 524_288,
  );
});

test("filesystem.edit's description states the guard, the cap and the remaining race", () => {
  const tool = filesystemEditTool(fakeHost({ status: "success", output: {} }));
  // The description is user-facing prose and the model's only briefing. Losing any of these facts
  // from it is a behaviour change, so they are pinned here — including what the edit now refuses
  // (ADR 0017): a hard link, a symlink, metadata it cannot carry, and a server that cannot publish
  // atomically are all "do something else", not "try again".
  assert.match(tool.description, /expectedRevision/);
  assert.match(tool.description, /exactly once/);
  assert.match(tool.description, /512 KiB/);
  assert.match(tool.description, /rename/);
  assert.match(tool.description, /hard links/);
  assert.match(tool.description, /mode, mtime, owner and group/);
  assert.match(tool.description, /unsupported/);
  assert.match(tool.description, /compare-and-swap/);
  assert.match(tool.description, /filesystem\.write/);
});

test("filesystem backup creates a bounded recovery copy and restore is high-risk", async () => {
  const seen: HostToolExecuteRequest[] = [];
  const backup = filesystemBackupTool(
    fakeHost({
      status: "success",
      output: {
        path: "/etc/app.env",
        backupPath: "/etc/.yukinal-backup-0123456789abcdef-0123456789abcdef0123456789abcdef",
        revision: REVISION,
        bytesBackedUp: 10,
      },
    }, seen),
  );
  const restore = filesystemRestoreTool(
    fakeHost({
      status: "success",
      output: {
        path: "/etc/app.env",
        backupPath: "/etc/.yukinal-backup-0123456789abcdef-0123456789abcdef0123456789abcdef",
        revision: "b".repeat(64),
        bytesBefore: 20,
        bytesAfter: 10,
      },
    }, seen),
  );
  const cleanup = filesystemBackupCleanupTool(
    fakeHost({
      status: "success",
      output: {
        path: "/etc/app.env",
        backupPath: "/etc/.yukinal-backup-0123456789abcdef-0123456789abcdef0123456789abcdef",
        revision: REVISION,
        bytesDeleted: 10,
      },
    }, seen),
  );

  const backedUp = await backup.execute({ path: "/etc/app.env" }, context);
  const restored = await restore.execute({
    path: "/etc/app.env",
    backupPath: backedUp.backupPath,
    expectedRevision: REVISION,
  }, context);
  assert.equal(backup.risk, "medium");
  assert.equal(backedUp.bytesBackedUp, 10);
  assert.equal(restore.risk, "high");
  assert.equal(restored.bytesAfter, 10);
  const cleaned = await cleanup.execute({
    path: "/etc/app.env",
    backupPath: backedUp.backupPath,
    expectedRevision: REVISION,
  }, context);
  assert.equal(cleanup.risk, "medium");
  assert.ok(!("items" in cleaned), "a single-item cleanup returns the single-item output");
  assert.equal(cleaned.bytesDeleted, 10);
  assert.deepEqual(seen.map((request) => request.toolName), ["filesystem.backup", "filesystem.restore", "filesystem.backup.cleanup"]);
});

test("filesystem.backup.retention is read-only and reports batch cleanup output", async () => {
  const retention = filesystemBackupRetentionTool(
    fakeHost({
      status: "success",
      output: { candidates: [], truncated: false, keptCount: 0, scannedCount: 0 },
    }),
  );
  assert.equal(retention.risk, "read");
  assert.notEqual(retention.effectful, true);
  const plan = await retention.execute({ olderThanDays: 30 }, context);
  assert.deepEqual(plan, { candidates: [], truncated: false, keptCount: 0, scannedCount: 0 });

  const cleanup = filesystemBackupCleanupTool(
    fakeHost({
      status: "success",
      output: {
        items: [
          { path: "/etc/app.env", backupPath: "/etc/.b1", outcome: "removed", revision: "a".repeat(64), bytesDeleted: 3 },
          { path: "/etc/app.env", backupPath: "/etc/.b2", outcome: "skipped", reason: "owned by another task" },
        ],
        partial: true,
      },
    }),
  );
  const batch = await cleanup.execute(
    {
      items: [
        { path: "/etc/app.env", backupPath: "/etc/.b1", expectedRevision: "a".repeat(64) },
        { path: "/etc/app.env", backupPath: "/etc/.b2", expectedRevision: "a".repeat(64) },
      ],
    },
    context,
  );
  assert.ok("items" in batch);
  assert.equal(batch.partial, true);
  assert.equal(batch.items.length, 2);
});

test("a host refusal that cannot be retried keeps its own code", async () => {
  // `unsupported` is the code for "this remote cannot do it, and retrying changes nothing".
  // Collapsing it into invalid_input would tell the model to re-read and try again forever.
  const tool = filesystemEditTool(
    fakeHost({
      status: "failed",
      error: {
        code: "unsupported",
        message: "/etc/app.env has 3 hard links; replacing it would leave the other names pointing at the old content",
        retryable: false,
      },
    }),
  );

  await assert.rejects(
    () =>
      tool.execute(
        { path: "/etc/app.env", expectedRevision: "a".repeat(64), oldString: "a", newString: "b" },
        context,
      ),
    (error: unknown) =>
      error instanceof ToolFailure && error.code === "unsupported" && !error.retryable,
  );
});

test("docker restart is exposed as a high-risk host action", async () => {
  const tool = dockerRestartTool(
    fakeHost({ status: "success", output: { container: "api_1", restarted: true } }),
  );
  const output = await tool.execute({ container: "api_1", timeoutSeconds: 15 }, context);
  assert.equal(tool.risk, "high");
  assert.equal(output.restarted, true);
});

test("systemd restart is exposed as a high-risk host action and inspect is bounded", async () => {
  const seen: HostToolExecuteRequest[] = [];
  const inspect = systemdInspectTool(
    fakeHost({
      status: "success",
      output: {
        service: "nginx.service",
        loadState: "loaded",
        activeState: "active",
        subState: "running",
        description: "Nginx",
      },
    }, seen),
  );
  const restart = systemdRestartTool(
    fakeHost({ status: "success", output: { service: "nginx.service", restarted: true } }, seen),
  );

  const inspected = await inspect.execute({ service: "nginx.service" }, context);
  const restarted = await restart.execute({ service: "nginx.service", timeoutSeconds: 20 }, context);

  assert.equal(inspected.activeState, "active");
  assert.equal(restart.risk, "high");
  assert.equal(restarted.restarted, true);
  assert.deepEqual(seen.map((request) => request.toolName), ["systemd.inspect", "systemd.restart"]);
});

test("package install is high-risk and package inspect returns a normalized state", async () => {
  const seen: HostToolExecuteRequest[] = [];
  const inspect = packageInspectTool(
    fakeHost({
      status: "success",
      output: { manager: "apt", package: "nginx", installed: true, version: "1.27.0-1" },
    }, seen),
  );
  const install = packageInstallTool(
    fakeHost({
      status: "success",
      output: { manager: "apt", package: "nginx", version: "1.27.0-1", installed: true },
    }, seen),
  );

  const inspected = await inspect.execute({ manager: "apt", package: "nginx" }, context);
  const installed = await install.execute({ manager: "apt", package: "nginx", version: "1.27.0-1" }, context);
  assert.equal(inspected.installed, true);
  assert.equal(inspected.version, "1.27.0-1");
  assert.equal(install.risk, "high");
  assert.equal(installed.installed, true);
  assert.deepEqual(seen.map((request) => request.toolName), ["package.inspect", "package.install"]);
});
