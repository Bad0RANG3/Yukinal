import assert from "node:assert/strict";
import { afterEach, test } from "node:test";
import { renderToStaticMarkup } from "react-dom/server";
import { EVENT_SCHEMAS, type InvestigationScheduleActivityEvent } from "@yukinal/shared";
import { receiveWatchActivity } from "../src/features/agent-nist/watch-activity.js";
import { useTeamRuntimeStore } from "../src/features/agent-nist/team-runtime-store.js";
import { TeamActionCueView } from "../src/features/agent-nist/TeamActivity.js";
import { useWorkspaceStore } from "../src/stores/workspace-store.js";
import { watchResult } from "../src/features/agent-nist/TeamWatchPane.js";

afterEach(() => {
  useTeamRuntimeStore.setState(useTeamRuntimeStore.getInitialState(), true);
  useWorkspaceStore.setState(useWorkspaceStore.getInitialState(), true);
});
const base: InvestigationScheduleActivityEvent = { scheduleId: "schedule_a", runId: "watch_a", taskId: "task_a",
  sessionId: "watch:watch_a", serverId: "srv_a", goal: "检查资源与服务", stage: "thinking", at: "2026-10-20T09:00:00Z" };
test("watch operations use real scoped cues and never touch foreground composition or navigation", () => {
  useWorkspaceStore.setState({ primary: "activity", agentDraft: "主对话未发送草稿", agentBusy: true, selectedServerId: "srv_b" });
  const foreground = useWorkspaceStore.getState();
  const runtime = useTeamRuntimeStore.getState();
  runtime.begin({ id: "main_chat", team: "yellow", sessionId: "main", serverId: "srv_b", goal: "独立工作" });
  receiveWatchActivity(EVENT_SCHEMAS["investigation.schedule_activity"].parse({ ...base, stage: "running_tool", tool: { callId: "logs", name: "server.logs", finished: false } }));
  assert.equal(useWorkspaceStore.getState(), foreground);
  assert.equal(useTeamRuntimeStore.getState().runs.find((run) => run.id === "main_chat")?.stage, "starting");
  assert.match(renderToStaticMarkup(<TeamActionCueView cues={useTeamRuntimeStore.getState().cues} serverId="srv_a" surface="logs" />), /运维 · 操作中 · server.logs/);
  assert.equal(renderToStaticMarkup(<TeamActionCueView cues={useTeamRuntimeStore.getState().cues} serverId="srv_b" surface="logs" />), "");
  receiveWatchActivity({ ...base, stage: "thinking", tool: { callId: "logs", name: "server.logs", finished: true } });
  receiveWatchActivity({ ...base, stage: "completed" });
  assert.match(renderToStaticMarkup(<TeamActionCueView cues={useTeamRuntimeStore.getState().cues} serverId="srv_a" surface="logs" />), /工具已返回/);
  assert.deepEqual(useTeamRuntimeStore.getState().watchUpdates.map((update) => update.runId), ["watch_a"]);
  assert.equal(useWorkspaceStore.getState(), foreground);
});
test("late events and foreign bindings cannot revive a finished watch or clear an unread update", () => {
  receiveWatchActivity({ ...base, stage: "completed" });
  const completed = useTeamRuntimeStore.getState().runs[0];
  receiveWatchActivity({ ...base, stage: "running_tool", serverId: "srv_b", tool: { callId: "late", name: "server.logs", finished: false } });
  receiveWatchActivity({ ...base, stage: "thinking" });
  assert.equal(useTeamRuntimeStore.getState().runs[0], completed);
  assert.equal(useTeamRuntimeStore.getState().cues.length, 0);
  assert.equal(useTeamRuntimeStore.getState().watchUpdates.length, 1);
  useTeamRuntimeStore.getState().readWatchUpdates(["schedule_other"]);
  assert.equal(useTeamRuntimeStore.getState().watchUpdates.length, 1);
  useTeamRuntimeStore.getState().readWatchUpdates(["schedule_a"]);
  assert.equal(useTeamRuntimeStore.getState().watchUpdates.length, 0);
});
test("watch reports show the human summary and reject chat-shaped activity", () => {
  assert.equal(watchResult(undefined, JSON.stringify({ outcome: "no_change", summary: "服务正常，未发现新增异常。" })), "服务正常，未发现新增异常。");
  assert.equal(EVENT_SCHEMAS["investigation.schedule_activity"].safeParse({ ...base, text: "Do not insert chat text" }).success, false);
  receiveWatchActivity({ ...base, sessionId: "main_conversation" });
  assert.equal(useTeamRuntimeStore.getState().runs.length, 0);
});
