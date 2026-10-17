import type { AgentTeamTask } from "@yukinal/shared";
import { AGENT_TEAMS } from "../agent-nist/agent-nist.js";
import { AgentEntryView } from "./AgentEntryView.js";
import { teamTaskActive, TEAM_TASK_LABEL } from "./coordinator-state.js";
import type { useCoordinatorTasks } from "./useCoordinatorTasks.js";

type Runtime = ReturnType<typeof useCoordinatorTasks>;
export function CoordinatorStatus({ tasks, sessionId, preview = false }: { tasks: AgentTeamTask[]; sessionId: string | null; preview?: boolean }) {
  const active = tasks.filter((task) => task.sessionId === sessionId && teamTaskActive(task));
  if (preview || active.length === 0) return null;
  return <span className="coordinator-status" role="status" aria-label={`协调助手：${active.length} 项后台任务，可继续对话`} title="后台团队独立执行，可继续对话"><i className="coordinator-status-dot" aria-hidden="true" /><span>{active.length} 项后台任务</span></span>;
}

export function CoordinatorTasks({ runtime, sessionId }: { runtime: Runtime; sessionId: string | null }) {
  const current = runtime.tasks.filter((task) => task.sessionId === sessionId).sort((a, b) => a.createdAt.localeCompare(b.createdAt));
  const elsewhere = runtime.tasks.filter((task) => task.sessionId !== sessionId && teamTaskActive(task));
  const render = (task: AgentTeamTask) => {
    const active = teamTaskActive(task);
    return <article key={task.id} className={`coordinator-task team-color-${task.team}`} data-state={task.state} aria-label={`${task.title} · ${TEAM_TASK_LABEL[task.state]}`}>
      <div className="coordinator-task-line"><span className={active && task.state !== "waiting_approval" && runtime.available ? "coordinator-task-pulse" : "coordinator-task-dot"} aria-hidden="true" /><strong>{task.title}</strong><span className="coordinator-task-state">{runtime.available || !active ? TEAM_TASK_LABEL[task.state] : "连接中断"}</span>
        {active ? <button type="button" className="text-button" disabled={!runtime.available || runtime.pending.includes(task.runId)} aria-label={`停止任务：${task.title}`} onClick={() => void runtime.stop(task.runId)}>停止</button> : null}</div>
      <div className="coordinator-task-meta"><span>{AGENT_TEAMS.find((team) => team.id === task.team)?.label}</span><span>{task.access === "write" ? "变更任务" : "只读任务"}</span>{task.lastTool && active ? <span>{task.lastTool}</span> : null}</div>
      {task.result || task.error ? <details className="coordinator-task-result" open={task.state === "failed" || task.state === "interrupted"}><summary>{task.error ? "查看问题" : "查看结果"}</summary><p>{task.result || task.error}</p></details> : null}
      {task.approval ? <AgentEntryView entry={{ kind: "approval", approval: task.approval }} onApproval={runtime.approve} approvalBusy={!runtime.available || runtime.pending.includes(task.approval.approvalId)} approvalStatus={runtime.decisions[task.approval.approvalId]} /> : null}
    </article>;
  };
  return <>
    {current.length ? <section className="coordinator-tasks" aria-label="团队后台任务"><div className="coordinator-tasks-heading">团队任务 <span>{current.filter(teamTaskActive).length} 进行中</span></div>{current.map(render)}</section> : null}
    {elsewhere.length ? <details className="coordinator-other-tasks"><summary>其他对话 · {elsewhere.length} 项后台任务{elsewhere.some((task) => task.state === "waiting_approval") ? " · 需要批准" : ""}</summary>{elsewhere.map(render)}</details> : null}
    {runtime.error ? <p className="agent-history-error" role="status">{runtime.error}<button type="button" className="text-button" onClick={() => void runtime.refresh()}>刷新</button></p> : null}
  </>;
}
