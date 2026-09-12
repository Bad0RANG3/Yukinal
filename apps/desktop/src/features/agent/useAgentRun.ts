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
  type AgentPermissionMode,
  type AgentRunMode,
  type ApprovalDecision,
  type ApprovalRequest,
} from "@yukinal/shared";
import { useCallback, useEffect, useRef, useState } from "react";

import { callDesktop, isDesktopShell, subscribeDesktop, type AgentEventName, type DesktopEventPayload, type DesktopSubscription } from "../../lib/ipc.js";
import { RunLifecycle } from "./run-lifecycle.js";
import {
  appendAssistantDelta,
  appendEntries,
  appendToolCall,
  settleAssistantText,
  settleToolResult,
  targetLabel,
  toolResultSummary,
  type Entry,
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
  /**
   * 持久化这条用户消息并返回它所属的 sessionId。
   * 动态本身不落库，所以这一步由调用方提供，而不是由本模块猜测。
   */
  persistUserMessage: (messageId: string) => Promise<string>;
  providerId?: string | null;
  model?: string | null;
  focusServerId?: string | null;
  permissionMode?: AgentPermissionMode;
  /** 运行模式：只读 / 计划 / 目标。由 sidecar 的权限引擎强制。 */
  mode?: AgentRunMode;
  /**
   * 这次运行按哪套策略判定。不传 = 按目标环境自动（sidecar 用内建表推导）；
   * 传了就必须是 sidecar 认得的内建策略 id，否则整次调用被拒。
   */
  policyId?: string;
};

