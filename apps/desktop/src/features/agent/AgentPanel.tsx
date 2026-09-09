/**
 * Agent 面板 —— 真实接线：输入一句话，Rust 解析 provider + 凭据 → sidecar
 * `agent.run.start` → 事件流回 UI（thinking / 工具卡片 / 审批 / completed）。
 *
 * 规则：不造假 transcript。没有运行中的 run 就没有消息；Stop 立刻掐断在途请求。
 */

import { AgentStreamEventSchema, IPC_COMMANDS, type AgentStreamEvent, type ApprovalRequest, type Environment, type PermissionMode, type RiskLevel } from "@yukinal/shared";
import { listen } from "@tauri-apps/api/event";
import { useQuery } from "@tanstack/react-query";
import { useEffect, useRef, useState } from "react";

import { callDesktop, isDesktopShell } from "../../lib/ipc.js";
import { useAgentStatus, useSpawnAgent } from "../../lib/runtime.js";
import { useWorkspaceStore } from "../../stores/workspace-store.js";
import { Icon } from "../../components/Icon.js";
import { KeywordText } from "../../components/KeywordText.js";
import { RunLifecycle } from "./run-lifecycle.js";
import { useServers } from "../../lib/servers.js";

type Entry =
  | { kind: "user"; text: string }
  | { kind: "assistant"; text: string }
  | { kind: "tool_call"; toolName: string; target: string; riskLevel: RiskLevel; decision: PermissionMode }
  | { kind: "tool_result"; toolName: string; status: "success" | "failed" | "cancelled"; durationMs: number; summary: string }
  | { kind: "approval"; approval: ApprovalRequest }
  | { kind: "error"; text: string };

const MAX_TRANSCRIPT_ENTRIES = 500;

function appendEntries(current: Entry[], additions: Entry[]): Entry[] {
  return [...current, ...additions].slice(-MAX_TRANSCRIPT_ENTRIES);
}

const RUN_STATE_LABEL: Record<string, string> = {
  starting: "正在提交…",
  thinking: "思考中…",
  running_tool: "执行工具…",
  waiting_approval: "等待审批…",
  completed: "完成",
  failed: "失败",
  cancelled: "已停止",
};

