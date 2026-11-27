import { useEffect, useRef, useState, type MouseEvent, type FocusEvent } from "react";
import { useTeamRuntimeStore, teamRunActive, type TeamCue } from "./team-runtime-store.js";
import { nextHeartbeatAt, toolSurface } from "./team-operations.js";
import type { WorkpageTool } from "../workpages/workpage-model.js";

/** AI accounting clock only. Data subscriptions and queries update independently. */
export function TeamHeartbeat() {
  const cues = useTeamRuntimeStore((state) => state.cues);
  useEffect(() => {
    let timer: ReturnType<typeof setTimeout>;
    let disposed = false;
    const tick = () => {
      if (disposed) return;
      const now = Date.now();
      useTeamRuntimeStore.getState().pulse(now);
      timer = setTimeout(tick, Math.max(100, nextHeartbeatAt(now) - now));
    };
    tick();
    return () => { disposed = true; clearTimeout(timer); };
  }, []);
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
  return <TeamActionCueView {...target} cues={cues} />;
}

export function TeamActionCueView({ cues, ...target }: OperationTarget & { cues: TeamCue[] }) {
  const matches = matchingCues(cues, target);
  const cue = matches[0];
  if (!cue) return null;
  const title = matches.map((item) => `${item.preview ? "模拟反馈 · " : ""}${item.finished ? "工具已返回" : "操作中"} · ${item.tool}`).join("\n");
  return <span className={`team-action-cue team-color-blue ${cue.finished ? "is-recent" : "is-working"}`} title={title}><span aria-hidden="true" />{cue.preview ? "演示 · " : "运维 Agent"}{matches.length > 1 ? ` · ${matches.length} 项操作` : cue.finished ? " · 刚刚" : ""}</span>;
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
  return <button type="button" className="team-feedback-preview agent-nist-text-action" disabled={previewing || busy || !serverId} title="仅演示运维读日志、执行读文件时的并行提示，不连接服务器" onClick={() => {
    const id = `preview-${Date.now()}`;
    previewId.current = id;
    setPreviewing(true);
    timers.current.forEach(clearTimeout);
    useTeamRuntimeStore.setState((state) => ({ cues: [...state.cues.filter((cue) => !cue.preview), ...([
      { callId: "logs", team: "blue", tool: "server.logs" },
      { callId: "files", team: "blue", tool: "filesystem.read" },
    ] as const).map((cue) => ({ ...cue, runId: id, serverId, finished: false, until: Number.MAX_SAFE_INTEGER, preview: true }))] }));
    timers.current = [4000, 7000].map((delay, index) => setTimeout(() => {
      useTeamRuntimeStore.setState((state) => ({ cues: state.cues.map((cue) => cue.runId === id && cue.callId === (index ? "files" : "logs") ? { ...cue, finished: true, until: Date.now() + 1800 } : cue) }));
      if (index) setPreviewing(false);
    }, delay));
  }}>{previewing ? "正在演示并行提示…" : "预览并行操作反馈"}<span>模拟</span></button>;
}


export function recordTeamAttention(event: MouseEvent<HTMLElement> | FocusEvent<HTMLElement>) {
  if (!(event.target instanceof Element)) return;
  const target = event.target.closest<HTMLElement>("[data-attention]");
  if (!target || !event.currentTarget.contains(target)) return;
  const action = target.dataset.attention;
  if (action) useTeamRuntimeStore.getState().noteAttention(action, target.dataset.serverId ?? null);
}
