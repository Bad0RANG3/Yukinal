import type { AgentTeamTask, AgentCoordinationConfig, AiProviderConfig, Server } from "@yukinal/shared";
import { create } from "zustand";
import { roleBinding, rolePersona, type AgentRole } from "../agent-nist/agent-nist.js";
import { resolveMentionedServer, type MentionCandidate } from "./composer-triggers.js";
import { useTeamRuntimeStore, type TeamStage } from "../agent-nist/team-runtime-store.js";
import type { Entry } from "./transcript.js";

/** Internal dispatch receipts stay in the trace; chat shows team states and results. */
export function coordinatorEntries(entries: Entry[]): Entry[] {
  return entries.filter((entry) => entry.kind !== "tool" || !["teams.dispatch", "teams.status", "teams.cancel"].includes(entry.toolName));
}

export const TEAM_TASK_STAGE: Record<AgentTeamTask["state"], TeamStage> = {
  queued: "starting", running: "thinking", waiting_approval: "waiting_approval",
  completed: "completed", failed: "failed", cancelled: "cancelled", interrupted: "interrupted",
};
export const TEAM_TASK_LABEL: Record<AgentTeamTask["state"], string> = {
  queued: "排队中", running: "处理中", waiting_approval: "待批准",
  completed: "已返回", failed: "失败", cancelled: "已停止", interrupted: "已中断",
};
export function teamTaskActive(task: AgentTeamTask): boolean {
  return ["queued", "running", "waiting_approval"].includes(task.state);
}

/** Late snapshots/events cannot revive a terminal task or cross its immutable binding. */
export function reconcileTeamTask(current: AgentTeamTask | undefined, incoming: AgentTeamTask): AgentTeamTask {
  if (!current) return incoming;
  if (incoming.id !== current.id || incoming.runId !== current.runId || incoming.sessionId !== current.sessionId || incoming.serverId !== current.serverId || incoming.parentRunId !== current.parentRunId || incoming.team !== current.team) return current;
  if (incoming.revision <= current.revision || (!teamTaskActive(current) && teamTaskActive(incoming))) return current;
  return incoming;
}

export const useCoordinatorStore = create<{
  tasks: AgentTeamTask[];
  receive(task: AgentTeamTask): void;
}>((set) => ({
  tasks: [],
  receive: (task) => set((state) => {
    const previous = state.tasks.find((item) => item.id === task.id);
    const accepted = reconcileTeamTask(previous, task);
    if (accepted === previous) return state;
    const runtime = useTeamRuntimeStore.getState();
    if (!runtime.runs.some((run) => run.id === task.runId)) runtime.begin({
      id: task.runId, team: task.team, source: "worker", serverId: task.serverId, sessionId: task.sessionId, goal: task.title,
    });
    runtime.stage(task.runId, TEAM_TASK_STAGE[task.state]);
    if (task.usage) runtime.usage(task.runId, task.usage);
    return { tasks: [...state.tasks.filter((item) => item.id !== task.id), task] };
  }),
}));

/** Multiple explicit server mentions require a global conversation to retain both scopes. */
export function coordinatorScope(prompt: string, candidates: readonly MentionCandidate[], selectedServerId: string | null): string | null {
  const targets = candidates.filter((candidate) => resolveMentionedServer(prompt, [candidate]));
  return targets.length > 1 ? null : targets[0]?.id ?? selectedServerId;
}

/** Explicitly broken bindings are omitted; unbound roles inherit the coordinator model. */
export function coordinatorConfig(roles: readonly AgentRole[], providers: readonly AiProviderConfig[], servers: readonly Server[], preferredRoleId: string | null): AgentCoordinationConfig {
  const available = roles.flatMap((role): AgentCoordinationConfig["roles"] => {
    const binding = roleBinding(role, providers);
    if (role.providerId && binding.reason) return [];
    const persona = rolePersona(role);
    return [{ id: role.id, team: role.team, label: persona.label, systemPrompt: persona.systemPrompt,
      ...(binding.provider && binding.choice ? { providerId: binding.provider.id, model: binding.choice.model } : {}) }];
  });
  return { version: 1, roles: available, servers: servers.slice(0, 128).map((server) => ({ id: server.id, name: server.name.slice(0, 200), environment: server.metadata.environment ?? "unknown" })),
    ...(preferredRoleId && available.some((role) => role.id === preferredRoleId) ? { preferredRoleId } : {}) };
}