export function AgentPanel({ onCloseStart, onCloseEnd }: { onCloseStart?: () => void; onCloseEnd?: () => void }) {
  const agentStatus = useAgentStatus();
  const spawnAgent = useSpawnAgent();
  const providers = useQuery({
    queryKey: ["providers"],
    enabled: isDesktopShell(),
    queryFn: async () => (await callDesktop(IPC_COMMANDS.providerList, {})).providers,
  });
  const selectedProviderId = useWorkspaceStore((state) => state.selectedProviderId);
  const selectedModel = useWorkspaceStore((state) => state.selectedModel);
  const selectedServerId = useWorkspaceStore((state) => state.selectedServerId);
  const agentOpen = useWorkspaceStore((state) => state.agentOpen);
  const setAgentOpen = useWorkspaceStore((state) => state.setAgentOpen);
  const toggleAgent = useWorkspaceStore((state) => state.toggleAgent);
  const selectProvider = useWorkspaceStore((state) => state.selectProvider);
  const [entries, setEntries] = useState<Entry[]>([]);
  const [running, setRunning] = useState(false);
  const [runState, setRunState] = useState<string | null>(null);
  const [prompt, setPrompt] = useState("");
  const [runId, setRunId] = useState<string | null>(null);
  const [pendingApprovals, setPendingApprovals] = useState<string[]>([]);
  const [approvalDecisions, setApprovalDecisions] = useState<Record<string, string>>({});
  const approvalLocks = useRef(new Set<string>());
  const [listening, setListening] = useState(false);
  const [stopping, setStopping] = useState(false);
  const [runContext, setRunContext] = useState<string | null>(null);
  const [isClosing, setIsClosing] = useState(false);
  const hasBeenOpen = useRef(agentOpen);
  const feedRef = useRef<HTMLDivElement>(null);
  const followFeed = useRef(true);
  const servers = useServers();
  const focusServer = servers.data?.find((server) => server.id === selectedServerId);
  const selectedProvider = providers.data?.find((provider) => provider.id === selectedProviderId && provider.enabled);
  const setPrimary = useWorkspaceStore((state) => state.setPrimary);
  const shell = isDesktopShell();
  const agentRunning = agentStatus.data?.running === true;
  const canSend = shell && agentRunning && listening && Boolean(selectedProvider);

  useEffect(() => {
    if (agentOpen) {
      hasBeenOpen.current = true;
      setIsClosing(false);
    } else if (hasBeenOpen.current && !isClosing) {
      setIsClosing(true);
      onCloseStart?.();
    }
  }, [agentOpen, isClosing, onCloseStart]);

  useEffect(() => {
    if (!providers.data?.length) return;
    const current = providers.data.find((provider) => provider.id === selectedProviderId && provider.enabled);
    const fallback = providers.data.find((provider) => provider.enabled);
    if (!current && fallback) selectProvider(fallback.id, fallback.model);
  }, [providers.data, selectedProviderId, selectProvider]);

  // 流式文本按 run 累积（事件乱序也没关系，同 run 追加）。
  const lifecycle = useRef(new RunLifecycle());

  useEffect(() => {
    const feed = feedRef.current;
    if (agentOpen && followFeed.current && feed) feed.scrollTop = feed.scrollHeight;
  }, [entries, agentOpen]);

  useEffect(() => {
    if (!isDesktopShell()) return;
    const unlisteners: Array<() => void> = [];
    const registrations: Promise<void>[] = [];
    let disposed = false;
    const on = (name: string, handler: (payload: unknown) => void): void => {
      registrations.push(listen(name, (event) => {
        if (disposed) return;
        const parsed = AgentStreamEventSchema.safeParse(event.payload);
        if (parsed.success) handler(parsed.data);
      }).then((unlisten) => {
        if (disposed) {
          unlisten();
        } else {
          unlisteners.push(unlisten);
        }
      }));
    };

    const isActive = (payload: { runId: string }): boolean => lifecycle.current.isActive(payload.runId);

    on("agent.started", (payload) => {
      const event = payload as AgentStreamEvent;
      if (!lifecycle.current.started(event.runId)) return;
      setRunId(event.runId);
      setRunning(true);
      setRunState("thinking");
    });
    on("agent.thinking", (payload) => {
      const event = payload as AgentStreamEvent;
      if (!isActive(event)) return;
      const delta = "textDelta" in event && event.textDelta ? event.textDelta : "";
      setEntries((current) => {
        if (delta.length === 0) return current;
        const last = current.at(-1);
        if (last && last.kind === "assistant") {
          return appendEntries(current.slice(0, -1), [{ kind: "assistant", text: last.text + delta }]);
        }
        return appendEntries(current, [{ kind: "assistant", text: delta }]);
      });
    });
    on("agent.tool_call", (payload) => {
      const event = payload as Extract<AgentStreamEvent, { type: "agent.tool_call" }>;
      if (!isActive(event)) return;
      setRunState("running_tool");
      setEntries((current) => appendEntries(current, [
        { kind: "tool_call", toolName: event.toolName, target: targetLabel(event.target), riskLevel: event.riskLevel, decision: event.decision },
      ]));
    });
    on("agent.tool_result", (payload) => {
      const event = payload as Extract<AgentStreamEvent, { type: "agent.tool_result" }>;
      if (!isActive(event)) return;
      setRunState("thinking");
      const summary = event.toolName === "filesystem.read" ? "文件内容已返回给 Agent（正文不在动态中保存）" : event.outputSummary.slice(0, 240);
      setEntries((current) => appendEntries(current, [
        { kind: "tool_result", toolName: event.toolName, status: event.status, durationMs: event.durationMs, summary },
      ]));
    });
    on("agent.waiting_approval", (payload) => {
      const event = payload as Extract<AgentStreamEvent, { type: "agent.waiting_approval" }>;
      if (!isActive(event)) return;
      setRunState("waiting_approval");
      setAgentOpen(true);
      setEntries((current) => appendEntries(current, [{ kind: "approval", approval: event.approval }]));
    });
    on("agent.completed", (payload) => {
      const event = payload as Extract<AgentStreamEvent, { type: "agent.completed" }>;
      if (!lifecycle.current.finish(event.runId)) return;
      setRunning(false);
      setRunState(event.result.state);
      if (event.result.text) {
        setEntries((current) => {
          const last = current.at(-1);
          return last && last.kind === "assistant"
            ? [...current.slice(0, -1), { kind: "assistant", text: event.result.text }]
            : appendEntries(current, [{ kind: "assistant", text: event.result.text }]);
        });
      }
    });
    on("agent.failed", (payload) => {
      const event = payload as Extract<AgentStreamEvent, { type: "agent.failed" }>;
      if (!lifecycle.current.finish(event.runId)) return;
      setRunning(false);
      setRunState("failed");
      setEntries((current) => appendEntries(current, [{ kind: "error", text: event.error }]));
    });

    void Promise.all(registrations).then(() => {
      if (!disposed) setListening(true);
    }).catch((error) => {
      if (!disposed) setEntries([{ kind: "error", text: `无法接收 Agent 事件：${String(error)}` }]);
    });

    return () => {
      disposed = true;
      unlisteners.splice(0).forEach((unlisten) => unlisten());
    };
  }, []);

  const send = async (): Promise<void> => {
    const text = prompt.trim();
    if (!text || !canSend || !lifecycle.current.begin()) return;
    setRunning(true);
    setRunState("starting");
    setRunId(null);
    setRunContext(focusServer?.name ?? "全局工作区");
    setApprovalDecisions({});
    setPendingApprovals([]);
    approvalLocks.current.clear();
    followFeed.current = true;
    try {
      setPrompt("");
      setEntries([{ kind: "user", text }]);
      const messageId = `msg_${globalThis.crypto?.randomUUID?.() ?? `${Date.now()}_${Math.random().toString(36).slice(2)}`}`;
      const { runId: started } = await callDesktop(IPC_COMMANDS.agentRunStart, {
        sessionId: "ses_ui",
        prompt: text,
        messageId,
        parts: [{ type: "text", text }],
        delivery: "async",
        resume: true,
        providerId: selectedProviderId ?? undefined,
        model: selectedModel ?? undefined,
        focusServerId: selectedServerId ?? undefined,
      });
      if (lifecycle.current.acknowledge(started)) {
        setRunId(started);
        setRunState("thinking");
      }
    } catch (error) {
      lifecycle.current.fail();
      setRunning(false);
      setRunState("failed");
      setPrompt(text);
      setEntries([{ kind: "user", text }, { kind: "error", text: String(error) }]);
    }
  };

  const stop = async (): Promise<void> => {
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
  };

  const respondApproval = async (approval: ApprovalRequest, decision: "approve_once" | "approve_session" | "reject"): Promise<void> => {
    if (approvalLocks.current.has(approval.approvalId) || !lifecycle.current.isActive(approval.runId)) return;
    if (Date.parse(approval.expiresAt) <= Date.now()) {
      setApprovalDecisions((current) => ({ ...current, [approval.approvalId]: "审批已过期" }));
      return;
    }
    approvalLocks.current.add(approval.approvalId);
    setPendingApprovals((current) => [...current, approval.approvalId]);
    try {
      const { accepted } = await callDesktop(IPC_COMMANDS.agentApprovalRespond, {
        approvalId: approval.approvalId,
        runId: approval.runId,
        decision,
        respondedAt: new Date().toISOString(),
      });
      setApprovalDecisions((current) => ({ ...current, [approval.approvalId]: !accepted ? "审批已失效" : decision === "reject" ? "已拒绝" : decision === "approve_once" ? "已批准一次" : "已批准本次运行" }));
    } catch (error) {
      approvalLocks.current.delete(approval.approvalId);
      setEntries((current) => appendEntries(current, [{ kind: "error", text: String(error) }]));
    } finally {
      setPendingApprovals((current) => current.filter((id) => id !== approval.approvalId));
    }
  };

  const handleAgentToggle = (): void => {
    if (agentOpen) {
      setIsClosing(true);
      onCloseStart?.();
    }
    toggleAgent();
  };

  const panelClosing = !agentOpen && (isClosing || hasBeenOpen.current);
  const panelClass = panelClosing ? "agent-panel-closing" : !agentOpen ? "agent-panel-hidden" : "";

  return (
    <aside
      id="agent-panel"
      className={`agent-panel ${panelClass}`}
      aria-hidden={!agentOpen}
      onAnimationEnd={(event) => {
        if (event.animationName !== "panel-exit" || agentOpen || !isClosing) return;
        hasBeenOpen.current = false;
        setIsClosing(false);
        onCloseEnd?.();
      }}
    >
      <header className="agent-header">
        <div className="agent-title"><span className="agent-orb"><Icon name="agent" size={15} /></span><div><p className="eyebrow">自动化工作区</p><h2>Agent</h2></div></div>
        <div className="agent-header-actions">
          {running && runState ? (
            <span className="agent-status agent-status-active"><span className="status-pulse" />{RUN_STATE_LABEL[runState] ?? runState}</span>
          ) : (
            <span className={`agent-status ${agentRunning ? "agent-status-ready" : "agent-status-idle"}`}>
              {runState ? RUN_STATE_LABEL[runState] ?? runState : !shell ? "预览" : agentRunning ? "已就绪" : "未启动"}
            </span>
          )}
          <button type="button" className="icon-button agent-toggle" aria-label="收起 Agent 面板" title="收起 Agent 面板" aria-controls="agent-panel" aria-expanded={agentOpen} onClick={handleAgentToggle}>
            <Icon name="chevronRight" size={15} />
          </button>
        </div>
      </header>

      <div className="agent-context"><Icon name="servers" size={13} /><span title={runContext ?? focusServer?.name ?? "全局工作区"}>{running ? runContext : focusServer?.name ?? "全局工作区"}</span><small>{running ? "本次运行目标" : "当前上下文"}</small></div>
      <div ref={feedRef} className="agent-feed" onScroll={(event) => { const feed = event.currentTarget; followFeed.current = feed.scrollHeight - feed.scrollTop - feed.clientHeight < 64; }}>
        {entries.length === 0 ? (
          <div className="agent-empty"><span className="agent-empty-mark"><Icon name="sparkle" size={22} /></span><strong>直接问 Agent</strong><p>不添加服务器也能先问问题；需要远程操作时再选择目标。</p><div className="agent-suggestions">{["解释 Agent 能做什么", "帮我拆解一个排查计划"].map((suggestion) => <button type="button" key={suggestion} disabled={!canSend} onClick={() => setPrompt(suggestion)}>{suggestion}<Icon name="chevronRight" size={13} /></button>)}</div></div>
        ) : (
          entries.map((entry, index) => <EntryView key={index} entry={entry} onApproval={respondApproval} approvalBusy={entry.kind === "approval" && pendingApprovals.includes(entry.approval.approvalId)} approvalStatus={entry.kind === "approval" ? approvalDecisions[entry.approval.approvalId] ?? (!running ? "本次运行已结束" : undefined) : undefined} />)
        )}
        {!shell || !agentRunning || !selectedProvider ? (
          <p className="agent-notice" role="status" aria-live="polite">
            {!shell ? "启动 Yukinal 桌面应用后即可直接提问，无需先添加服务器。" : !agentRunning ? (
              <>
                <span>{agentStatus.data?.lastExit ? "Agent 已退出，可以重新启动。" : agentStatus.isError ? "无法读取 Agent 状态，可以尝试重新启动。" : "Agent 正在启动或尚未启动。"}</span>
                <button type="button" className="text-button" disabled={spawnAgent.isPending} onClick={() => spawnAgent.mutate()}>{spawnAgent.isPending ? "启动中…" : "启动 / 重试"}</button>
                {spawnAgent.isError ? <span className="agent-notice-error">{spawnAgent.error.message}</span> : null}
              </>
            ) : <>配置 AI Provider 后即可直接提问；不需要先添加服务器。<button type="button" className="text-button" onClick={() => setPrimary("settings")}>前往设置</button></>}
          </p>
        ) : null}
      </div>

      <footer className="agent-composer">
        {providers.data?.length ? (
          <div className="composer-meta">
            <select
              aria-label="选择 AI Provider"
              disabled={running}
              value={selectedProviderId ?? ""}
              onChange={(event) => {
                const provider = providers.data?.find((item) => item.id === event.target.value);
                if (provider) selectProvider(provider.id, provider.model);
              }}
              className="composer-select"
            >
              {providers.data.map((provider) => (
                <option key={provider.id} value={provider.id} disabled={!provider.enabled}>
                  {provider.label}{provider.enabled ? "" : "（已禁用）"}
                </option>
              ))}
            </select>
            {selectedModel ? <span className="composer-model">{selectedModel}</span> : null}
          </div>
        ) : null}
        <div className="composer-row">
          <textarea
            rows={2}
            aria-label="Agent 任务"
            value={prompt}
            disabled={!canSend || running}
            onChange={(event) => setPrompt(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === "Enter" && !event.shiftKey && !event.nativeEvent.isComposing && event.keyCode !== 229) { event.preventDefault(); void send(); }
            }}
            placeholder={running ? "运行中…" : "输入问题或任务（无需服务器也可以）"}
            className="composer-input"
          />
          {running ? (
            <button
              type="button"
              onClick={() => void stop()}
              disabled={!runId || stopping}
              className="composer-button composer-button-stop"
            >
              {stopping ? "停止中" : "停止"}
            </button>
          ) : (
            <button
              type="button"
              disabled={!canSend || !prompt.trim()}
              onClick={() => void send()}
              className="composer-button composer-button-send"
            >
              发送
            </button>
          )}
        </div>
        <p className="composer-hint">Enter 发送 <span>Shift + Enter 换行</span></p>
      </footer>
    </aside>
  );
}

