/**
 * 一次 Agent 运行的全部机制：宿主事件流、run 生命周期握手、审批簿记。
 *
 * 这个模块存在的理由，是 panel 里最难写对、也最难用界面验证的那部分：
 *
 *  - 宿主可能在 `agent_run_start` 命令 resolve *之前* 就发出 started/completed，
 *    所以「claim 一个 run 槽位」必须先于命令发生，并且只接受自己期望的那个 runId。
 *  - 一个 dying sidecar 会在没人看事件流的时候断开，因此轮询到的存活信号
 *    也必须能把在途运行判死（见 `sidecar-watchdog.ts`）。
 *  - 审批是一次性的：重复点击、过期、运行已结束都必须被挡在 IPC 之外。
 *
 * 调用方只需要说「跑这句话」和「回复怎么落库」，其余顺序由这里保证。
 * 事件接线在 `run-events.ts`，死亡信号在 `sidecar-watchdog.ts`；本文件只留生命周期本身。
 */

import {
  IPC_COMMANDS,
  type AgentPromptPart,
  type AgentSubagentConfig,
  type AgentPermissionMode,
  type AgentRunMode,
  type AgentRunResult,
  type AgentStage,
  type AgentTokenUsage,
  type AgentContextUsage,
  type AgentParticipant,
  type AgentPersona,
  type AgentReasoningEffort,
  type ApprovalDecision,
  type ApprovalRequest,
  type IpcCommandMap,
} from "@yukinal/shared";
import { useCallback, useEffect, useRef, useState } from "react";

import { callDesktop, isDesktopShell } from "../../lib/ipc.js";
import { errorMessage } from "../../lib/format.js";
import { resetAdmissionOnFailure, type AdmissionRef, type PendingAdmission } from "./run-lifecycle.js";
import { ASSISTANT_REPLY_NOT_SAVED, assistantReplyNotSaved } from "./session-persistence.js";
import type { AgentTeam } from "../agent-nist/agent-teams.js";
import { useTeamRuntimeStore } from "../agent-nist/team-runtime-store.js";
import { AgentRunStream, appendStreamBatch, settleStreamText, type RunStreamScope } from "./run-stream.js";
import { wireAgentRunEvents } from "./run-events.js";
import { useSidecarWatchdog } from "./sidecar-watchdog.js";
import { appendEntries, type Entry, type ReplyIdentity } from "./transcript.js";


export type StartOutcome =
  /** 运行已提交（或已生成 runId 并等待 started 事件）。 */
  | "started"
  /** 已经有运行在途，本次输入原样保留。 */
  | "busy"
  /** 提交失败，调用方应把 prompt 还给用户。 */
  | "failed";

export type StartRunInput = {
  prompt: string;
  persona?: AgentPersona;
  subagents?: AgentSubagentConfig;
  team?: AgentTeam;
  /** Snapshot captured on first admission, retained unchanged across retries. */
  uiContext?: string;
  participant?: AgentParticipant;
  /** Optional durable investigation objective receiving read-only evidence. */
  taskId?: string | null;
  /** Exact prompt parts, including bounded images, PDFs and text files. Omitted means text-only fallback. */
  parts?: AgentPromptPart[];
  /**
   * 持久化这条用户消息并返回它所属的 sessionId。
   * 动态本身不落库，所以这一步由调用方提供，而不是由本模块猜测。
   */
  persistUserMessage: (messageId: string, parts: AgentPromptPart[]) => Promise<string>;
  providerId?: string | null;
  model?: string | null;
  reasoningEffort?: AgentReasoningEffort;
  toolAccess?: "none";
  focusServerId?: string | null;
  permissionMode?: AgentPermissionMode;
  /** 运行模式：只读 / 计划 / 目标。由 sidecar 的权限引擎强制。 */
  mode?: AgentRunMode;
  /** `sync` waits for the terminal result before the start call resolves. */
  delivery?: "async" | "sync";
};

