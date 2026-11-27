/**
 * sidecar 死亡的两种权威信号：轮询到的不存活，以及自动重启记录的变化。
 *
 * 从 `useAgentRun` 里拿出来，是因为这两段只做「把在途运行判死」这一件事，
 * 不必和 run 的受理握手挤在同一个文件里。
 */

import { useEffect, useRef } from "react";

import { useTeamRuntimeStore } from "../agent-nist/team-runtime-store.js";
import { resetAdmissionOnFailure, type AdmissionRef } from "./run-lifecycle.js";
import type { AgentRunStream } from "./run-stream.js";
import { appendEntries, type Entry } from "./transcript.js";

export const SIDECAR_EXIT_NOTICE = "Agent sidecar 已退出，本次运行已中断。";
/**
 * 崩溃后已被自动重启时的措辞。与上面那句分开，是因为用户的处境不同：这里进程已经
 * 回来了（可以立刻重试），而上面那句需要用户自己按下启动——把两者说成一句话会让
 * 用户在「已经恢复」和「还没恢复」之间猜。
 */
export const SIDECAR_RESTART_NOTICE = "Agent sidecar 崩溃后已自动重启，本次运行已中断，可以重新提问。";

/** 界面状态写入器；由调用方传入，让这段逻辑不必知道 React 的 state 布局。 */
export type RunViewSetters = {
  setRunning: (running: boolean) => void;
  setRunId: (runId: string | null) => void;
  setStopping: (stopping: boolean) => void;
  setPendingApprovalIds: (value: string[]) => void;
  setRunState: (state: string | null) => void;
  setEntries: (value: Entry[] | ((current: Entry[]) => Entry[])) => void;
};

export type SidecarWatchdogOptions = RunViewSetters & {
  stream: AgentRunStream;
  pendingAdmission: AdmissionRef;
  running: boolean;
  sidecarRunning?: boolean;
  sidecarRestart?: string | null;
};

/**
 * 把在途运行判死，并让界面回到「失败但可以重试」。
 *
 * 三条不变量：只终止一次（`stream.fail()` 自守）、未执行的受理记录一并作废、
 * 同一条提示不重复追加。
 */
function markInterrupted(options: SidecarWatchdogOptions, notice: string): void {
  const { stream, pendingAdmission, setRunning, setRunId, setStopping, setPendingApprovalIds, setRunState, setEntries } =
    options;
  if (!stream.fail()) return;
  resetAdmissionOnFailure(pendingAdmission);
  useTeamRuntimeStore
    .getState()
    .runs.filter((run) => ["starting", "thinking", "running_tool", "waiting_approval"].includes(run.stage))
    .forEach((run) => useTeamRuntimeStore.getState().stage(run.id, "interrupted"));
  setRunning(false);
  setRunId(null);
  setStopping(false);
  setPendingApprovalIds([]);
  setRunState("failed");
  setEntries((current) => {
    const last = current.at(-1);
    if (last?.kind === "error" && last.text === notice) return current;
    return appendEntries(current, [{ kind: "error", text: notice }]);
  });
}

/** 挂上两条死亡信号：轮询到的不存活，与「真的重启过一次」。 */
export function useSidecarWatchdog(options: SidecarWatchdogOptions): void {
  const { running, sidecarRunning, sidecarRestart } = options;

  // 轮询到的存活信号是权威的：进程没了，在途运行不可能继续。
  useEffect(() => {
    if (!running || sidecarRunning !== false) return;
    markInterrupted(options, SIDECAR_EXIT_NOTICE);
  }, [sidecarRunning, running]);

  // 自动重启把上面那条信号变得不可靠：崩溃后一两次尝试就恢复了，而运行中的轮询是
  // 1.5 秒一次，界面完全可能只看到 `running === true`。那会让一个已经随进程消失的
  // run 永远停在「运行中」，用户等一个不会到来的回答。
  //
  // 重启记录是权威的第二条信号，而且它只在**真的发生了一次重启**时变化（用户自己的
  // 启动/停止会把记录清空，那种情况由上面那条覆盖，所以清空不在这里触发失败）。
  const seenRestart = useRef<string | null>(sidecarRestart ?? null);
  useEffect(() => {
    const next = sidecarRestart ?? null;
    if (next === null || next === seenRestart.current) return;
    seenRestart.current = next;
    if (!running) return;
    markInterrupted(options, SIDECAR_RESTART_NOTICE);
  }, [sidecarRestart, running]);
}