function EntryView({
  entry,
  onApproval,
  approvalBusy,
  approvalStatus,
}: {
  entry: Entry;
  onApproval: (approval: ApprovalRequest, decision: "approve_once" | "approve_session" | "reject") => Promise<void>;
  approvalBusy: boolean;
  approvalStatus?: string;
}) {
  switch (entry.kind) {
    case "user":
      return <div className="agent-entry agent-entry-user"><span className="entry-label">你</span><div>{entry.text}</div></div>;
    case "assistant":
      return <div className="agent-entry agent-entry-assistant"><span className="entry-label">Agent</span><KeywordText text={entry.text || "…"} className="agent-entry-text" /></div>;
    case "tool_call":
      return (
        <div className="tool-card tool-card-call">
          <div className="tool-card-heading"><span className="tool-card-label">工具调用</span><span className="tool-name"><KeywordText text={entry.toolName} /></span></div>
          <div className="tool-card-meta"><code><KeywordText text={entry.target} /></code><span className={`risk-badge risk-${entry.riskLevel}`}>风险：{riskLabel(entry.riskLevel)}</span><span className={`decision-badge decision-${entry.decision}`}>{decisionLabel(entry.decision)}</span></div>
        </div>
      );
    case "tool_result":
      return (
        <div className={`tool-card tool-card-result tool-result-${entry.status}`}>
          <div className="tool-card-heading"><span className="tool-card-label">工具结果</span><span className="tool-name"><KeywordText text={entry.toolName} /></span><span className="tool-result-status">{resultLabel(entry.status)}</span></div>
          <code className="tool-result-copy"><KeywordText text={`${entry.durationMs}ms · ${entry.summary}`} /></code>
        </div>
      );
    case "error":
      return <div className="agent-error">{entry.text}</div>;
    case "approval":
      return (
        <div className="approval-card">
          <div className="approval-heading"><span className="approval-icon"><Icon name="warning" size={13} /></span><div><strong>需要审批</strong><code><KeywordText text={entry.approval.toolName} /></code></div></div>
          <p>{entry.approval.reason}</p>
          <p className="approval-target">{targetLabel(entry.approval.target)}</p>
          {approvalStatus ? <p className="approval-resolved" role="status">{approvalStatus}</p> : <div className="approval-actions">
            <button type="button" className="approval-button approval-button-reject" disabled={approvalBusy} onClick={() => void onApproval(entry.approval, "reject")}>
              拒绝
            </button>
            <button type="button" className="approval-button approval-button-approve" disabled={approvalBusy} onClick={() => void onApproval(entry.approval, "approve_once")}>
              批准一次
            </button>
            <button type="button" className="approval-button approval-button-approve" disabled={approvalBusy} onClick={() => void onApproval(entry.approval, "approve_session")}>
              本次运行批准
            </button>
          </div>}
        </div>
      );
  }
}

function decisionLabel(decision: "auto" | "ask" | "deny"): string {
  if (decision === "auto") return "自动批准";
  if (decision === "ask") return "需审批";
  return "策略禁止";
}

function targetLabel(target: { serverId?: string; environment: string; host: string }): string {
  return target.serverId ? `${target.serverId} · ${environmentLabel(target.environment as Environment)}` : `${target.host} · ${environmentLabel(target.environment as Environment)}`;
}

function environmentLabel(environment: Environment): string {
  const labels: Record<Environment, string> = {
    local: "本地环境",
    development: "开发环境",
    staging: "预发布环境",
    production: "生产环境",
    unknown: "未知环境",
  };
  return labels[environment] ?? environment;
}

function riskLabel(level: RiskLevel): string {
  const labels: Record<RiskLevel, string> = { read: "只读", low: "低", medium: "中", high: "高", critical: "严重" };
  return labels[level];
}

function resultLabel(status: "success" | "failed" | "cancelled"): string {
  return status === "success" ? "成功" : status === "cancelled" ? "已取消" : "失败";
}
