/**
 * 一次 Agent 运行的事件接线：8 个通道 + runId/终态/审批簿记。
 *
 * 从 `useAgentRun` 里拿出来，是因为这段代码只是「把宿主发来的帧送给谁」，
 * 与钩子自身的状态机无关；放在一起会淹掉真正需要读懂的生命周期握手。
 */

import type { AgentRunResult, AgentStage, AgentTokenUsage } from "@yukinal/shared";

import {
  subscribeDesktop,
  type AgentEventName,
  type DesktopEventPayload,
  type DesktopSubscription,
} from "../../lib/ipc.js";
import { useTeamRuntimeStore } from "../agent-nist/team-runtime-store.js";
import type { AgentRunStream } from "./run-stream.js";
import type { AdmissionRef } from "./run-lifecycle.js";
import {
  appendEntries,
  appendToolCall,
  settleToolResult,
  targetLabel,
  toolOutcomeFromEvent,
  toolPurpose,
  type Entry,
} from "./transcript.js";

export type AgentRunEventWiring = {
  stream: AgentRunStream;
  /** 终态帧的回调；由 `useAgentRun` 提供，负责落库与清空阶段。 */
  completeRun: (eventRunId: string, result: AgentRunResult) => void;
  /** 运行开始等待审批时通知面板，用 ref 读取以免闭包过期。 */
  onApprovalRequested: { current: (() => void) | undefined };
  setRunId: (runId: string) => void;
  setRunning: (running: boolean) => void;
  setRunState: (state: string | null) => void;
  setStage: (stage: AgentStage) => void;
  setTokenUsage: (usage: AgentTokenUsage) => void;
  setStopping: (stopping: boolean) => void;
  setPendingApprovalIds: (value: string[] | ((current: string[]) => string[])) => void;
  /** 未执行完的受理记录；终态帧到达时清掉对应条目。 */
  pendingAdmission: AdmissionRef;
  setEntries: (value: Entry[] | ((current: Entry[]) => Entry[])) => void;
  setApprovalStatuses: (value: Record<string, string> | ((current: Record<string, string>) => Record<string, string>)) => void;
  setListening: (listening: boolean) => void;
};

/**
 * 接上所有事件通道并返回退订函数。
 *
 * 泛型**就是重点**：`on` 曾经把 handler 的参数写成 `(payload: AgentStreamEvent) => void`，
 * 于是 `subscribeDesktop(name, …)` 按通道收窄出来的类型在这里又被拓宽回整个联合，
 * 下游只能靠 9 次 `as Extract<…>` 重新收窄 —— 而 cast 正是「通过粗闸门但形状不符的
 * payload 被当成没校验过的形状交给 handler」的地方（`event.result` 对别的成员就是
 * undefined）。保留 `E` 之后，每个 handler 拿到的就是它那个通道的**成员**类型。
 */
