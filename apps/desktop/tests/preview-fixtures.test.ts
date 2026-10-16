import assert from "node:assert/strict";
import test from "node:test";

import { IPC_COMMANDS, IPC_SCHEMAS, type IpcCommandName } from "@yukinal/shared";
import { isPreviewFixture, previewResponse } from "../src/lib/preview-fixtures.js";
import { snapshotAge } from "../src/features/overview/ServerOverview.js";

function response<C extends IpcCommandName>(command: C, params: unknown) {
  return IPC_SCHEMAS[command].response.parse(previewResponse(command, IPC_SCHEMAS[command].params.parse(params)));
}

test("development fixture data satisfies the real IPC response contracts", () => {
  const cases: Array<[IpcCommandName, unknown]> = [
    [IPC_COMMANDS.corePing, {}],
    [IPC_COMMANDS.serverList, {}],
    [IPC_COMMANDS.workspaceList, {}],
    [IPC_COMMANDS.serverSnapshot, { serverId: "srv_previewdb" }],
    [IPC_COMMANDS.serverServices, { serverId: "srv_previewdb" }],
    [IPC_COMMANDS.serverLogs, { serverId: "srv_previewdb" }],
    [IPC_COMMANDS.remoteFileList, { serverId: "srv_previewdb", path: "/srv/app/config" }],
    [IPC_COMMANDS.remoteFileRead, { serverId: "srv_previewdb", path: "/srv/app/docker-compose.yml" }],
    [IPC_COMMANDS.remoteFilePreview, { serverId: "srv_previewdb", path: "/srv/app/docker-compose.yml" }],
    [IPC_COMMANDS.localFilePreview, { handleId: "fixture-local-config" }],
    [IPC_COMMANDS.localPathPick, { kind: "uploadFiles" }],
    [IPC_COMMANDS.localPathPick, { kind: "uploadDirectory" }],
    [IPC_COMMANDS.localPathPick, { kind: "downloadDirectory" }],
    [IPC_COMMANDS.fileTransferList, { serverId: "srv_previewdb" }],
    [IPC_COMMANDS.fileTransferGet, { transferId: "transfer_preview_conflict" }],
    [IPC_COMMANDS.activityList, { serverId: "srv_previewdb", limit: 50 }],
    [IPC_COMMANDS.toolExecutionList, { traceId: "trace_preview", limit: 50 }],
    [IPC_COMMANDS.providerList, {}],
    [IPC_COMMANDS.providerModels, { providerId: "prv_preview" }],
    [IPC_COMMANDS.providerModels, { providerId: "prv_preview_local" }],
    [IPC_COMMANDS.agentStatus, {}],
    [IPC_COMMANDS.agentLogs, {}],
    [IPC_COMMANDS.chatSessionList, {}],
    [IPC_COMMANDS.chatSessionGet, { sessionId: "ses_preview_triage" }],
    [IPC_COMMANDS.investigationTaskList, {}],
    [IPC_COMMANDS.investigationTaskGet, { taskId: "task_preview" }],
    [IPC_COMMANDS.investigationScheduleList, {}],
    [IPC_COMMANDS.investigationScheduleRuns, { scheduleId: "schedule_preview" }],
    [IPC_COMMANDS.mcpServerList, {}],
    [IPC_COMMANDS.networkProxyGet, {}],
    [IPC_COMMANDS.serverHostKeyStatus, { serverId: "srv_previewdb" }],
  ];
  for (const [command, params] of cases) {
    assert.doesNotThrow(() => response(command, params), command);
  }
});

test("new snapshots stay fresh between independent polling and display-clock ticks", (context) => {
  let requestTime = Date.parse("2026-10-16T04:00:00.000Z");
  const mountedAt = requestTime - 9_500;
  context.mock.method(Date, "now", () => requestTime);
  let previousSampleTime = 0;
  // Fifteen minutes of 15-second polling against a separately phased 10-second
  // display clock reproduces the intermittent negative-age regression.
  for (let poll = 0; poll < 60; poll++) {
    const displayClock = mountedAt + Math.floor((requestTime - mountedAt) / 10_000) * 10_000;
    const { snapshot } = response(IPC_COMMANDS.serverSnapshot, { serverId: "srv_previewdb" }) as ReturnType<typeof IPC_SCHEMAS.server_snapshot.response.parse>;
    const sampleTime = Date.parse(snapshot.collectedAt);
    assert.ok(sampleTime > previousSampleTime, "polling must advance the sample timestamp");
    assert.deepEqual(snapshotAge(snapshot.collectedAt, displayClock, requestTime + 4), { kind: "fresh", label: "刚刚" });
    assert.ok(snapshot.collectors?.every((collector) => collector.collectedAt === snapshot.collectedAt));
    previousSampleTime = sampleTime;
    requestTime += 15_000;
  }
});

test("snapshot freshness still detects real future timestamps and aged cached data", () => {
  const receivedAt = Date.parse("2026-10-16T04:00:00.000Z");
  assert.equal(snapshotAge(new Date(receivedAt + 5_000).toISOString(), receivedAt - 9_000, receivedAt).kind, "unknown");
  assert.equal(snapshotAge(new Date(receivedAt).toISOString(), receivedAt + 60_000, receivedAt).kind, "stale");
  assert.equal(snapshotAge("invalid", receivedAt, receivedAt).kind, "unknown");
});

