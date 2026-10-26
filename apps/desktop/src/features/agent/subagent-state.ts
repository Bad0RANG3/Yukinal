import type { AgentSubagentTask } from "@yukinal/shared";
import { create } from "zustand";

export const SUBAGENT_TASK_LABEL: Record<AgentSubagentTask["state"], string> = {
  queued: "排队中", running: "处理中", waiting_approval: "待批准",
  completed: "已返回", failed: "失败", cancelled: "已停止", interrupted: "已中断",
};

export function subagentTaskActive(task: AgentSubagentTask): boolean {
  return ["queued", "running", "waiting_approval"].includes(task.state);
}

function reconcile(current: AgentSubagentTask | undefined, incoming: AgentSubagentTask): AgentSubagentTask {
  if (!current) return incoming;
  if (incoming.id !== current.id || incoming.runId !== current.runId || incoming.sessionId !== current.sessionId ||
      incoming.serverId !== current.serverId || incoming.parentRunId !== current.parentRunId) return current;
  if (incoming.revision <= current.revision || (!subagentTaskActive(current) && subagentTaskActive(incoming))) return current;
  return incoming;
}

export const useSubagentStore = create<{
  tasks: AgentSubagentTask[];
  receive(task: AgentSubagentTask): void;
}>((set) => ({
  tasks: [],
  receive: (task) => set((state) => {
    const previous = state.tasks.find((item) => item.id === task.id);
    const next = reconcile(previous, task);
    return next === previous ? state : { tasks: [...state.tasks.filter((item) => item.id !== task.id), next] };
  }),
}));
