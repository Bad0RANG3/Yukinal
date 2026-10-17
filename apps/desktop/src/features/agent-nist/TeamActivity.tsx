import { useEffect, useRef, useState, type MouseEvent, type FocusEvent } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { AGENT_TEAMS } from "./agent-nist.js";
import { useTeamRuntimeStore, TEAM_STAGE_LABEL, teamRunActive, type TeamCue } from "./team-runtime-store.js";
import { nextHeartbeatAt, toolSurface } from "./team-operations.js";
import type { WorkpageTool } from "../workpages/workpage-model.js";

/** The main window owns one UI refresh clock. This does not dispatch scheduled work. */
export function TeamHeartbeat() {
  const queryClient = useQueryClient();
  const cues = useTeamRuntimeStore((state) => state.cues);
  useEffect(() => {
    let timer: ReturnType<typeof setTimeout>;
    let disposed = false;
    const tick = () => {
      if (disposed) return;
      const now = Date.now();
      useTeamRuntimeStore.getState().pulse(now);
      void queryClient.invalidateQueries({ queryKey: ["team-watch"] });
      void queryClient.invalidateQueries({ queryKey: ["team-board-tasks"] });
      void queryClient.invalidateQueries({ queryKey: ["agent-team-tasks"] });
      timer = setTimeout(tick, Math.max(100, nextHeartbeatAt(now) - now));
    };
    tick();
    return () => { disposed = true; clearTimeout(timer); };
  }, [queryClient]);
  useEffect(() => {
    const until = Math.min(...cues.filter((cue) => cue.finished).map((cue) => cue.until));
    if (!Number.isFinite(until)) return;
    const timer = setTimeout(() => useTeamRuntimeStore.getState().expireCues(Date.now()), Math.max(0, until - Date.now()));
    return () => clearTimeout(timer);
  }, [cues]);
  return null;
}

type OperationTarget = { serverId: string | null; surface?: WorkpageTool };
function matchingCues(cues: TeamCue[], { serverId, surface }: OperationTarget): TeamCue[] {
  return cues.filter((cue) => cue.until > Date.now() && cue.serverId === serverId && (!surface || toolSurface(cue.tool) === surface)).sort((a, b) => Number(a.finished) - Number(b.finished));
}

/** A shared overlay keeps borders on rows, buttons and open workpages consistent. */
export function TeamOperationOutline(target: OperationTarget) {
  const cues = useTeamRuntimeStore((state) => state.cues);
  const cue = matchingCues(cues, target)[0];
  return cue ? <span key={`${cue.runId}:${cue.callId}:${cue.finished}`} aria-hidden="true" className={`team-operation-outline team-color-${cue.team}`} data-operation={cue.finished ? "recent" : "working"} /> : null;
}

export function TeamActionCue(target: OperationTarget) {
  const cues = useTeamRuntimeStore((state) => state.cues);
  const matches = matchingCues(cues, target);
  const cue = matches[0];
  if (!cue) return null;
  const team = AGENT_TEAMS.find((candidate) => candidate.id === cue.team);
  const teams = new Set(matches.map((item) => item.team));
  const title = matches.map((item) => `${item.preview ? "模拟反馈 · " : ""}${AGENT_TEAMS.find((candidate) => candidate.id === item.team)?.label} · ${item.finished ? "工具已返回" : "操作中"} · ${item.tool}`).join("\n");
  return <span className={`team-action-cue team-color-${cue.team} ${cue.finished ? "is-recent" : "is-working"}`} title={title}><span aria-hidden="true" />{cue.preview ? "演示 · " : ""}{team?.label}{teams.size > 1 ? ` +${teams.size - 1}` : cue.finished ? " · 刚刚" : ""}</span>;
}