test("task examples contain related evidence, decisions, execution history and schedules", () => {
  const detail = response(IPC_COMMANDS.investigationTaskGet, { taskId: "task_preview" }) as ReturnType<typeof IPC_SCHEMAS.investigation_task_get.response.parse>;
  const evidenceIds = new Set(detail.evidence.map((item) => item.id));
  assert.ok(detail.findings.length >= 2);
  assert.ok(detail.decisionBrief?.options.length);
  assert.ok(detail.plan?.steps.length);
  assert.ok(detail.artifacts.length);
  assert.ok(detail.runs.some((run) => run.status === "failed"));
  assert.ok(detail.steps.some((step) => step.status === "waiting_user"));
  for (const record of [...detail.findings, ...detail.artifacts, ...detail.steps]) {
    assert.equal(record.taskId, detail.task.id);
    for (const evidenceId of record.evidenceIds) assert.ok(evidenceIds.has(evidenceId));
  }
  const { schedules } = response(IPC_COMMANDS.investigationScheduleList, {}) as ReturnType<typeof IPC_SCHEMAS.investigation_schedule_list.response.parse>;
  assert.deepEqual(new Set(schedules.map((schedule) => schedule.status)), new Set(["active", "paused"]));
  for (const schedule of schedules) {
    assert.equal(schedule.taskId, detail.task.id);
    const { runs } = response(IPC_COMMANDS.investigationScheduleRuns, { scheduleId: schedule.id }) as ReturnType<typeof IPC_SCHEMAS.investigation_schedule_runs.response.parse>;
    assert.ok(runs.some((run) => run.status === "failed"));
    assert.ok(runs.some((run) => run.status === "succeeded"));
    assert.ok(runs.every((run) => run.scheduleId === schedule.id && run.taskId === detail.task.id));
  }
});

test("catalogs and transfer examples cover populated, long-label and failure states", () => {
  const { servers } = response(IPC_COMMANDS.mcpServerList, {}) as ReturnType<typeof IPC_SCHEMAS.mcp_server_list.response.parse>;
  assert.ok(servers.some((server) => server.status.running && server.tools.length >= 3));
  assert.ok(servers.some((server) => server.config.oauth?.flow === "device_code"));
  assert.ok(servers.some((server) => server.status.lastExit && server.status.restart?.exhausted));
  assert.ok(servers.some((server) => server.tools.some((tool) => tool.name.length > 80)));
  const { providers } = response(IPC_COMMANDS.providerList, {}) as ReturnType<typeof IPC_SCHEMAS.provider_list.response.parse>;
  assert.ok(providers.some((provider) => !provider.enabled));
  assert.ok(providers.some((provider) => provider.models?.some((model) => !model.supportsToolCalling)));
  const { transfers } = response(IPC_COMMANDS.fileTransferList, { serverId: "srv_previewdb" }) as ReturnType<typeof IPC_SCHEMAS.file_transfer_list.response.parse>;
  assert.deepEqual(new Set(transfers.map((transfer) => transfer.status)), new Set(["completed", "failed", "waitingConflict"]));
  assert.ok(transfers.some((transfer) => transfer.activeConflict?.allowedActions.includes("rename")));
  assert.ok(transfers.some((transfer) => transfer.failures.length));
  assert.deepEqual(response(IPC_COMMANDS.fileTransferList, { serverId: "srv_previewapi" }), { transfers: [] });
});

test("fixture file navigation and directory picks retain their requested context", () => {
  const { entries, path } = response(IPC_COMMANDS.remoteFileList, { serverId: "srv_previewdb", path: "/srv/app/config" }) as ReturnType<typeof IPC_SCHEMAS.remote_file_list.response.parse>;
  assert.equal(path, "/srv/app/config");
  assert.ok(entries.every((entry) => entry.path.startsWith(`${path}/`)));
  for (const kind of ["uploadDirectory", "downloadDirectory"] as const) {
    const { handles } = response(IPC_COMMANDS.localPathPick, { kind }) as ReturnType<typeof IPC_SCHEMAS.local_path_pick.response.parse>;
    assert.equal(handles[0]?.kind, "directory");
  }
});

test("saved chat covers long diagnostic tables and code without starting a run", () => {
  const { messages } = response(IPC_COMMANDS.chatSessionGet, { sessionId: "ses_preview_triage" }) as ReturnType<typeof IPC_SCHEMAS.chat_session_get.response.parse>;
  const summary = messages.find((message) => message.id === "msg_preview_assistant_2");
  assert.match(summary?.content ?? "", /\| 观察项 \| 结果 \|/);
  assert.ok(summary?.content.includes("```sh\n"));
  assert.match(summary?.content ?? "", /仅供界面评估/);
});

test("fixture mutations are explicit unsupported operations and do not alter the read examples", () => {
  const before = structuredClone(previewResponse(IPC_COMMANDS.fileTransferList, {}));
  for (const command of [
    IPC_COMMANDS.mcpServerStart, IPC_COMMANDS.mcpServerSave, IPC_COMMANDS.mcpServerReview, IPC_COMMANDS.mcpOAuthConnect,
    IPC_COMMANDS.providerSave, IPC_COMMANDS.investigationTaskStart, IPC_COMMANDS.investigationBriefSelect,
    IPC_COMMANDS.investigationScheduleUpdate, IPC_COMMANDS.fileTransferUpload, IPC_COMMANDS.fileTransferDownload,
    IPC_COMMANDS.fileTransferCancel, IPC_COMMANDS.fileTransferResolveConflict, IPC_COMMANDS.filePrepareRemoteDrag, IPC_COMMANDS.fileDragOutStart,
  ]) {
    assert.throws(() => previewResponse(command, {}), /模拟数据模式仅供界面评估/, command);
  }
  assert.deepEqual(previewResponse(IPC_COMMANDS.fileTransferList, {}), before);
  assert.equal(isPreviewFixture(), false, "Node tests cannot activate the Vite development adapter");
});
