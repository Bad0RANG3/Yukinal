import assert from "node:assert/strict";
import { registerHooks } from "node:module";
import { afterEach, beforeEach, test } from "node:test";
import { createRef, type ReactNode } from "react";
import { ErrorPanel, LoadingPanel } from "../src/components/PanelStates.js";
import { renderToStaticMarkup } from "react-dom/server";
import { QueryClient, QueryClientProvider, QueryObserver, type QueryKey } from "@tanstack/react-query";
import { IPC_COMMANDS, IPC_SCHEMAS, type TransferSnapshot } from "@yukinal/shared";
import { installHost, removeHost } from "./host-mock.js";
import { previewResponse } from "../src/lib/preview-fixtures.js";
import { RUNTIME_QUERY_KEYS, useAgentLogs } from "../src/lib/runtime.js";
import { useWorkspaceStore, type WorkspaceState } from "../src/stores/workspace-store.js";
import { AgentFeed } from "../src/features/agent/AgentFeed.js";
import { AgentHeader } from "../src/features/agent/AgentHeader.js";
import { appendEntries, type Entry } from "../src/features/agent/transcript.js";

// Actual React components and QueryClient states, without a DOM, browser, or native host.
// Effects/focus/layout/real remote behavior are deliberately outside these assertions.
const styles = registerHooks({ load: (url, context, nextLoad) => url.endsWith(".css")
  ? { format: "module", source: "", shortCircuit: true } : nextLoad(url, context) });
const { MemoryLibrary } = await import("../src/features/agent/MemoryLibrary.js");
const { ServerOverview, recentServerActivityQueryOptions, startSnapshotDisplayClock } = await import("../src/features/overview/ServerOverview.js");
const { RuntimeSettings, RuntimeLogs } = await import("../src/features/settings/RuntimeSettings.js");
const { LogsPane } = await import("../src/features/logs/LogsPane.js");
const { ServicesPane } = await import("../src/features/services/ServicesPane.js");
const { ActivityFeed } = await import("../src/features/activity/ActivityFeed.js");
const { RemoteFilesPane } = await import("../src/features/files/RemoteFilesPane.js");
const { InvestigationsPane } = await import("../src/features/investigations/InvestigationsPane.js");
const { TeamBoardPane } = await import("../src/features/agent-nist/TeamBoardPane.js");
const { HostKeyGate } = await import("../src/features/servers/HostKeyGate.js");
const { GettingStarted } = await import("../src/features/onboarding/GettingStarted.js");
const { WorkpageArtifactView } = await import("../src/features/workpages/WorkpageArtifactView.js");
styles.deregister();

const initialWorkspace = useWorkspaceStore.getInitialState();
const workspaceDefaults = { ...initialWorkspace };
const originalWindow = Object.getOwnPropertyDescriptor(globalThis, "window");
const serverId = "srv_previewdb";
const servers = IPC_SCHEMAS.server_list.response.parse(previewResponse(IPC_COMMANDS.serverList, {})).servers;
const snapshot = () => IPC_SCHEMAS.server_snapshot.response.parse(previewResponse(IPC_COMMANDS.serverSnapshot, { serverId })).snapshot;
const activity = () => IPC_SCHEMAS.activity_list.response.parse(previewResponse(IPC_COMMANDS.activityList, { serverId, limit: 50 })).activities;
const remoteFiles = () => IPC_SCHEMAS.remote_file_list.response.parse(previewResponse(IPC_COMMANDS.remoteFileList, { serverId, path: "/" }));
const transferFixtures = () => IPC_SCHEMAS.file_transfer_list.response.parse(previewResponse(IPC_COMMANDS.fileTransferList, { serverId })).transfers;
let client: QueryClient;
let calls: string[];