export function wireAgentRunEvents(wiring: AgentRunEventWiring): () => void {
  const {
    stream,
    completeRun,
    onApprovalRequested,
    setRunId,
    setRunning,
    setRunState,
    setStage,
    setTokenUsage,
    setStopping,
    setPendingApprovalIds,
    pendingAdmission,
    setEntries,
    setApprovalStatuses,
    setListening,
  } = wiring;
  let disposed = false;
    const subscriptions: DesktopSubscription[] = [];
    // 订阅/退订的竞态由 `subscribeDesktop` 负责；这里只关心两件事 —— 把 8 个通道
    // 接上，以及「全部接上」这个时刻（`ready`），因为 listen 是异步的，在它完成
    // 之前发提示词会丢掉回答。
    // 泛型**就是重点**：`on` 曾经把 handler 的参数写成 `(payload: AgentStreamEvent) => void`，
    // 于是 `subscribeDesktop(name, …)` 按通道收窄出来的类型，在这里又被拓宽回整个联合，
    // 下游只能靠 9 次 `as Extract<…>` 重新收窄 —— 而 cast 正是「通过粗闸门但形状不符的
    // payload 被当成没校验过的形状交给 handler」的地方（`event.result` 对别的成员就是
    // undefined）。保留 `E` 之后，每个 handler 拿到的就是它那个通道的**成员**类型。
    // （`subscribeDesktop(name, …)` 不需要显式写 `<E>`：`name: E` 能从字面量反推出 `E`。）
    const on = <E extends AgentEventName>(name: E, handler: (payload: DesktopEventPayload<E>) => void): void => {
      subscriptions.push(
        subscribeDesktop(name, (payload) => {
          if (disposed) return;
          handler(payload);
        }),
      );
    };

    const isActive = (payload: { runId: string }): boolean => stream.isActive(payload.runId);

    on("agent.started", (event) => {
      if (!stream.started(event.runId)) return;
      setRunId(event.runId);
      setRunning(true);
      setRunState("thinking");
      setStage("intake");
      useTeamRuntimeStore.getState().stage(event.runId, "thinking");
    });
    on("agent.stage", (event) => {
      if (!isActive(event)) return;
      // Only the payload's own stage is stored; the runtime derived it from
      // executed results, so a late or duplicated frame cannot invent progress.
      setStage(event.stage);
    });
    on("agent.thinking", (event) => {
      if (!isActive(event)) return;
      const delta = event.textDelta ?? "";
      stream.enqueue(event.runId, "reasoning", delta);
    });
    on("agent.text", (event) => {
      if (!isActive(event)) return;
      stream.enqueue(event.runId, "assistant", event.textDelta);
    });
    on("agent.usage", (event) => {
      useTeamRuntimeStore.getState().usage(event.runId, event.usage);
      if (!isActive(event)) return;
      setTokenUsage(event.usage);
    });
    on("agent.tool_call", (event) => {
      if (!isActive(event)) return;
      stream.boundary();
      setRunState("running_tool");
      useTeamRuntimeStore.getState().stage(event.runId, "running_tool");
      useTeamRuntimeStore.getState().tool(event.runId, event.toolName, event.target.serverId ?? null, false, event.callId);
      setEntries((current) =>
        appendToolCall(current, {
          callId: event.callId,
          toolName: event.toolName,
          purpose: toolPurpose(event.input),
          target: targetLabel(event.target),
          riskLevel: event.riskLevel,
          decision: event.decision,
          approvedBy: event.approvedBy,
        }),
      );
    });
    on("agent.tool_result", (event) => {
      if (!isActive(event)) return;
      stream.boundary();
      useTeamRuntimeStore.getState().stage(event.runId, "thinking");
      useTeamRuntimeStore.getState().tool(event.runId, event.toolName, event.target.serverId ?? null, true, event.callId);
      setRunState("thinking");
      setEntries((current) =>
        settleToolResult(
          current,
          {
            callId: event.callId,
            toolName: event.toolName,
          purpose: toolPurpose(event.input),
            target: targetLabel(event.target),
            riskLevel: event.riskLevel,
            decision: event.decision,
            approvedBy: event.approvedBy,
          },
          toolOutcomeFromEvent(event),
        ),
      );
    });
    on("agent.waiting_approval", (event) => {
      if (!isActive(event)) return;
      stream.boundary();
      if (event.approval.autoApproved === true) {
        // Auto-approval mode: the host already minted and accepted its one-shot
        // ticket. This is a record of a call that is running, not a decision
        // waiting for a person, so it must not interrupt or render buttons.
        return;
      }
      setRunState("waiting_approval");
      useTeamRuntimeStore.getState().stage(event.runId, "waiting_approval");
      onApprovalRequested.current?.();
      setEntries((current) => appendEntries(current, [{ kind: "approval", approval: event.approval }]));
    });
    on("agent.approval_expired", (event) => {
      if (!isActive(event)) return;
      stream.flush();
      setApprovalStatuses((current) => ({ ...current, [event.approvalId]: "审批已过期" }));
    });
    on("agent.completed", (event) => completeRun(event.runId, event.result));
    on("agent.failed", (event) => {
      if (!stream.finish(event.runId)) return;
      useTeamRuntimeStore.getState().stage(event.runId, "failed");
      if (pendingAdmission.current?.runId === event.runId) pendingAdmission.current = null;
      setRunning(false);
      setRunState("failed");
      setStopping(false);
      setPendingApprovalIds([]);
      setEntries((current) => appendEntries(current, [{ kind: "error", text: event.error }]));
    });

    void Promise.all(subscriptions.map((subscription) => subscription.ready))
      .then(() => {
        if (!disposed) setListening(true);
      })
      .catch((error) => {
        if (!disposed) setEntries(appendEntries([], [{ kind: "error", text: `无法接收 Agent 事件：${String(error)}` }]));
      });
  return () => {
    disposed = true;
    subscriptions.forEach((subscription) => subscription.stop());
  };
}
