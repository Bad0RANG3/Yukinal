import { useCallback, useEffect, useRef, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { IPC_COMMANDS, type ApprovalRequest, type ApprovalDecision } from "@yukinal/shared";
import { callDesktop, isDesktopShell, subscribeDesktop } from "../../lib/ipc.js";
import { errorMessage } from "../../lib/format.js";
import { useTeamRuntimeStore } from "../agent-nist/team-runtime-store.js";
import { teamTaskActive, useCoordinatorStore } from "./coordinator-state.js";

/** One application subscriber, independent of whichever conversation is visible. */
export function useCoordinatorTasks(sidecarRunning: boolean | undefined, onAttention: () => void) {
  const attention = useRef(onAttention);
  attention.current = onAttention;
  const tasks = useCoordinatorStore((state) => state.tasks);
  const [error, setError] = useState<string | null>(null);
  const [pending, setPending] = useState<string[]>([]);
  const [decisions, setDecisions] = useState<Record<string, string>>({});
  const locks = useRef(new Set<string>());
  const snapshot = useQuery({ queryKey: ["agent-team-tasks"], enabled: isDesktopShell() && sidecarRunning === true,
    staleTime: 25_000, retry: false, queryFn: () => callDesktop(IPC_COMMANDS.agentTeamTasks, {}),
  });
  const sync = useRef({ running: sidecarRunning, refetch: snapshot.refetch });
  sync.current = { running: sidecarRunning, refetch: snapshot.refetch };
  useEffect(() => {
    for (const task of snapshot.data?.tasks ?? []) useCoordinatorStore.getState().receive(task);
  }, [snapshot.data]);
  useEffect(() => {
    if (!isDesktopShell()) return;
    let active = true;
    const subscription = subscribeDesktop("agent.team_task", (event) => {
      const previous = useCoordinatorStore.getState().tasks.find((item) => item.id === event.task.id);
      useCoordinatorStore.getState().receive(event.task);
      if (event.task.approval && previous?.approval?.approvalId !== event.task.approval.approvalId) attention.current();
    });
    // Worker events retain run/call identities for existing breathing outlines.
    const call = subscribeDesktop("agent.tool_call", (event) => {
      if (!useCoordinatorStore.getState().tasks.some((task) => task.runId === event.runId && teamTaskActive(task))) return;
      const runtime = useTeamRuntimeStore.getState();
      runtime.stage(event.runId, "running_tool");
      runtime.tool(event.runId, event.toolName, event.target.serverId ?? null, false, event.callId);
    });
    const result = subscribeDesktop("agent.tool_result", (event) => {
      if (!useCoordinatorStore.getState().tasks.some((task) => task.runId === event.runId && teamTaskActive(task))) return;
      const runtime = useTeamRuntimeStore.getState();
      runtime.tool(event.runId, event.toolName, event.target.serverId ?? null, true, event.callId);
      runtime.stage(event.runId, "thinking");
    });
    void Promise.all([subscription.ready, call.ready, result.ready]).then(() => {
      // Close the gap between the first snapshot and the event subscription.
      if (active && sync.current.running === true) void sync.current.refetch({ cancelRefetch: false });
    }).catch((cause: unknown) => { if (active) setError(`后台事件连接失败：${errorMessage(cause)}`); });
    return () => { active = false; subscription.stop(); call.stop(); result.stop(); };
  }, []);
  useEffect(() => {
    if (sidecarRunning !== false) return;
    for (const task of useCoordinatorStore.getState().tasks.filter(teamTaskActive)) useTeamRuntimeStore.getState().stage(task.runId, "interrupted");
  }, [sidecarRunning]);

  const stop = useCallback(async (runId: string) => {
    const task = useCoordinatorStore.getState().tasks.find((item) => item.runId === runId);
    if (!task || !teamTaskActive(task) || locks.current.has(runId)) return;
    locks.current.add(runId); setPending((values) => [...values, runId]); setError(null);
    try {
      const response = await callDesktop(IPC_COMMANDS.agentRunStop, { runId });
      if (!response.stopped) await snapshot.refetch();
    } catch (cause) { setError(`停止失败：${errorMessage(cause)}`); }
    finally { locks.current.delete(runId); setPending((values) => values.filter((value) => value !== runId)); }
  }, [snapshot.refetch]);
  const approve = useCallback(async (approval: ApprovalRequest, decision: ApprovalDecision) => {
    const task = useCoordinatorStore.getState().tasks.find((item) => item.runId === approval.runId);
    if (!task || task.state !== "waiting_approval" || task.approval?.approvalId !== approval.approvalId || locks.current.has(approval.approvalId) || decision === "approve_session") return;
    if (Date.parse(approval.expiresAt) <= Date.now()) { setDecisions((values) => ({ ...values, [approval.approvalId]: "已过期" })); return; }
    locks.current.add(approval.approvalId); setPending((values) => [...values, approval.approvalId]); setError(null);
    try {
      const response = await callDesktop(IPC_COMMANDS.agentApprovalRespond, { runId: approval.runId, approvalId: approval.approvalId, decision, respondedAt: new Date().toISOString() });
      setDecisions((values) => ({ ...values, [approval.approvalId]: response.accepted ? decision === "reject" ? "已拒绝" : "已批准一次" : "批准已失效" }));
    } catch (cause) { setError(`批准失败：${errorMessage(cause)}`); locks.current.delete(approval.approvalId); }
    finally { setPending((values) => values.filter((value) => value !== approval.approvalId)); }
  }, []);
  return { tasks, stop, approve, pending, decisions, error: error ?? (snapshot.isError ? `后台任务状态读取失败：${errorMessage(snapshot.error)}` : null), refresh: snapshot.refetch, available: sidecarRunning === true };
}