beforeEach(() => {
  Object.defineProperty(globalThis, "window", { value: {}, configurable: true });
  calls = [];
  installHost({ invoke: async (command) => { calls.push(command); throw new Error(`No real IPC is allowed in this offline suite: ${command}`); } });
  client = new QueryClient({ defaultOptions: { queries: { retry: false, retryOnMount: false, staleTime: Infinity, gcTime: Infinity } } });
  client.setQueryData(["servers"], servers);
  client.setQueryData(["providers"], []);
  client.setQueryData(["team-board-tasks"], []);
  client.setQueryData(["investigationTasks"], { tasks: [] });
  client.setQueryData(["investigationSchedules"], { schedules: [] });
  client.setQueryData(["file-transfers", serverId], []);
  client.setQueryData(["activities", serverId], []);
  client.setQueryData(["activities", "all"], []);
});
afterEach(() => {
  client.clear();
  removeHost();
  Object.assign(initialWorkspace, workspaceDefaults);
  useWorkspaceStore.setState({ ...workspaceDefaults }, true);
  if (originalWindow) Object.defineProperty(globalThis, "window", originalWindow);
  else Reflect.deleteProperty(globalThis, "window");
  assert.deepEqual(calls, [], "static component rendering must never invoke the real host");
});
function workspace(patch: Partial<WorkspaceState>) {
  useWorkspaceStore.setState(patch);
  Object.assign(initialWorkspace, useWorkspaceStore.getState());
}
function render(node: ReactNode): string { return renderToStaticMarkup(<QueryClientProvider client={client}>{node}</QueryClientProvider>); }
function pending(key: QueryKey) { client.getQueryCache().build(client, { queryKey: key }).setState({ status: "pending", fetchStatus: "fetching", data: undefined, error: null }); }
function failed(key: QueryKey, message = "OFFLINE_FIXTURE_FAILURE") { client.getQueryCache().build(client, { queryKey: key }).setState({ status: "error", fetchStatus: "idle", error: new Error(message) }); }
function assertAlert(markup: string, text = "OFFLINE_FIXTURE_FAILURE") { assert.match(markup, /role="alert"/); assert.ok(markup.includes(text)); }

test("shared loading and both error layouts expose exactly one live region without changing their structure", () => {
  const loading = render(<LoadingPanel title="Offline loading" hint="fixture" />);
  assert.equal((loading.match(/role="status"/g) ?? []).length, 1);
  assert.match(loading, /class="loading-spinner" aria-hidden="true"/);
  for (const showIcon of [false, true]) {
    const markup = render(<ErrorPanel title="Offline error" message="fixture error" showIcon={showIcon} onRetry={() => assert.fail("render cannot retry")} />);
    assert.equal((markup.match(/role="alert"/g) ?? []).length, 1);
    assert.equal((markup.match(/>重试<\/button>/g) ?? []).length, 1);
    assert.equal(markup.includes("error-panel-icon"), showIcon);
    assert.doesNotMatch(markup, /aria-live=|role="status"/);
  }
});

test("memory query failure cannot simultaneously claim that the memory library is empty", () => {
  failed(["agent-memories", "global", null, ""]);
  const markup = render(<MemoryLibrary onClose={() => {}} />);
  assertAlert(markup);
  assert.doesNotMatch(markup, /暂无记忆|读取中/);
});
test("memory initial loading, successful empty state, and retained rows are distinct", () => {
  const key = ["agent-memories", "global", null, ""];
  pending(key);
  let markup = render(<MemoryLibrary onClose={() => {}} />);
  assert.match(markup, /role="status">读取中/); assert.doesNotMatch(markup, /暂无记忆/);
  client.setQueryData(key, []);
  markup = render(<MemoryLibrary onClose={() => {}} />);
  assert.match(markup, /暂无记忆/); assert.doesNotMatch(markup, /role="alert"/);
  client.setQueryData(key, [{ id: "memory_fixture", scope: "global", title: "Retained user memory", content: "User-entered body", createdAt: "2026-10-18T00:00:00Z", updatedAt: "2026-10-18T00:00:00Z" }]);
  failed(key);
  markup = render(<MemoryLibrary onClose={() => {}} />);
  assertAlert(markup); assert.match(markup, /Retained user memory/); assert.doesNotMatch(markup, /暂无记忆/);
});
test("memory context reset remains disabled while the current agent run is busy", () => {
  workspace({ agentBusy: true });
  client.setQueryData(["agent-memories", "global", null, ""], []);
  const markup = render(<MemoryLibrary onClose={() => {}} />);
  assert.match(markup, /<button[^>]*disabled=""[^>]*>重置全部上下文<\/button>/);
});

