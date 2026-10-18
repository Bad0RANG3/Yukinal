import { useEffect, useRef } from "react";
import { isDesktopShell } from "../../lib/ipc.js";
import { ApprovalCard } from "./ApprovalCard.js";
import { nativeTaskRuns, useNativeTaskRunStore, visibleNativeTaskRuns, type NativeTaskSnapshot } from "./native-task-runs.js";

/** The app's persistent chat mounts this once; folding task pages never drops approvals. */
export function useNativeTaskRunEvents(running: boolean | undefined, restart: string | null): void {
  useEffect(() => {
    if (!isDesktopShell()) return;
    // connect records its error for the task UI; start also awaits the same ready promise.
    void nativeTaskRuns.connect().catch(() => {});
    return () => nativeTaskRuns.disconnect();
  }, []);
  useEffect(() => { if (running === false) nativeTaskRuns.interrupt(); }, [running]);
  const previousRestart = useRef(restart);
  useEffect(() => {
    const previous = previousRestart.current;
    previousRestart.current = restart;
    if (previous !== null && restart !== null && previous !== restart) nativeTaskRuns.interrupt();
  }, [restart]);
}

/** Both views use the same real approval object and submission lock, never a recreated ticket. */
export function NativeTaskApprovals(scope: { taskId?: string; sessionId?: string | null; serverId?: string | null }) {
  const runs = useNativeTaskRunStore((state) => state.runs);
  const error = useNativeTaskRunStore((state) => state.error);
  return <NativeTaskApprovalCards snapshot={{ runs, error }} scope={scope} />;
}

/** Render the same snapshot in both places; the runtime owns ticket identity and decisions. */
export function NativeTaskApprovalCards({ snapshot: { runs, error }, scope }: {
  snapshot: NativeTaskSnapshot;
  scope: { taskId?: string; sessionId?: string | null; serverId?: string | null };
}) {
  const visible = visibleNativeTaskRuns(runs, scope).filter((run) => run.approval);
  if (!visible.length && !(scope.taskId && error)) return null;
  return <section className="coordinator-tasks" aria-label="目标任务审批">
    {visible.map((run) => <article className="coordinator-task" key={run.runId} aria-label={`目标任务：${run.title}`}>
      <div className="coordinator-task-line"><strong>目标任务审批</strong><span>{run.pending ? "正在提交" : run.decision ?? "等待你的选择"}</span></div>
      <p>{run.title}</p>
      <ApprovalCard approval={run.approval!}
        onApproval={(approval, decision) => nativeTaskRuns.respond(approval, decision)}
        busy={run.pending} status={run.decision} />
      {run.error ? <p className="agent-history-error" role="alert">{run.error}</p> : null}
    </article>)}
    {scope.taskId && error ? <p className="agent-history-error" role="alert">{error}</p> : null}
  </section>;
}
