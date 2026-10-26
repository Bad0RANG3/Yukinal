/**
 * 一次 Agent 运行的全部机制：Tauri 事件流、run 生命周期握手、审批簿记。
 *
 * 这个模块存在的理由，是 panel 里最难写对、也最难用界面验证的那部分：
 *
 *  - Rust 可能在 `agent_run_start` 命令 resolve *之前* 就发出 started/completed，
 *    所以「claim 一个 run 槽位」必须先于命令发生，并且只接受自己期望的那个 runId。
 *  - 一个 dying sidecar 会在没人看事件流的时候断开，因此轮询到的存活信号
 *    也必须能把在途运行判死。
 *  - 审批是一次性的：重复点击、过期、运行已结束都必须被挡在 IPC 之外。
 *
 * 调用方只需要说「跑这句话」和「回复怎么落库」，其余顺序由这里保证。
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

import { callDesktop, isDesktopShell, subscribeDesktop, type AgentEventName, type DesktopEventPayload, type DesktopSubscription } from "../../lib/ipc.js";
import { errorMessage } from "../../lib/format.js";
import { resetAdmissionOnFailure, type AdmissionRef, type PendingAdmission } from "./run-lifecycle.js";
import { ASSISTANT_REPLY_NOT_SAVED, assistantReplyNotSaved } from "./session-persistence.js";
import type { AgentTeam } from "../agent-nist/agent-teams.js";
import { useTeamRuntimeStore } from "../agent-nist/team-runtime-store.js";
import { AgentRunStream, appendStreamBatch, settleStreamText, type RunStreamScope } from "./run-stream.js";
import {
  appendEntries,
  appendToolCall,
  settleToolResult,
  targetLabel,
  toolPurpose,
  toolOutcomeFromEvent,
  type Entry,
  type ReplyIdentity,
} from "./transcript.js";

const SIDECAR_EXIT_NOTICE = "Agent sidecar 已退出，本次运行已中断。";
/**
 * 崩溃后已被自动重启时的措辞。与上面那句分开，是因为用户的处境不同：这里进程已经
 * 回来了（可以立刻重试），而上面那句需要用户自己按下启动——把两者说成一句话会让
 * 用户在「已经恢复」和「还没恢复」之间猜。
 */
const SIDECAR_RESTART_NOTICE = "Agent sidecar 崩溃后已自动重启，本次运行已中断，可以重新提问。";

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
  /**
   * 这次运行按哪套策略判定。不传 = 按目标环境自动（sidecar 用内建表推导）；
   * 传了就必须是 sidecar 认得的内建策略 id，否则整次调用被拒。
   */
  policyId?: string;
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
    const resetView = (): void => {
      mounted.current = false;
      stream.reset();
      stoppingScope.current = null;
    };
    if (!isDesktopShell()) return resetView;
    const subscriptions: DesktopSubscription[] = [];
    let disposed = false;
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
      resetView();
    };
  }, []);

  // 轮询到的存活信号是权威的：进程没了，在途运行不可能继续。
  useEffect(() => {
    if (!running || sidecarRunning !== false) return;
    if (!stream.fail()) return;
    resetAdmissionOnFailure(pendingAdmission);
    useTeamRuntimeStore.getState().runs.filter((run) => ["starting", "thinking", "running_tool", "waiting_approval"].includes(run.stage)).forEach((run) => useTeamRuntimeStore.getState().stage(run.id, "interrupted"));
    setRunning(false);
    setRunId(null);
    setStopping(false);
    setPendingApprovalIds([]);
    setRunState("failed");
    setEntries((current) => {
      const last = current.at(-1);
      if (last?.kind === "error" && last.text === SIDECAR_EXIT_NOTICE) return current;
      return appendEntries(current, [{ kind: "error", text: SIDECAR_EXIT_NOTICE }]);
    });
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
    if (!stream.fail()) return;
    resetAdmissionOnFailure(pendingAdmission);
    useTeamRuntimeStore.getState().runs.filter((run) => ["starting", "thinking", "running_tool", "waiting_approval"].includes(run.stage)).forEach((run) => useTeamRuntimeStore.getState().stage(run.id, "interrupted"));
    setRunning(false);
    setRunId(null);
    setStopping(false);
    setPendingApprovalIds([]);
    setRunState("failed");
    setEntries((current) => {
      const last = current.at(-1);
      if (last?.kind === "error" && last.text === SIDECAR_RESTART_NOTICE) return current;
      return appendEntries(current, [{ kind: "error", text: SIDECAR_RESTART_NOTICE }]);
    });
  }, [sidecarRestart, running]);

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
    const routingKey = JSON.stringify({ persona: input.persona, subagents: input.subagents, participant: input.participant, providerId: input.providerId, model: input.model, focusServerId: input.focusServerId, reasoningEffort: input.reasoningEffort, toolAccess: input.toolAccess, permissionMode: input.permissionMode, mode: input.mode, policyId: input.policyId });
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
        // Omitted policy means the sidecar derives it from the target environment.
        policyId: input.policyId ?? undefined,
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