export type AgentRun = {
  entries: Entry[];
  running: boolean;
  runState: string | null;
  runId: string | null;
  stopping: boolean;
  /** 事件订阅是否已经装好；没装好就不该允许发送。 */
  listening: boolean;
  pendingApprovalIds: string[];
  approvalStatuses: Record<string, string>;
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
  onAssistantMessage?: (text: string) => void;
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

  // 流式文本按 run 累积（事件乱序也没关系，同 run 追加）。
  const lifecycle = useRef(new RunLifecycle());
  const approvalLocks = useRef(new Set<string>());

  const [entries, setEntries] = useState<Entry[]>([]);
  const [running, setRunning] = useState(false);
  const [runState, setRunState] = useState<string | null>(null);
  const [runId, setRunId] = useState<string | null>(null);
  const [stopping, setStopping] = useState(false);
  const [listening, setListening] = useState(false);
  const [pendingApprovalIds, setPendingApprovalIds] = useState<string[]>([]);
  const [approvalStatuses, setApprovalStatuses] = useState<Record<string, string>>({});

  useEffect(() => {
    if (!isDesktopShell()) return;
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

    const isActive = (payload: { runId: string }): boolean => lifecycle.current.isActive(payload.runId);

    on("agent.started", (event) => {
      if (!lifecycle.current.started(event.runId)) return;
      setRunId(event.runId);
      setRunning(true);
      setRunState("thinking");
    });
    on("agent.thinking", (event) => {
      if (!isActive(event)) return;
      const delta = event.textDelta ?? "";
      setEntries((current) => appendAssistantDelta(current, delta));
    });
    on("agent.tool_call", (event) => {
      if (!isActive(event)) return;
      setRunState("running_tool");
      setEntries((current) =>
        appendToolCall(current, {
          callId: event.callId,
          toolName: event.toolName,
          target: targetLabel(event.target),
          riskLevel: event.riskLevel,
          decision: event.decision,
          approvedBy: event.approvedBy,
        }),
      );
    });
    on("agent.tool_result", (event) => {
      if (!isActive(event)) return;
      setRunState("thinking");
      setEntries((current) =>
        settleToolResult(
          current,
          {
            callId: event.callId,
            toolName: event.toolName,
            target: targetLabel(event.target),
            riskLevel: event.riskLevel,
            decision: event.decision,
            approvedBy: event.approvedBy,
          },
          {
            status: event.status,
            durationMs: event.durationMs,
            summary: toolResultSummary(event.toolName, event.outputSummary),
          },
        ),
      );
    });
    on("agent.waiting_approval", (event) => {
      if (!isActive(event)) return;
      setRunState("waiting_approval");
      onApprovalRequested.current?.();
      setEntries((current) => appendEntries(current, [{ kind: "approval", approval: event.approval }]));
    });
    on("agent.approval_expired", (event) => {
      if (!isActive(event)) return;
      setApprovalStatuses((current) => ({ ...current, [event.approvalId]: "审批已过期" }));
    });
    on("agent.completed", (event) => {
      if (!lifecycle.current.finish(event.runId)) return;
      setRunning(false);
      setRunState(event.result.state);
      if (!event.result.text) return;
      onAssistantMessage.current?.(event.result.text);
      setEntries((current) => settleAssistantText(current, event.result.text));
    });
    on("agent.failed", (event) => {
      if (!lifecycle.current.finish(event.runId)) return;
      setRunning(false);
      setRunState("failed");
      setEntries((current) => appendEntries(current, [{ kind: "error", text: event.error }]));
    });

    void Promise.all(subscriptions.map((subscription) => subscription.ready))
      .then(() => {
        if (!disposed) setListening(true);
      })
      .catch((error) => {
        if (!disposed) setEntries([{ kind: "error", text: `无法接收 Agent 事件：${String(error)}` }]);
      });

    return () => {
      disposed = true;
      subscriptions.forEach((subscription) => subscription.stop());
    };
  }, []);

  // 轮询到的存活信号是权威的：进程没了，在途运行不可能继续。
  useEffect(() => {
    if (!running || sidecarRunning !== false) return;
    lifecycle.current.fail();
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
    lifecycle.current.fail();
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
    const expectedRunId = newId("run");
    if (!lifecycle.current.begin(expectedRunId)) return "busy";
    setRunning(true);
    setRunState("starting");
    setRunId(null);
    setApprovalStatuses({});
    setPendingApprovalIds([]);
    approvalLocks.current.clear();

    const prompt = input.prompt;
    const messageId = newId("msg");
    try {
      setEntries([{ kind: "user", text: prompt }]);
      const sessionId = await input.persistUserMessage(messageId);
      const { runId: started } = await callDesktop(IPC_COMMANDS.agentRunStart, {
        runId: expectedRunId,
        sessionId,
        prompt,
        messageId,
        parts: [{ type: "text", text: prompt }],
        delivery: "async",
        resume: true,
        providerId: input.providerId ?? undefined,
        model: input.model ?? undefined,
        focusServerId: input.focusServerId ?? undefined,
        permissionMode: input.permissionMode,
        mode: input.mode,
        // 不传 = 由 sidecar 按目标环境推导策略。这里不做任何「默认策略」的猜测：
        // 猜错就是让这次运行受另一套策略约束，而界面显示的却是「按环境自动」。
        policyId: input.policyId ?? undefined,
      });
      if (lifecycle.current.acknowledge(started)) {
        setRunId(started);
        setRunState("thinking");
      }
      return "started";
    } catch (error) {
      lifecycle.current.fail();
      setRunning(false);
      setRunState("failed");
      setEntries([{ kind: "user", text: prompt }, { kind: "error", text: String(error) }]);
      return "failed";
    }
  }, []);

  const stop = useCallback(async (): Promise<void> => {
    if (!runId || stopping) return;
    setStopping(true);
    try {
      const { stopped } = await callDesktop(IPC_COMMANDS.agentRunStop, { runId });
      if (stopped && lifecycle.current.finish(runId)) {
        setRunning(false);
        setRunState("cancelled");
      } else if (!stopped && lifecycle.current.isActive(runId)) {
        throw new Error("停止请求未被接受，请重试。");
      }
    } catch (error) {
      setEntries((current) => appendEntries(current, [{ kind: "error", text: String(error) }]));
    } finally {
      setStopping(false);
    }
  }, [runId, stopping]);

  const respondApproval = useCallback(
    async (approval: ApprovalRequest, decision: ApprovalDecision): Promise<void> => {
      if (approvalLocks.current.has(approval.approvalId) || !lifecycle.current.isActive(approval.runId)) return;
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
        approvalLocks.current.delete(approval.approvalId);
        setEntries((current) => appendEntries(current, [{ kind: "error", text: String(error) }]));
      } finally {
        setPendingApprovalIds((current) => current.filter((id) => id !== approval.approvalId));
      }
    },
    [],
  );

  const loadTranscript = useCallback((next: Entry[]): void => {
    setEntries(next);
    setRunState(null);
    setRunId(null);
    setStopping(false);
    setPendingApprovalIds([]);
    setApprovalStatuses({});
    approvalLocks.current.clear();
  }, []);

  const clearTranscript = useCallback((): void => {
    loadTranscript([]);
  }, [loadTranscript]);

  const appendSystemEntry = useCallback((text: string): void => {
    setEntries((current) => appendEntries(current, [{ kind: "system", text }]));
  }, []);

  return {
    entries,
    running,
    runState,
    runId,
    stopping,
    listening,
    pendingApprovalIds,
    approvalStatuses,
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