/** The left navigation also signals work when the board is not in view. */
export function TeamBoardNavFeedback() {
  const runs = useTeamRuntimeStore((state) => state.runs);
  const cues = useTeamRuntimeStore((state) => state.cues);
  const working = runs.filter((run) => run.source !== "coordinator" && ["starting", "thinking", "running_tool"].includes(run.stage));
  const previews = cues.filter((cue) => cue.preview && !cue.finished && cue.until > Date.now());
  const recent = cues.find((cue) => cue.finished && cue.until > Date.now());
  const count = working.length + previews.length;
  const team = working[0]?.team ?? previews[0]?.team ?? recent?.team;
  if (!team) return null;
  return <><span key={count ? `${team}:active` : `${recent?.runId}:${recent?.callId}:recent`} aria-hidden="true" className={`team-operation-outline team-color-${team}`} data-operation={count ? "working" : "recent"} />{count ? <span className={`team-board-nav-count team-color-${team}`} title={previews.length ? "模拟操作反馈" : `${count} 个 AI 正在处理`} aria-label={previews.length ? `${count} 项模拟操作` : `${count} 个 AI 正在处理`}>{count}</span> : null}</>;
}

/** Explicit fixture-only preview: no connection, tool call, transcript or token record. */
export function TeamFeedbackPreview({ serverId }: { serverId: string | null }) {
  const [previewing, setPreviewing] = useState(false);
  const timers = useRef<ReturnType<typeof setTimeout>[]>([]);
  const previewId = useRef<string | null>(null);
  const busy = useTeamRuntimeStore((state) => state.runs.some(teamRunActive));
  useEffect(() => () => {
    timers.current.forEach(clearTimeout);
    useTeamRuntimeStore.setState((state) => ({ cues: state.cues.filter((cue) => cue.runId !== previewId.current) }));
  }, []);
  return <button type="button" className="team-feedback-preview agent-nist-text-action" disabled={previewing || busy || !serverId} title="仅演示蓝队读日志、黄队读文件时的并行提示，不连接服务器" onClick={() => {
    const id = `preview-${Date.now()}`;
    previewId.current = id;
    setPreviewing(true);
    timers.current.forEach(clearTimeout);
    useTeamRuntimeStore.setState((state) => ({ cues: [...state.cues.filter((cue) => !cue.preview), ...([
      { callId: "logs", team: "blue", tool: "server.logs" },
      { callId: "files", team: "yellow", tool: "filesystem.read" },
    ] as const).map((cue) => ({ ...cue, runId: id, serverId, finished: false, until: Number.MAX_SAFE_INTEGER, preview: true }))] }));
    timers.current = [4000, 7000].map((delay, index) => setTimeout(() => {
      useTeamRuntimeStore.setState((state) => ({ cues: state.cues.map((cue) => cue.runId === id && cue.callId === (index ? "files" : "logs") ? { ...cue, finished: true, until: Date.now() + 1800 } : cue) }));
      if (index) setPreviewing(false);
    }, delay));
  }}>{previewing ? "正在演示并行提示…" : "预览并行操作反馈"}<span>模拟</span></button>;
}

export function TeamRunStrip({ sessionId }: { sessionId: string | null }) {
  const runs = useTeamRuntimeStore((state) => state.runs);
  const run = [...runs].reverse().find((item) => teamRunActive(item) && item.sessionId === sessionId);
  if (!run) return null;
  const team = AGENT_TEAMS.find((candidate) => candidate.id === run.team);
  return <div className={`team-run-strip team-color-${run.team}`} role="status"><span className="team-run-indicator" aria-hidden="true" /><strong>{team?.label}</strong><span>{TEAM_STAGE_LABEL[run.stage]}</span><span className="team-run-goal" title={run.goal}>{run.goal}</span></div>;
}

export function recordTeamAttention(event: MouseEvent<HTMLElement> | FocusEvent<HTMLElement>) {
  if (!(event.target instanceof Element)) return;
  const target = event.target.closest<HTMLElement>("[data-attention]");
  if (!target || !event.currentTarget.contains(target)) return;
  const action = target.dataset.attention;
  if (action) useTeamRuntimeStore.getState().noteAttention(action, target.dataset.serverId ?? null);
}
