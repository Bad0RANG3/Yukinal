import { useEffect } from "react";
import type { InvestigationScheduleActivityEvent } from "@yukinal/shared";
import { isDesktopShell, subscribeDesktop } from "../../lib/ipc.js";
import { teamRunActive, useTeamRuntimeStore } from "./team-runtime-store.js";

/** This subscriber deliberately has no access to chat, drafts, focus or navigation. */
export function receiveWatchActivity(event: InvestigationScheduleActivityEvent): void {
  if (event.sessionId !== `watch:${event.runId}`) return;
  const runtime = useTeamRuntimeStore.getState();
  const previous = runtime.runs.find((run) => run.id === event.runId);
  if (previous && (previous.source !== "watch" || previous.serverId !== event.serverId || previous.taskId !== event.taskId || previous.scheduleId !== event.scheduleId || !teamRunActive(previous))) return;
  if (!previous) runtime.begin({ id: event.runId, team: "blue", source: "watch", serverId: event.serverId,
    sessionId: event.sessionId, taskId: event.taskId, scheduleId: event.scheduleId, goal: event.goal });
  if (event.usage) runtime.usage(event.runId, event.usage);
  if (event.tool) runtime.tool(event.runId, event.tool.name, event.serverId, event.tool.finished, event.tool.callId);
  runtime.stage(event.runId, event.stage);
  if (["completed", "failed", "interrupted"].includes(event.stage)) runtime.noteWatchUpdate({
    scheduleId: event.scheduleId, runId: event.runId, serverId: event.serverId, at: event.at,
  });
}

export function useWatchActivity(sidecarRunning: boolean | undefined): void {
  useEffect(() => {
    if (!isDesktopShell()) return;
    const subscription = subscribeDesktop("investigation.schedule_activity", receiveWatchActivity);
    void subscription.ready.catch(() => {});
    return () => subscription.stop();
  }, []);
  useEffect(() => {
    if (sidecarRunning !== false) return;
    const runtime = useTeamRuntimeStore.getState();
    for (const run of runtime.runs.filter((run) => run.source === "watch" && teamRunActive(run))) runtime.stage(run.id, "interrupted");
  }, [sidecarRunning]);
}