export type AgentRun = {
  entries: Entry[];
  running: boolean;
  runState: string | null;
  /**
   * The operating stage the agent has actually reached (evidence, diagnosis,
   * change, verify …), derived from executed tool results by the runtime. It is
   * an observation, never the model's own claim, so the UI may label it as fact.
   */
  stage: AgentStage | null;
  runId: string | null;
  stopping: boolean;
  /** 事件订阅是否已经装好；没装好就不该允许发送。 */
  listening: boolean;
  pendingApprovalIds: string[];
  approvalStatuses: Record<string, string>;
  tokenUsage: AgentTokenUsage | null;
  contextUsage: AgentContextUsage | null;
  start(input: StartRunInput): Promise<StartOutcome>;
  stop(): Promise<void>;
  respondApproval(approval: ApprovalRequest, decision: ApprovalDecision): Promise<void>;
  /** 用一段既有对话替换动态（打开本地记录）。 */
  loadTranscript(entries: Entry[]): void;
  /** 清空动态并回到空闲（新建任务）。 */
  clearTranscript(): void;
  /**
   * 追加一条应用自己产生的内容（例如 /help 的回复）。
   * 与 Agent 的回复分开，因此不会被误读成模型说过的话，也不落库。
   */
  appendSystemEntry(text: string): void;
};