test("hidden overview activity observers make no request, activation fetches once, hiding stops invalidation fetches", async () => {
  const observed: Array<{ command: string; params: Record<string, unknown> }> = [];
  installHost({ invoke: async (command, params) => { observed.push({ command, params }); assert.equal(command, IPC_COMMANDS.activityList); return { activities: activity() }; } });
  client.removeQueries({ queryKey: ["activities", serverId] });
  const observer = new QueryObserver(client, recentServerActivityQueryOptions(serverId, false));
  const unsubscribe = observer.subscribe(() => {});
  try {
    await client.invalidateQueries({ queryKey: ["activities", serverId] });
    assert.equal(observed.length, 0);
    observer.setOptions(recentServerActivityQueryOptions(serverId, true));
    for (let i = 0; i < 10 && !observer.getCurrentResult().isSuccess; i++) await new Promise<void>((resolve) => setImmediate(resolve));
    assert.equal(observer.getCurrentResult().isSuccess, true);
    assert.equal(observed.length, 1);
    assert.deepEqual(observed[0]?.params, { serverId, limit: 50 });
    observer.setOptions(recentServerActivityQueryOptions(serverId, false));
    await client.invalidateQueries({ queryKey: ["activities", serverId] });
    assert.equal(observed.length, 1);
    assert.deepEqual(observer.getCurrentResult().data, activity());
  } finally { unsubscribe(); observer.destroy(); }
});
test("the overview freshness clock creates no hidden timer, refreshes on reveal and clears its visible timer", () => {
  const intervals = new Map<number, () => void>();
  const values: number[] = [];
  let next = 0;
  Object.assign(window, {
    setInterval: (tick: () => void, ms: number) => { assert.equal(ms, 10_000); intervals.set(++next, tick); return next; },
    clearInterval: (id: number) => { intervals.delete(id); },
  });
  const stopHidden = startSnapshotDisplayClock(false, (value) => values.push(value));
  assert.equal(intervals.size, 0); assert.equal(values.length, 0); stopHidden();
  const stopVisible = startSnapshotDisplayClock(true, (value) => values.push(value));
  assert.equal(values.length, 1); assert.equal(intervals.size, 1);
  intervals.values().next().value!(); assert.equal(values.length, 2);
  stopVisible(); assert.equal(intervals.size, 0);
  startSnapshotDisplayClock(false, (value) => values.push(value))(); assert.equal(values.length, 2);
});

