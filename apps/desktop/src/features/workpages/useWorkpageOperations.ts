import { useQuery, useQueryClient } from "@tanstack/react-query";
import { IPC_COMMANDS, type Activity, type ToolExecutionRecord } from "@yukinal/shared";
import { useEffect, useMemo, useState } from "react";
import { callDesktop, isDesktopShell, subscribeDesktop } from "../../lib/ipc.js";
import { useServers } from "../../lib/servers.js";
import { useWorkspaceStore } from "../../stores/workspace-store.js";
import { EXECUTION_STATUS_LABEL } from "../../lib/labels.js";
import { type WorkpageEvidence } from "./workpage-model.js";

export type WorkpageOperation = { id: string; actor: string; target: string; title: string; status: string; statusLabel: string; at: string; evidence?: WorkpageEvidence };

export function useWorkpageOperations() {
  const shell = isDesktopShell();
  const serverId = useWorkspaceStore((state) => state.selectedServerId);
  const busy = useWorkspaceStore((state) => state.agentBusy);
  const queryClient = useQueryClient();
  const [live, setLive] = useState<Array<{ runId: string; record: ToolExecutionRecord }>>([]);
  const activities = useQuery({
    queryKey: ["workpage-activities", serverId], enabled: shell, staleTime: 10_000,
    queryFn: async () => (await callDesktop(IPC_COMMANDS.activityList, { ...(serverId ? { serverId } : {}), limit: 30 })).activities,
  });
  const executions = useQuery({
    queryKey: ["workpage-tool-executions", serverId], enabled: shell, staleTime: 10_000,
    refetchInterval: busy ? 3000 : 30_000,
    queryFn: async () => (await callDesktop(IPC_COMMANDS.toolExecutionList, { ...(serverId ? { serverId } : {}), limit: 30 })).executions,
  });
  const servers = useServers({ enabled: shell });

  useEffect(() => {
    if (!shell) return;
    const refresh = () => {
      void queryClient.invalidateQueries({ queryKey: ["workpage-tool-executions"] });
      void queryClient.invalidateQueries({ queryKey: ["workpage-activities"] });
    };
    const calls = subscribeDesktop("agent.tool_call", (event) => {
      const record: ToolExecutionRecord = { traceId: event.traceId, stepId: event.stepId, callId: event.callId, toolName: event.toolName, serverId: event.target.serverId, environment: event.target.environment, riskLevel: event.riskLevel, decision: event.decision, approvedBy: event.approvedBy, input: event.input, startedAt: event.at, status: "running" };
      setLive((current) => [{ runId: event.runId, record }, ...current.filter((item) => item.record.callId !== event.callId)].slice(0, 30));
    });
    const results = subscribeDesktop("agent.tool_result", (event) => {
      const record: ToolExecutionRecord = { traceId: event.traceId, stepId: event.stepId, callId: event.callId, toolName: event.toolName, serverId: event.target.serverId, environment: event.target.environment, riskLevel: event.riskLevel, decision: event.decision, approvedBy: event.approvedBy, input: event.input, status: event.status, output: { summary: event.outputSummary, ...(event.executionState ? { executionState: event.executionState } : {}) }, error: event.error, startedAt: event.startedAt, endedAt: event.endedAt, durationMs: event.durationMs };
      setLive((current) => [{ runId: event.runId, record }, ...current.filter((item) => item.record.callId !== event.callId)].slice(0, 30));
      refresh();
    });
    const approvals = subscribeDesktop("agent.waiting_approval", (event) => {
      if (!event.approval.callId) return;
      setLive((current) => current.map((item) => item.record.callId === event.approval.callId ? { ...item, record: { ...item.record, status: "waiting_approval" } } : item));
    });
    const activity = subscribeDesktop("activity.created", refresh);
    const finish = (event: { runId: string }) => {
      setLive((current) => current.filter((item) => item.runId !== event.runId || !["running", "pending", "waiting_approval"].includes(item.record.status)));
      refresh();
    };
    const completed = subscribeDesktop("agent.completed", finish);
    const failed = subscribeDesktop("agent.failed", finish);
    return () => { calls.stop(); results.stop(); approvals.stop(); activity.stop(); completed.stop(); failed.stop(); };
  }, [queryClient, shell]);

  const rows = useMemo(() => {
    const names = new Map((servers.data ?? []).map((server) => [server.id, server.name]));
    const persisted = (executions.data ?? []).filter((record) => !serverId || record.serverId === serverId);
    const merged = new Map(persisted.map((record) => [record.callId, record]));
    // Persisted results include the full redacted output; live events carry summaries.
    for (const item of live) {
      if (serverId && item.record.serverId !== serverId) continue;
      const saved = merged.get(item.record.callId);
      if (!saved || ["running", "pending", "waiting_approval"].includes(saved.status)) merged.set(item.record.callId, item.record);
    }
    const activityRows = (activities.data ?? []).filter((activity) => !serverId || activity.serverId === serverId);
    const operations: WorkpageOperation[] = [...merged.values()].map((execution) => {
      const activity = activityRows.find((item) => item.traceId === execution.traceId);
      const actor = activity ? actorLabel(activity.actor) : live.some((item) => item.record.callId === execution.callId) ? "Agent" : "执行器";
      const target = execution.serverId ? names.get(execution.serverId) ?? execution.serverId : "本机";
      const title = operationTitle(execution.toolName);
      return { id: execution.callId, actor, target, title, status: execution.status, statusLabel: EXECUTION_STATUS_LABEL[execution.status], at: execution.startedAt,
        evidence: { id: execution.callId, title, actor, target, execution } };
    });
    const traced = new Set([...merged.values()].map((item) => item.traceId));
    for (const activity of activityRows) {
      if (activity.traceId && traced.has(activity.traceId)) continue;
      operations.push(operationFromActivity(activity, names));
    }
    return operations.sort((a, b) => b.at.localeCompare(a.at)).slice(0, 30);
  }, [activities.data, executions.data, live, serverId, servers.data]);
  return { rows, isLoading: activities.isLoading || executions.isLoading, error: executions.error ?? activities.error, refresh: () => { void executions.refetch(); void activities.refetch(); }, serverId };
}

function actorLabel(actor: string): string { return actor === "user" ? "你" : actor === "core" ? "Core" : actor; }
function operationTitle(tool: string): string {
  const labels: Record<string, string> = { "server.logs.read": "读取日志", "server.services.list": "读取服务", "server.health": "读取状态", "server.exec": "执行命令", "server.snapshot": "采集快照", "docker.ps": "读取容器", "docker.logs": "读取容器日志", "file.read": "读取文件", "file.list": "浏览目录" };
  return labels[tool] ?? tool;
}
function operationFromActivity(activity: Activity, names: Map<string, string>): WorkpageOperation {
  const outcome = activity.outcome;
  const labels = { success: "完成", failure: "失败", cancelled: "已取消", denied: "已拒绝" } as const;
  return { id: activity.id, actor: actorLabel(activity.actor), target: activity.serverId ? names.get(activity.serverId) ?? activity.serverId : "工作区", title: activity.title, status: outcome ?? "recorded", statusLabel: outcome ? labels[outcome] : "已记录", at: activity.createdAt };
}
