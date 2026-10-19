import type { InvestigationTask, TaskStatus } from "@yukinal/shared";
import type { AgentTeam } from "./agent-nist.js";
import { TEAM_STAGE_LABEL, teamRunActive, type TeamRun, type TeamCue } from "./team-runtime-store.js";

export const BOARD_COLUMNS = [
  { id: "pending", label: "待处理" },
  { id: "working", label: "进行中" },
  { id: "waiting", label: "待确认" },
  { id: "ended", label: "已结束" },
] as const;
export type BoardColumn = (typeof BOARD_COLUMNS)[number]["id"];
export type TeamBoardCard = {
  id: string; column: BoardColumn; goal: string; serverId: string | null;
  team?: AgentTeam; status: string; failed: boolean; updatedAt: number;
  task?: InvestigationTask; run?: TeamRun; preview?: TeamCue;
};
const TASK_COLUMN: Record<TaskStatus, BoardColumn> = {
  pending: "pending", investigating: "working", executing: "working", verifying: "working",
  waiting_user: "waiting", completed: "ended", failed: "ended", stopped: "ended", expired: "ended",
};
const TASK_LABEL: Record<TaskStatus, string> = {
  pending: "待开始", investigating: "调查中", executing: "执行中", verifying: "验证中",
  waiting_user: "等待用户", completed: "已完成", failed: "失败", stopped: "已停止", expired: "已过期",
};
function runColumn(run: TeamRun): BoardColumn {
  return run.stage === "starting" ? "pending" : run.stage === "waiting_approval" ? "waiting" : teamRunActive(run) ? "working" : "ended";
}

/** Durable tasks own workflow state; a model attempt finishing does not complete its task. */
export function teamBoardCards(tasks: InvestigationTask[], runs: TeamRun[]): TeamBoardCard[] {
  const joined = new Set<string>();
  const cards: TeamBoardCard[] = tasks.map((task) => {
    const serverId = task.scope.serverId ?? null;
    const attempts = runs.filter((run) => run.serverId === serverId && (run.taskId === task.id || run.id === task.activeRunId));
    attempts.forEach((run) => joined.add(run.id));
    const run = attempts.sort((a, b) => b.startedAt - a.startedAt)[0];
    const active = run && teamRunActive(run) && TASK_COLUMN[task.status] !== "ended" ? run : undefined;
    return {
      id: `task:${task.id}`, column: active ? runColumn(active) : TASK_COLUMN[task.status], goal: task.objective,
      serverId, team: run?.team, status: active ? TEAM_STAGE_LABEL[active.stage] : TASK_LABEL[task.status],
      failed: task.status === "failed", updatedAt: Math.max(Date.parse(task.updatedAt) || 0, run?.updatedAt ?? 0), task, run,
    };
  });
  for (const run of runs) {
    if (run.source === "coordinator" || joined.has(run.id)) continue;
    cards.push({ id: `run:${run.id}`, column: runColumn(run), goal: run.goal, serverId: run.serverId, team: run.team,
      status: run.source === "worker" && run.stage === "completed" ? "已返回" : TEAM_STAGE_LABEL[run.stage], failed: run.stage === "failed" || run.stage === "interrupted", updatedAt: run.updatedAt, run });
  }
  return cards.sort((a, b) => b.updatedAt - a.updatedAt || a.id.localeCompare(b.id));
}

export function boardCardFeedback(card: TeamBoardCard, cues: TeamCue[], now = Date.now()): { team: AgentTeam; phase: "working" | "recent"; key: string } | null {
  const cue = card.preview ?? cues.find((item) => item.runId === card.run?.id && item.finished && item.until > now);
  if (card.run && card.column !== "ended" && ["starting", "thinking", "running_tool"].includes(card.run.stage)) {
    return { team: card.run.team, phase: "working", key: `${card.run.id}:active` };
  }
  if (card.run?.stage === "waiting_approval") return null;
  if (cue && cue.until > now && (cue.finished || cue.preview)) return { team: cue.team, phase: cue.finished ? "recent" : "working", key: `${cue.runId}:${cue.callId}:${cue.finished}` };
  return null;
}

/** Explicit demo projections stay outside durable tasks, runs, transcripts and usage. */
export function boardPreviewCards(cues: TeamCue[], now = Date.now()): TeamBoardCard[] {
  return cues.filter((cue) => cue.preview && cue.until > now).map((cue) => ({
    id: `preview:${cue.runId}:${cue.callId}`, column: cue.finished ? "ended" : "working",
    goal: cue.callId === "logs" ? "读取服务日志，定位异常线索" : "读取环境文件，检查部署配置",
    serverId: cue.serverId, team: cue.team, status: cue.finished ? "模拟操作已返回" : "模拟操作中", failed: false, updatedAt: 0, preview: cue,
  }));
}