export function useAgentRun(options: {
  /**
   * 运行以终态回复结束时的回调 —— 落库的唯一入口。
   * 通过 ref 读取，因此订阅只建立一次也不会拿到过期的闭包。
   */
  onAssistantMessage?: (text: string, identity: ReplyIdentity, sessionId: string) => boolean | void | Promise<boolean | void>;
  /** 运行开始等待审批；面板据此把 Agent 面板带到前景。 */
  onApprovalRequested?: () => void;
  /** sidecar 存活信号；false 表示进程已经不在了。 */
  sidecarRunning?: boolean;
  /**
   * 最近一次自动恢复的标识（`restart.attempt:restart.at`，来自 `agent_status`）。
   *
   * 自动重启让「轮询到的 `running === false`」不再可靠：崩溃与重启之间可能短于一个
   * 轮询间隔（运行中 1.5 秒一次），界面就会看到 `running` 一直是 true，而在途的 run
   * 其实已经随进程一起消失了。重启记录是那条消息的可靠来源——它按崩溃递增，所以
   * 「它变了」就等于「进程死过一次」。
   */
  sidecarRestart?: string | null;
}): AgentRun {
  const { sidecarRunning, sidecarRestart } = options;
  const onAssistantMessage = useRef(options.onAssistantMessage);
  onAssistantMessage.current = options.onAssistantMessage;
  const onApprovalRequested = useRef(options.onApprovalRequested);
  onApprovalRequested.current = options.onApprovalRequested;

  const approvalLocks = useRef(new Set<string>());
  const stoppingScope = useRef<RunStreamScope | null>(null);
  const mounted = useRef(true);
  /**
   * A message admitted with `resume: false` but not yet executed.
   *
   * This is the UI half of the durable admission contract: a retry reuses the same
   * `messageId` and `runId` instead of inserting another user message or forking a run.
   */
  const pendingAdmission: AdmissionRef = useRef<PendingAdmission | null>(null);

  const [entries, setEntries] = useState<Entry[]>([]);
  const [running, setRunning] = useState(false);
  const [runState, setRunState] = useState<string | null>(null);
  const [stage, setStage] = useState<AgentStage | null>(null);
  const [runId, setRunId] = useState<string | null>(null);
  const [stopping, setStopping] = useState(false);
  const [listening, setListening] = useState(false);
  const [pendingApprovalIds, setPendingApprovalIds] = useState<string[]>([]);
  const [approvalStatuses, setApprovalStatuses] = useState<Record<string, string>>({});
  const [tokenUsage, setTokenUsage] = useState<AgentTokenUsage | null>(null);
  const [contextUsage, setContextUsage] = useState<AgentContextUsage | null>(null);
  const streamRef = useRef<AgentRunStream | null>(null);
  if (streamRef.current === null) {
    streamRef.current = new AgentRunStream((deltas, identity) => {
      // Capture the run's identity now; React may apply this updater after a new run begins.
      setEntries((current) => appendStreamBatch(current, deltas, identity));
    });
  }
  const stream = streamRef.current;

  const completeRun = useCallback((eventRunId: string, result: AgentRunResult): void => {
    if (result.runId !== eventRunId) return;
    const finished = stream.finish(eventRunId);
    if (!finished) return;
    const { scope, streamedText } = finished;
    if (pendingAdmission.current?.runId === eventRunId) pendingAdmission.current = null;
    setRunning(false);
    setStopping(false);
    setPendingApprovalIds([]);
    setRunState(result.state);
    // The run is no longer working, so a stale operating stage must not label it.
    // A completed run used to be labelled `report` locally, which duplicated — and
    // could contradict — the runtime's own `agent.stage` frame. The runtime is the
    // producer now; this only clears the stage when the run did not complete.
    setStage((current) => (result.state === "completed" ? current : null));
    useTeamRuntimeStore.getState().stage(eventRunId, result.state === "completed" ? "completed" : result.state === "cancelled" ? "cancelled" : "failed");
    setContextUsage(result.context ?? null);
    if (result.text) {
      if (scope.sessionId) {
        void Promise.resolve(onAssistantMessage.current?.(result.text, scope.identity, scope.sessionId))
          .then((persisted) => {
            if (mounted.current && assistantReplyNotSaved(persisted)) {
              setEntries((current) => appendEntries(current, [{ kind: "error", text: ASSISTANT_REPLY_NOT_SAVED }]));
            }
          })
          .catch((error: unknown) => {
            if (mounted.current) setEntries((current) => appendEntries(current, [{ kind: "error", text: errorMessage(error) }]));
          });
      }
      setEntries((current) => settleStreamText(current, result.text, scope.identity, streamedText));
    } else if (result.state === "failed" && result.error) {
      const error = result.error;
      setEntries((current) => appendEntries(current, [{ kind: "error", text: error }]));
    }
  }, []);

  useEffect(() => {
    mounted.current = true;
    if (!isDesktopShell()) {
      mounted.current = false;
      stream.reset();
      stoppingScope.current = null;
      return;
    }
    const disconnect = wireAgentRunEvents({
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
      setApprovalStatuses,
      setEntries,
      setListening,
      pendingAdmission,
    });
    return () => {
      disconnect();
      mounted.current = false;
      stream.reset();
      stoppingScope.current = null;
    };
  }, []);

  useSidecarWatchdog({
    stream,
    pendingAdmission,
    running,
    sidecarRunning,
    sidecarRestart,
    setRunning,
    setRunId,
    setStopping,
    setPendingApprovalIds,
    setRunState,
    setEntries,
  });

  const start = useCallback(async (input: StartRunInput): Promise<StartOutcome> => {
    if (!mounted.current) return "failed";
    const prompt = input.prompt;
    const parts =
      input.parts?.length
        ? input.parts
        : prompt.trim()
          ? [{ type: "text" as const, text: prompt }]
          : [];
    if (!parts.length) {
      setEntries(appendEntries([], [{ kind: "error", text: "消息必须包含文字或图片。" }]));
      return "failed";
    }
    const contentKey = JSON.stringify(parts);
    const routingKey = JSON.stringify({ persona: input.persona, subagents: input.subagents, participant: input.participant, providerId: input.providerId, model: input.model, focusServerId: input.focusServerId, reasoningEffort: input.reasoningEffort, toolAccess: input.toolAccess, permissionMode: input.permissionMode, mode: input.mode });
    const previous = pendingAdmission.current;
    const reusable =
      previous?.prompt === prompt && JSON.stringify(previous.parts) === contentKey && previous.taskId === (input.taskId ?? null) && previous.routingKey === routingKey
        ? previous
        : null;
    const expectedRunId = reusable?.runId ?? newId("run");
    const scope = stream.begin(expectedRunId, { ...(input.participant ? { participant: input.participant } : {}), ...(input.model ? { model: input.model } : {}) });
    if (!scope) return "busy";
    if (input.team) useTeamRuntimeStore.getState().begin({ id: expectedRunId, team: input.team, serverId: input.focusServerId ?? null, taskId: input.taskId ?? undefined, goal: prompt });
    setRunning(true);
    setRunState("starting");
    setStage("intake");
    setRunId(null);
    setApprovalStatuses({});
    setPendingApprovalIds([]);
    setTokenUsage(null);
    setStopping(false);
    stoppingScope.current = null;
    approvalLocks.current.clear();

    const attachments = parts.filter((part) => part.type === "image" || part.type === "file" || part.type === "document");
    const messageId = reusable?.messageId ?? newId("msg");
    const userEntry: Entry = {
      kind: "user",
      entryId: messageId,
      text: prompt,
      ...(attachments.length ? { attachments } : {}),
    };
    try {
      if (!reusable) setEntries((current) => appendEntries(current, [userEntry]));
      // The first call records the message without executing it. The second call is
      // the resume; keeping these separate is what makes retrying the second call
      // idempotent rather than producing a second user message or run.
      if (!reusable) {
        const sessionId = await input.persistUserMessage(messageId, parts);
        if (!stream.isPending(scope)) return "started";
        pendingAdmission.current = { runId: expectedRunId, messageId, sessionId, prompt, parts, taskId: input.taskId ?? null, routingKey, uiContext: input.uiContext };
      }
      const admitted = pendingAdmission.current;
      if (!admitted || admitted.runId !== scope.runId) throw new Error("运行受理状态丢失，请重新发送。");
      scope.sessionId = admitted.sessionId;
      useTeamRuntimeStore.getState().bind(expectedRunId, admitted.sessionId, input.taskId);
      const request: IpcCommandMap[typeof IPC_COMMANDS.agentRunStart]["params"] = {
        runId: admitted.runId,
        sessionId: admitted.sessionId,
        taskId: input.taskId ?? undefined,
        prompt,
        messageId: admitted.messageId,
        parts,
        providerId: input.providerId ?? undefined,
        model: input.model ?? undefined,
        reasoningEffort: input.reasoningEffort,
        toolAccess: input.toolAccess,
        subagents: input.subagents,
        persona: input.persona && admitted.uiContext ? { ...input.persona, systemPrompt: `${input.persona.systemPrompt}\n\n${admitted.uiContext}` } : input.persona,
        participant: input.participant,
        focusServerId: input.focusServerId ?? undefined,
        permissionMode: input.permissionMode,
        mode: input.mode,
      };
      const admissionResponse = await callDesktop(IPC_COMMANDS.agentRunStart, {
        ...request, delivery: "async", resume: false,
      });
      if (!stream.isPending(scope)) return "started";
      if (admissionResponse.runId !== admitted.runId) {
        throw new Error("sidecar 为已受理消息返回了不同的 runId。");
      }
      const response = await callDesktop(IPC_COMMANDS.agentRunStart, {
        ...request, delivery: input.delivery ?? "async", resume: true,
      });
      if (!stream.isPending(scope)) return "started";
      if (response.runId !== scope.runId) throw new Error("sidecar 为运行返回了不同的 runId。");
      if (response.duplicate || !response.started || !response.resumed) {
        throw new Error("这条消息已经由另一个调用启动，当前界面不能重复接管它。");
      }
      if (pendingAdmission.current?.runId === scope.runId) pendingAdmission.current = null;
      const started = response.runId;
      if (stream.acknowledge(scope)) {
        setRunId(started);
        setRunState("thinking");
      }
      // Sync delivery can carry the terminal snapshot even if its event was missed.
      if (response.result && ["completed", "cancelled", "failed"].includes(response.result.state)) completeRun(started, response.result);
      return "started";
    } catch (error) {
      // A terminal event or a view change wins over a late RPC response/error.
      if (!stream.fail(scope)) return "started";
      resetAdmissionOnFailure(pendingAdmission);
      useTeamRuntimeStore.getState().stage(expectedRunId, "failed");
      setRunning(false);
      setStopping(false);
      setPendingApprovalIds([]);
      setRunState("failed");
      setEntries((current) => appendEntries(current, [{ kind: "error", text: String(error) }]));
      return "failed";
    }
  }, [completeRun]);

  const stop = useCallback(async (): Promise<void> => {
    const scope = runId ? stream.scopeFor(runId) : null;
    if (!scope || !stream.isPending(scope) || stoppingScope.current === scope) return;
    stoppingScope.current = scope;
    setStopping(true);
    try {
      const { stopped } = await callDesktop(IPC_COMMANDS.agentRunStop, { runId: scope.runId });
      if (!stream.isPending(scope)) return;
      if (stopped && stream.finish(scope.runId)) {
        setRunning(false);
        setPendingApprovalIds([]);
        setRunState("cancelled");
        useTeamRuntimeStore.getState().stage(scope.runId, "cancelled");
      } else if (!stopped) {
        throw new Error("停止请求未被接受，请重试。");
      }
    } catch (error) {
      if (!stream.isPending(scope)) return;
      stream.flush();
      setEntries((current) => appendEntries(current, [{ kind: "error", text: String(error) }]));
    } finally {
      if (stoppingScope.current === scope) stoppingScope.current = null;
      if (stream.isCurrent(scope)) setStopping(false);
    }
  }, [runId]);

  const respondApproval = useCallback(
    async (approval: ApprovalRequest, decision: ApprovalDecision): Promise<void> => {
      const scope = stream.scopeFor(approval.runId);
      if (!scope || approvalLocks.current.has(approval.approvalId) || !stream.isActive(approval.runId)) return;
      if (Date.parse(approval.expiresAt) <= Date.now()) {
        setApprovalStatuses((current) => ({ ...current, [approval.approvalId]: "审批已过期" }));
        return;
      }
      approvalLocks.current.add(approval.approvalId);
      setPendingApprovalIds((current) => [...current, approval.approvalId]);
      try {
        const { accepted } = await callDesktop(IPC_COMMANDS.agentApprovalRespond, {
          approvalId: approval.approvalId,
          runId: approval.runId,
          decision,
          respondedAt: new Date().toISOString(),
        });
        if (!stream.isCurrent(scope)) return;
        setApprovalStatuses((current) => ({
          ...current,
          [approval.approvalId]: !accepted
            ? "审批已失效"
            : decision === "reject"
              ? "已拒绝"
              : decision === "approve_once"
                ? "已批准一次"
                : "已批准本次运行",
        }));
      } catch (error) {
        if (!stream.isPending(scope)) return;
        approvalLocks.current.delete(approval.approvalId);
        stream.flush();
        setEntries((current) => appendEntries(current, [{ kind: "error", text: String(error) }]));
      } finally {
        if (stream.isCurrent(scope)) setPendingApprovalIds((current) => current.filter((id) => id !== approval.approvalId));
      }
    },
    [],
  );

  const loadTranscript = useCallback((next: Entry[]): void => {
    stream.reset();
    stoppingScope.current = null;
    pendingAdmission.current = null;
    setEntries(appendEntries([], next));
    setRunning(false);
    setRunState(null);
    setStage(null);
    setRunId(null);
    setStopping(false);
    setPendingApprovalIds([]);
    setApprovalStatuses({});
    setTokenUsage(null);
    setContextUsage(null);
    approvalLocks.current.clear();
  }, []);

  const clearTranscript = useCallback((): void => {
    pendingAdmission.current = null;
    loadTranscript([]);
  }, [loadTranscript]);

  const appendSystemEntry = useCallback((text: string): void => {
    stream.flush();
    setEntries((current) => appendEntries(current, [{ kind: "system", text }]));
  }, []);

  return {
    entries,
    running,
    runState,
    stage,
    runId,
    stopping,
    listening,
    pendingApprovalIds,
    approvalStatuses,
    tokenUsage,
    contextUsage,
    start,
    stop,
    respondApproval,
    loadTranscript,
    clearTranscript,
    appendSystemEntry,
  };
}

function newId(prefix: string): string {
  const unique = globalThis.crypto?.randomUUID?.() ?? `${Date.now()}_${Math.random().toString(36).slice(2)}`;
  return `${prefix}_${unique}`;
}
