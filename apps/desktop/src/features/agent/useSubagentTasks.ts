import { useCallback, useEffect, useRef, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { IPC_COMMANDS, type ApprovalRequest, type ApprovalDecision } from "@yukinal/shared";
import { callDesktop, isDesktopShell, subscribeDesktop } from "../../lib/ipc.js";
import { errorMessage } from "../../lib/format.js";
import { subagentTaskActive, useSubagentStore } from "./subagent-state.js";

/** One subscriber keeps subagent status, stop and approval actions available across chats. */
export function useSubagentTasks(sidecarRunning: boolean | undefined, onAttention: () => void) {
  const attention = useRef(onAttention);
  attention.current = onAttention;
  const tasks = useSubagentStore((state) => state.tasks);
  const [error, setError] = useState<string | null>(null);
  const [pending, setPending] = useState<string[]>([]);
  const [decisions, setDecisions] = useState<Record<string, string>>({});
  const locks = useRef(new Set<string>());
  const snapshot = useQuery({ queryKey: ["agent-subagent-tasks"], enabled: isDesktopShell() && sidecarRunning === true,
    staleTime: 2_000, refetchInterval: 5_000, retry: false, queryFn: () => callDesktop(IPC_COMMANDS.agentSubagentTasks, {}),
  });
  const sync = useRef({ running: sidecarRunning, refetch: snapshot.refetch });
  sync.current = { running: sidecarRunning, refetch: snapshot.refetch };
  useEffect(() => {
    for (const task of snapshot.data?.tasks ?? []) useSubagentStore.getState().receive(task);
  }, [snapshot.data]);
  useEffect(() => {
    if (!isDesktopShell()) return;
    let active = true;
    const subscription = subscribeDesktop("agent.subagent_task", (event) => {
      const previous = useSubagentStore.getState().tasks.find((item) => item.id === event.task.id);
      useSubagentStore.getState().receive(event.task);
      if (event.task.approval && previous?.approval?.approvalId !== event.task.approval.approvalId) attention.current();
    });
    void subscription.ready.then(() => {
      if (active && sync.current.running === true) void sync.current.refetch({ cancelRefetch: false });
    }).catch((cause: unknown) => { if (active) setError(`subagent 状态连接失败：${errorMessage(cause)}`); });
    return () => { active = false; subscription.stop(); };
  }, []);
  const stop = useCallback(async (runId: string) => {
    const task = useSubagentStore.getState().tasks.find((item) => item.runId === runId);
    if (!task || !subagentTaskActive(task) || locks.current.has(runId)) return;
    locks.current.add(runId); setPending((values) => [...values, runId]); setError(null);
    try {
      const response = await callDesktop(IPC_COMMANDS.agentRunStop, { runId });
      if (!response.stopped) await snapshot.refetch();
    } catch (cause) { setError(`停止 subagent 失败：${errorMessage(cause)}`); }
    finally { locks.current.delete(runId); setPending((values) => values.filter((value) => value !== runId)); }
  }, [snapshot.refetch]);
  const approve = useCallback(async (approval: ApprovalRequest, decision: ApprovalDecision) => {
    const task = useSubagentStore.getState().tasks.find((item) => item.runId === approval.runId);
    if (!task || task.state !== "waiting_approval" || task.approval?.approvalId !== approval.approvalId || locks.current.has(approval.approvalId) || decision === "approve_session") return;
    if (Date.parse(approval.expiresAt) <= Date.now()) { setDecisions((values) => ({ ...values, [approval.approvalId]: "已过期" })); return; }
    locks.current.add(approval.approvalId); setPending((values) => [...values, approval.approvalId]); setError(null);
    try {
      const response = await callDesktop(IPC_COMMANDS.agentApprovalRespond, { runId: approval.runId, approvalId: approval.approvalId, decision, respondedAt: new Date().toISOString() });
      setDecisions((values) => ({ ...values, [approval.approvalId]: response.accepted ? decision === "reject" ? "已拒绝" : "已批准一次" : "批准已失效" }));
    } catch (cause) { setError(`批准失败：${errorMessage(cause)}`); }
    finally { locks.current.delete(approval.approvalId); setPending((values) => values.filter((value) => value !== approval.approvalId)); }
  }, []);
  return { tasks, stop, approve, pending, decisions, error: error ?? (snapshot.isError ? `subagent 状态读取失败：${errorMessage(snapshot.error)}` : null), refresh: snapshot.refetch, available: sidecarRunning === true };
}
