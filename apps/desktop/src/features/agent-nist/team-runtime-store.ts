import { create } from "zustand";
import type { AgentTokenUsage } from "@yukinal/shared";
import type { AgentTeam } from "./agent-nist.js";
import { heartbeatSlot, mergeRunUsage } from "./team-operations.js";

export type TeamStage = "starting" | "thinking" | "running_tool" | "waiting_approval" | "completed" | "failed" | "cancelled" | "interrupted";
export type TeamRun = { source?: "coordinator" | "worker"; id: string; team: AgentTeam; serverId: string | null; sessionId?: string; taskId?: string; goal: string; stage: TeamStage; startedAt: number; updatedAt: number; usage?: AgentTokenUsage };
export type TeamPanelView = "teams" | "watch" | "config";
export type TeamCue = { runId: string; callId: string; team: AgentTeam; serverId: string | null; tool: string; finished: boolean; until: number; preview?: boolean };
type RuntimeState = {
  runs: TeamRun[]; cues: TeamCue[]; beat: number; lastBeatAt: number | null; panelView: TeamPanelView;
  totals: { inputTokens: number; outputTokens: number; reportedRuns: number; unreportedRuns: number };
  attention: { action: string; serverId: string | null; at: number } | null;
  begin(input: Pick<TeamRun, "id" | "team" | "serverId" | "goal" | "sessionId" | "taskId" | "source">): void;
  bind(id: string, sessionId: string, taskId?: string | null): void;
  stage(id: string, stage: TeamStage): void;
  tool(id: string, tool: string, serverId: string | null, finished: boolean, callId: string): void;
  usage(id: string, usage: AgentTokenUsage): void;
  pulse(now: number): void;
  expireCues(now: number): void;
  noteAttention(action: string, serverId: string | null): void;
  setPanelView(view: TeamPanelView): void;
};
export const TEAM_STAGE_LABEL: Record<TeamStage, string> = { starting: "正在受理", thinking: "分析与规划", running_tool: "正在调用工具", waiting_approval: "等待批准", completed: "已完成", failed: "失败", cancelled: "已取消", interrupted: "已中断" };
export function teamRunActive(run: TeamRun): boolean { return ["starting", "thinking", "running_tool", "waiting_approval"].includes(run.stage); }

export const useTeamRuntimeStore = create<RuntimeState>((set) => ({
  runs: [], cues: [], beat: heartbeatSlot(Date.now()), lastBeatAt: null, panelView: "teams",
  totals: { inputTokens: 0, outputTokens: 0, reportedRuns: 0, unreportedRuns: 0 }, attention: null,
  begin: (input) => set((state) => {
    const previous = state.runs.find((run) => run.id === input.id);
    const run: TeamRun = previous ? { ...previous, stage: "starting", updatedAt: Date.now() } : { ...input, goal: input.goal.slice(0, 160), stage: "starting", startedAt: Date.now(), updatedAt: Date.now() };
    return { runs: [...state.runs.filter((item) => item.id !== input.id), run] };
  }),
  bind: (id, sessionId, taskId) => set((state) => ({ runs: state.runs.map((run) => run.id === id && (!run.sessionId || run.sessionId === sessionId) ? { ...run, sessionId, taskId: taskId ?? run.taskId } : run) })),
  stage: (id, stage) => set((state) => ({
    runs: state.runs.map((run) => run.id === id ? { ...run, stage, updatedAt: Date.now() } : run),
    cues: ["starting", "thinking", "running_tool"].includes(stage) ? state.cues : state.cues.filter((cue) => cue.runId !== id || cue.finished),
  })),
  tool: (id, tool, serverId, finished, callId) => set((state) => {
    const run = state.runs.find((item) => item.id === id);
    if (!run || !teamRunActive(run) || run.stage === "waiting_approval") return state;
    const previous = state.cues.find((cue) => cue.runId === id && cue.callId === callId);
    // Duplicate or late events cannot restart a completed operation or extend its trail.
    if (previous?.finished) return state;
    return { cues: [...state.cues.filter((cue) => cue.runId !== id || cue.callId !== callId), { runId: id, callId, team: run.team, serverId, tool, finished, until: finished ? Date.now() + 1800 : Number.MAX_SAFE_INTEGER }] };
  }),
  usage: (id, usage) => set((state) => ({ runs: state.runs.map((run) => run.id === id ? { ...run, usage: mergeRunUsage(run.usage, usage) } : run) })),
  pulse: (now) => set((state) => ({ beat: heartbeatSlot(now), lastBeatAt: now, totals: state.runs.reduce((total, run) => ({
    inputTokens: total.inputTokens + (run.usage?.inputTokens ?? 0), outputTokens: total.outputTokens + (run.usage?.outputTokens ?? 0),
    reportedRuns: total.reportedRuns + Number(Boolean(run.usage) && run.usage?.reportedRequests !== 0),
    unreportedRuns: total.unreportedRuns + Number(!run.usage || run.usage.reportedRequests === 0 || (run.usage.unreportedRequests ?? 0) > 0),
  }), { inputTokens: 0, outputTokens: 0, reportedRuns: 0, unreportedRuns: 0 }) })),
  expireCues: (now) => set((state) => ({ cues: state.cues.filter((cue) => !cue.finished || cue.until > now) })),
  noteAttention: (action, serverId) => set({ attention: { action, serverId, at: Date.now() } }),
  setPanelView: (panelView) => set({ panelView }),
}));