function DiagnosticLogs({ open = true }: { open?: boolean }) { const logs = useAgentLogs(open); return <RuntimeLogs open={open} logs={logs} />; }
for (const state of ["loading", "error", "empty", "content"] as const) test(`diagnostic log ${state} state keeps one aria-controls target`, () => {
  if (state === "loading") pending(RUNTIME_QUERY_KEYS.logs);
  else if (state === "error") failed(RUNTIME_QUERY_KEYS.logs);
  else client.setQueryData(RUNTIME_QUERY_KEYS.logs, { lines: state === "content" ? ["first", "second"] : [] });
  const markup = render(<DiagnosticLogs />);
  assert.equal((markup.match(/id="runtime-logs"/g) ?? []).length, 1);
  if (state === "error") { assertAlert(markup); assert.match(markup, /重试/); }
  else if (state === "loading") assert.match(markup, /正在读取日志/);
  else if (state === "empty") assert.match(markup, /暂无捕获输出/);
  else assert.match(markup, /first\nsecond/);
});
test("closed diagnostic output is absent and initial settings mount only the preference editor", () => {
  assert.equal(render(<DiagnosticLogs open={false} />), "");
  const markup = render(<RuntimeSettings />);
  const ids = new Set([...markup.matchAll(/\bid="([^"]+)"/g)].map((match) => match[1]));
  const tabs = [...markup.matchAll(/role="tab"[^>]*aria-controls="([^"]+)"/g)];
  assert.equal(tabs.length, 5);
  for (const tab of tabs) assert.ok(ids.has(tab[1]));
  assert.equal((markup.match(/role="tabpanel"/g) ?? []).length, 5);
  assert.equal(client.getQueryCache().find({ queryKey: RUNTIME_QUERY_KEYS.status }), undefined);
  assert.equal(client.getQueryCache().find({ queryKey: RUNTIME_QUERY_KEYS.ping }), undefined);
});

for (const page of ["logs", "services"] as const) {
  const key = page === "logs" ? ["serverLogs", serverId] : ["serverServices", serverId];
  const node = () => page === "logs" ? <LogsPane serverId={serverId} /> : <ServicesPane serverId={serverId} />;
  test(`${page} page distinguishes loading from failure without manufacturing rows`, () => {
    pending(key); let markup = render(node()); assert.match(markup, /role="status"/); assert.doesNotMatch(markup, /class="(?:log|service)-row"/);
    failed(key); markup = render(node()); assertAlert(markup); assert.match(markup, /重试/);
  });
  test(`${page} cache stays visible on refresh failure and the first page has a fixed row budget`, () => {
    if (page === "logs") client.setQueryData(key, { source: "journalctl", lines: Array.from({ length: 100 }, (_, i) => ({ text: `offline-log-${i}`, level: "info" })) });
    else client.setQueryData(key, { source: "systemd", services: Array.from({ length: 200 }, (_, i) => ({ name: `offline-${i}.service`, state: i ? "running" : "failed", status: "synthetic status", description: "fixture" })) });
    failed(key); const markup = render(node());
    assert.match(markup, /刷新失败，保留上次结果/);
    assert.equal((markup.match(page === "logs" ? /class="log-row"/g : /class="service-row"/g) ?? []).length, page === "logs" ? 30 : 20);
    assert.match(markup, /下一页/);
    assert.ok(!markup.includes(page === "logs" ? "offline-log-99" : "offline-199.service"));
  });
  test(`${page} inactive pane renders no visible content and no query request`, () => {
    const markup = render(page === "logs" ? <LogsPane serverId={serverId} active={false} /> : <ServicesPane serverId={serverId} active={false} />);
    assert.equal(markup, "");
  });
}
test("activity page separates loading, failure, successful empty and cached target-specific events", () => {
  const key = ["activities", serverId];
  pending(key); let markup = render(<ActivityFeed serverId={serverId} />); assert.match(markup, /正在读取动态/); assert.doesNotMatch(markup, /暂无动态/);
  failed(key); markup = render(<ActivityFeed serverId={serverId} />); assertAlert(markup); assert.doesNotMatch(markup, /暂无动态/);
  client.setQueryData(key, []); markup = render(<ActivityFeed serverId={serverId} />); assert.match(markup, /暂无动态/);
  const rows = activity(); client.setQueryData(key, rows); failed(key);
  markup = render(<ActivityFeed serverId={serverId} />); assertAlert(markup); assert.match(markup, /activity-timeline/); assert.ok(markup.includes(rows[0]!.title));
  client.setQueryData(["activities", "srv_previewapi"], []);
  const switched = render(<ActivityFeed serverId="srv_previewapi" />); assert.match(switched, /暂无动态/); assert.ok(!switched.includes(rows[0]!.title));
});

test("remote file list distinguishes unknown, empty and failed reads, retaining previous directory rows", () => {
  const key = ["remote-files", serverId, "/"];
  pending(key); let markup = render(<RemoteFilesPane serverId={serverId} />); assert.match(markup, /aria-label="加载文件列表"/); assert.doesNotMatch(markup, /空目录/);
  failed(key); markup = render(<RemoteFilesPane serverId={serverId} />); assertAlert(markup); assert.doesNotMatch(markup, /空目录/);
  client.setQueryData(key, { path: "/", entries: [] }); markup = render(<RemoteFilesPane serverId={serverId} />); assert.match(markup, /空目录/);
  const data = remoteFiles(); client.setQueryData(key, data); failed(key);
  markup = render(<RemoteFilesPane serverId={serverId} />); assertAlert(markup); assert.ok(markup.includes(data.entries[0]!.name));
});
test("transfer cards keep cancellation limited to active states and unknown totals remain indeterminate", () => {
  const base = transferFixtures()[0]!;
  const states: TransferSnapshot["status"][] = ["queued", "running", "waitingConflict", "completed", "partial", "failed", "cancelled", "interrupted"];
  client.setQueryData(["remote-files", serverId, "/"], { path: "/", entries: [] });
  client.setQueryData(["file-transfers", serverId], states.map((status, i) => ({ ...base, transferId: `fixture_${i}`, status, currentItem: undefined, activeConflict: undefined, totalBytes: null, totalFiles: null, transferredBytes: 12, failures: status === "failed" ? [{ item: "fixture", message: "staged failure", stagingResidue: "/tmp/.owned.part" }] : [] })));
  const markup = render(<RemoteFilesPane serverId={serverId} />);
  assert.equal((markup.match(/class="transfer-card"/g) ?? []).length, 8);
  assert.equal((markup.match(/>取消<\/button>/g) ?? []).length, 3);
  assert.equal((markup.match(/<progress /g) ?? []).length, 4);
  const progress = markup.match(/<progress[^>]*>/g) ?? [];
  assert.equal(progress.filter((tag) => !/\bvalue=/.test(tag)).length, 3);
  assert.equal(progress.filter((tag) => /value="100"/.test(tag)).length, 1);
  assert.match(markup, /暂存残留：\/tmp\/\.owned\.part/);
  for (const label of ["已完成", "部分完成", "失败", "已取消", "已中断"]) assert.ok(markup.includes(label));
});
test("file conflicts expose only host-authorized choices and keep blank rename disabled", () => {
  const conflict = transferFixtures().find((transfer) => transfer.activeConflict);
  assert.ok(conflict?.activeConflict);
  client.setQueryData(["remote-files", serverId, "/"], { path: "/", entries: [] });
  client.setQueryData(["file-transfers", serverId], [{ ...conflict, activeConflict: { ...conflict.activeConflict, allowedActions: ["skip", "rename"] } }]);
  const markup = render(<RemoteFilesPane serverId={serverId} />);
  assert.match(markup, /跳过/); assert.match(markup, /aria-label="重命名目标"/);
  assert.match(markup, /<button[^>]*disabled=""[^>]*>保留两份<\/button>/);
  assert.doesNotMatch(markup, />覆盖<\/button>/);
});

test("server overview keeps selection, connection, loading and mismatched-target data separate", () => {
  assert.match(render(<ServerOverview />), /选择一台服务器/);
  workspace({ selectedServerId: serverId });
  client.setQueryData(["servers"], servers.map((server) => server.id === serverId ? { ...server, status: "disconnected" } : server));
  assert.match(render(<ServerOverview />), /尚未连接/);
  client.setQueryData(["servers"], servers.map((server) => server.id === serverId ? { ...server, status: "connecting" } : server));
  assert.match(render(<ServerOverview />), /disabled=""[^>]*>[^<]*(?:<[^>]*>[^<]*<\/[^>]*>)*正在连接|正在连接…/);
  client.setQueryData(["servers"], servers.map((server) => server.id === serverId ? { ...server, status: "connected" } : server));
  pending(["serverSnapshot", serverId]); assert.match(render(<ServerOverview />), /已连接，正在检测/);
  client.setQueryData(["serverSnapshot", serverId], { ...snapshot(), serverId: "srv_foreign" });
  const mismatch = render(<ServerOverview />); assertAlert(mismatch, "收到的快照属于另一台服务器"); assert.doesNotMatch(mismatch, /overview-metric-grid/);
});
test("overview refresh failure preserves marked cached data and displays collector failures honestly", () => {
  workspace({ selectedServerId: serverId });
  client.setQueryData(["servers"], servers.map((server) => server.id === serverId ? { ...server, status: "connected" } : server));
  const data = snapshot();
  client.setQueryData(["serverSnapshot", serverId], { ...data, collectors: data.collectors?.map((collector, i) => i ? collector : { ...collector, ok: false, error: "collector fixture failure" }) });
  failed(["serverSnapshot", serverId]);
  const markup = render(<ServerOverview active={false} />);
  assert.match(markup, /role="status"/); assert.match(markup, /OFFLINE_FIXTURE_FAILURE/);
  assert.match(markup, /资源更新失败，正在保留上次采集结果/); assert.match(markup, /<details><summary>查看错误详情/);
  assert.match(markup, /collector fixture failure/); assert.match(markup, /1 项失败/);
});
test("host-key loading and errors fail closed; only an actual pinned status renders remote children", () => {
  let rendered = 0;
  function RemoteChild() { rendered += 1; return <p>REMOTE_CHILD_FIXTURE</p>; }
  const node = <HostKeyGate serverId={serverId}><RemoteChild /></HostKeyGate>;
  const key = ["host-key", serverId];
  pending(key); assert.match(render(node), /正在读取主机指纹/); assert.equal(rendered, 0);
  failed(key); assertAlert(render(node)); assert.equal(rendered, 0);
  client.setQueryData(key, { pinned: false }); assert.match(render(node), /需要先核验/); assert.equal(rendered, 0);
  client.setQueryData(key, { pinned: true }); assert.match(render(node), /REMOTE_CHILD_FIXTURE/); assert.equal(rendered, 1);
});

test("task board preserves lanes but does not call unsynchronized data an empty task list", () => {
  pending(["team-board-tasks"]); let markup = render(<TeamBoardPane serverId={null} />); assert.match(markup, /读取任务/); assert.match(markup, /team-board-columns/); assert.doesNotMatch(markup, /暂无任务/);
  failed(["team-board-tasks"]); markup = render(<TeamBoardPane serverId={null} />); assertAlert(markup); assert.match(markup, /任务状态未同步/); assert.doesNotMatch(markup, /暂无任务/);
  client.setQueryData(["team-board-tasks"], []); markup = render(<TeamBoardPane serverId={null} />); assert.match(markup, /暂无任务/);
  const detail = IPC_SCHEMAS.investigation_task_get.response.parse(previewResponse(IPC_COMMANDS.investigationTaskGet, { taskId: "task_preview" }));
  client.setQueryData(["team-board-tasks"], [detail.task]); markup = render(<TeamBoardPane serverId={null} />); assert.match(markup, /查看任务属性：/); assert.ok(markup.includes(detail.task.objective));
});
test("goal-task loading and read failures do not replace the state with a successful empty result", () => {
  pending(["investigationTasks"]); let markup = render(<InvestigationsPane />); assert.match(markup, /role="status"/);
  failed(["investigationTasks"]); markup = render(<InvestigationsPane />); assertAlert(markup); assert.match(markup, /重试/);
});
test("goal-task detail renders persisted evidence, planned steps and schedule records without executing them", () => {
  const detail = IPC_SCHEMAS.investigation_task_get.response.parse(previewResponse(IPC_COMMANDS.investigationTaskGet, { taskId: "task_preview" }));
  const schedules = IPC_SCHEMAS.investigation_schedule_list.response.parse(previewResponse(IPC_COMMANDS.investigationScheduleList, {}));
  workspace({ selectedTaskId: detail.task.id });
  client.setQueryData(["investigationTasks"], { tasks: [detail.task] });
  client.setQueryData(["investigationTask", detail.task.id], detail);
  client.setQueryData(["investigationSchedules"], schedules);
  const selectedSchedule = schedules.schedules.find((schedule) => schedule.taskId === detail.task.id)!;
  client.setQueryData(["investigationScheduleRuns", selectedSchedule.id], IPC_SCHEMAS.investigation_schedule_runs.response.parse(previewResponse(IPC_COMMANDS.investigationScheduleRuns, { scheduleId: selectedSchedule.id })));
  const markup = render(<InvestigationsPane />);
  assert.ok(markup.includes(detail.task.objective)); assert.match(markup, /证据|取证/); assert.match(markup, /计划/);
  for (const finding of detail.findings) assert.ok(markup.includes(finding.title));
});
test("onboarding displays query errors and never treats configured models as already tested", () => {
  pending(["providers"]); let markup = render(<GettingStarted onClose={() => {}} />); assert.match(markup, /正在读取已有配置/);
  failed(["providers"]); markup = render(<GettingStarted onClose={() => {}} />); assertAlert(markup, "无法读取已有配置");
  client.setQueryData(["providers"], IPC_SCHEMAS.provider_list.response.parse(previewResponse(IPC_COMMANDS.providerList, {})).providers);
  client.setQueryData(RUNTIME_QUERY_KEYS.status, IPC_SCHEMAS.agent_status.response.parse(previewResponse(IPC_COMMANDS.agentStatus, {})));
  markup = render(<GettingStarted onClose={() => {}} />); assert.match(markup, /测试模型连接/); assert.doesNotMatch(markup, />测试成功</);
});

test("workpage HTML remains inert before an explicit Run action and text is escaped", () => {
  const markup = render(<WorkpageArtifactView artifact={{ id: "html_fixture", title: "Offline HTML", content: { format: "html", html: "<script>throw 'must not execute'</script><h1>fixture</h1>" } }} />);
  assert.doesNotMatch(markup, /<iframe|<script>|<h1>/); assert.match(markup, /&lt;script&gt;/); assert.match(markup, />运行<\/button>/);
});
test("workpage charts and flows preserve accessible labels and never inject label markup", (context) => {
  const warnings: unknown[][] = [];
  context.mock.method(console, "error", (...args: unknown[]) => { warnings.push(args); });
  const chart = render(<WorkpageArtifactView artifact={{ id: "chart_fixture", title: "Bounded chart", content: { format: "chart", series: [{ label: "<unsafe>", value: -5 }, { label: "positive", value: 10 }], unit: "%" } }} />);
  assert.match(chart, /role="img" aria-label="Bounded chart"/); assert.match(chart, /<title>&lt;unsafe&gt;: -5%<\/title>/); assert.match(chart, /<title>positive: 10%<\/title>/); assert.doesNotMatch(chart, /NaN|Infinity|<unsafe>/);
  assert.deepEqual(warnings, [], "React 19 must receive a single title string instead of a children array");
  const flow = render(<WorkpageArtifactView artifact={{ id: "flow_fixture", title: "Flow fixture", content: { format: "flow", nodes: [{ id: "a", label: "<entry>" }, { id: "b", label: "done" }], edges: [{ from: "a", to: "b", label: "next" }] } }} />);
  assert.match(flow, /role="img" aria-label="Flow fixture"/); assert.match(flow, /&lt;entry&gt;/); assert.doesNotMatch(flow, /<entry>/);
});

function feed(entries: Entry[], running = false, state: string | null = null) {
  return render(<AgentFeed entries={entries} running={running} runState={state} lastUserEntry={entries.findLast((entry): entry is Extract<Entry, { kind: "user" }> => entry.kind === "user")} canSend={!running}
    onRetry={() => assert.fail("render cannot retry")} onApproval={async () => { assert.fail("render cannot approve"); }} pendingApprovalIds={[]} approvalStatuses={{}}
    feedRef={createRef<HTMLDivElement>()} frameRef={createRef<HTMLDivElement>()} onScroll={() => {}} showJumpControl={false} onJumpToLatest={() => {}} motionScope="offline_session" />);
}
test("conversation empty, running, failed and cancelled states expose the corresponding retry affordance only", () => {
  assert.match(feed([]), /新任务/);
  const entries = appendEntries([], [{ kind: "user", text: "User fixture" }, { kind: "assistant", text: "Partial answer\n" }]);
  assert.doesNotMatch(feed(entries, true, "thinking"), /重试上一条/);
  assert.match(feed(entries, false, "failed"), /重试上一条/);
  assert.doesNotMatch(feed(entries, false, "cancelled"), /重试上一条/);
  assert.match(feed(entries, false, "cancelled"), /Partial answer/);
});
test("long conversation clipping keeps the newest content without fabricated loading or completion rows", () => {
  const entries = appendEntries([], Array.from({ length: 620 }, (_, index) => ({ kind: "system", text: `ENTRY_${String(index).padStart(4, "0")}_END` })));
  const markup = feed(entries);
  assert.equal(entries.length, 500); assert.doesNotMatch(markup, /ENTRY_0000_END|ENTRY_0119_END/); assert.match(markup, /ENTRY_0120_END/); assert.match(markup, /ENTRY_0619_END/);
  assert.doesNotMatch(markup, /重试上一条|正在提交|运行已完成/);
});
test("running conversations disable history switching and workspace target guards preserve drafts and model overrides", () => {
  const markup = render(<AgentHeader running runState="waiting_approval" tokenUsage={null} agentReady agentOpen shell historyOpen={false} historyButtonRef={createRef()} onToggleHistory={() => {}} onToggle={() => {}} closeButtonRef={createRef()} />);
  assert.match(markup, /aria-label="切换任务：[^"]*"[^>]*aria-disabled="true"/); assert.match(markup, /等待审批/);
  workspace({ selectedServerId: serverId, serverPage: "files", agentBusy: true, agentDraft: "unsubmitted user draft", selectedProviderId: "fixture_provider", selectedModel: "user-model-override" });
  useWorkspaceStore.getState().selectServer("srv_foreign");
  useWorkspaceStore.getState().syncServerSelection(["srv_foreign"]);
  assert.equal(useWorkspaceStore.getState().selectedServerId, serverId);
  assert.equal(useWorkspaceStore.getState().serverPage, "files");
  assert.equal(useWorkspaceStore.getState().agentDraft, "unsubmitted user draft");
  assert.equal(useWorkspaceStore.getState().selectedModel, "user-model-override");
});
