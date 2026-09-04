/**
 * Agent 面板 —— 真实接线：输入一句话，Rust 解析 provider + 凭据 → sidecar
 * `agent.run.start` → 事件流回 UI（thinking / 工具卡片 / 审批 / completed）。
 *
 * 规则：不造假 transcript。没有运行中的 run 就没有消息；Stop 立刻掐断在途请求。
 */

import { IPC_COMMANDS, type AgentStreamEvent, type ApprovalRequest } from "@yukinal/shared";
import { listen } from "@tauri-apps/api/event";
import { useQuery } from "@tanstack/react-query";
import { useEffect, useRef, useState } from "react";

import { callDesktop, isDesktopShell } from "../../lib/ipc.js";
import { useAgentStatus } from "../../lib/runtime.js";
import { useWorkspaceStore } from "../../stores/workspace-store.js";

type Entry =
  | { kind: "user"; text: string }
  | { kind: "assistant"; text: string }
  | { kind: "tool_call"; text: string }
  | { kind: "tool_result"; text: string }
  | { kind: "approval"; approval: ApprovalRequest }
  | { kind: "error"; text: string };

const RUN_STATE_LABEL: Record<string, string> = {
  thinking: "思考中…",
  running_tool: "执行工具…",
  waiting_approval: "等待审批…",
  completed: "完成",
  failed: "失败",
  cancelled: "已停止",
};

export function AgentPanel() {
  const agentStatus = useAgentStatus();
  const providers = useQuery({
    queryKey: ["providers"],
    enabled: isDesktopShell(),
    queryFn: async () => (await callDesktop(IPC_COMMANDS.providerList, {})).providers,
  });
  const selectedProviderId = useWorkspaceStore((state) => state.selectedProviderId);
  const selectedModel = useWorkspaceStore((state) => state.selectedModel);
  const selectedServerId = useWorkspaceStore((state) => state.selectedServerId);
  const selectProvider = useWorkspaceStore((state) => state.selectProvider);
  const [entries, setEntries] = useState<Entry[]>([]);
  const [running, setRunning] = useState(false);
  const [runState, setRunState] = useState<string | null>(null);
  const [prompt, setPrompt] = useState("");
  const [runId, setRunId] = useState<string | null>(null);
  const [pendingApprovals, setPendingApprovals] = useState<string[]>([]);

  useEffect(() => {
    if (!providers.data?.length) return;
    const current = providers.data.find((provider) => provider.id === selectedProviderId);
    const fallback = providers.data.find((provider) => provider.enabled) ?? providers.data[0];
    if (!current && fallback) selectProvider(fallback.id, fallback.model);
  }, [providers.data, selectedProviderId, selectProvider]);

  // 流式文本按 run 累积（事件乱序也没关系，同 run 追加）。
  const activeRunRef = useRef<string | null>(null);

  useEffect(() => {
    if (!isDesktopShell()) return;
    const unlisteners: Array<() => void> = [];
    let disposed = false;
    const on = (name: string, handler: (payload: unknown) => void): void => {
      void listen(name, (event) => handler(event.payload)).then((unlisten) => {
        if (disposed) {
          unlisten();
        } else {
          unlisteners.push(unlisten);
        }
      });
    };

    const isActive = (payload: { runId?: string }): boolean => activeRunRef.current !== null && payload.runId === activeRunRef.current;

    on("agent.started", (payload) => {
      const event = payload as AgentStreamEvent;
      activeRunRef.current = event.runId;
      setRunId(event.runId);
      setRunning(true);
      setRunState("thinking");
      // Keep the user's prompt visible. Clearing it here made the transcript
      // jump as soon as the Rust event arrived after `agent_run_start`.
      setEntries((current) => current);
    });
    on("agent.thinking", (payload) => {
      const event = payload as AgentStreamEvent;
      if (!isActive(event)) return;
      const delta = "textDelta" in event && event.textDelta ? event.textDelta : "";
      setEntries((current) => {
        if (delta.length === 0) return current;
        const last = current.at(-1);
        if (last && last.kind === "assistant") {
          return [...current.slice(0, -1), { kind: "assistant", text: last.text + delta }];
        }
        return [...current, { kind: "assistant", text: delta }];
      });
    });
    on("agent.tool_call", (payload) => {
      const event = payload as Extract<AgentStreamEvent, { type: "agent.tool_call" }>;
      if (!isActive(event)) return;
      setRunState("running_tool");
      setEntries((current) => [
        ...current,
        { kind: "tool_call", text: `➤ ${event.toolName}` },
      ]);
    });
    on("agent.tool_result", (payload) => {
      const event = payload as Extract<AgentStreamEvent, { type: "agent.tool_result" }>;
      if (!isActive(event)) return;
      setRunState("thinking");
      const marker = event.status === "success" ? "✓" : event.status === "cancelled" ? "✗(已取消)" : "✗";
      setEntries((current) => [
        ...current,
        { kind: "tool_result", text: `${marker} ${event.toolName}: ${event.outputSummary.slice(0, 240)}` },
      ]);
    });
    on("agent.waiting_approval", (payload) => {
      const event = payload as Extract<AgentStreamEvent, { type: "agent.waiting_approval" }>;
      if (!isActive(event)) return;
      setRunState("waiting_approval");
      setEntries((current) => [...current, { kind: "approval", approval: event.approval }]);
    });
    on("agent.completed", (payload) => {
      const event = payload as Extract<AgentStreamEvent, { type: "agent.completed" }>;
      if (!isActive(event)) return;
      setRunning(false);
      setRunState(event.result.state);
      if (event.result.text) {
        setEntries((current) => {
          const last = current.at(-1);
          return last && last.kind === "assistant"
            ? [...current.slice(0, -1), { kind: "assistant", text: event.result.text }]
            : [...current, { kind: "assistant", text: event.result.text }];
        });
      }
    });
    on("agent.failed", (payload) => {
      const event = payload as Extract<AgentStreamEvent, { type: "agent.failed" }>;
      if (!isActive(event)) return;
      setRunning(false);
      setRunState("failed");
      setEntries((current) => [...current, { kind: "error", text: event.error }]);
    });

    return () => {
      disposed = true;
      unlisteners.splice(0).forEach((unlisten) => unlisten());
    };
  }, []);

  const send = async (): Promise<void> => {
    const text = prompt.trim();
    if (!text || running) return;
    try {
      setPrompt("");
      setEntries([{ kind: "user", text }]);
      const { runId: started } = await callDesktop(IPC_COMMANDS.agentRunStart, {
        sessionId: "ses_ui",
        prompt: text,
        providerId: selectedProviderId ?? undefined,
        model: selectedModel ?? undefined,
        focusServerId: selectedServerId ?? undefined,
      });
      activeRunRef.current = started;
      setRunId(started);
      setRunning(true);
      setRunState("thinking");
    } catch (error) {
      setEntries([{ kind: "user", text }, { kind: "error", text: String(error) }]);
    }
  };

  const stop = async (): Promise<void> => {
    if (!runId) return;
    await callDesktop(IPC_COMMANDS.agentRunStop, { runId }).catch(() => {});
  };

  const respondApproval = async (approval: ApprovalRequest, decision: "approve_once" | "approve_session" | "reject"): Promise<void> => {
    if (pendingApprovals.includes(approval.approvalId)) return;
    setPendingApprovals((current) => [...current, approval.approvalId]);
    try {
      await callDesktop(IPC_COMMANDS.agentApprovalRespond, {
        approvalId: approval.approvalId,
        decision,
        respondedAt: new Date().toISOString(),
      });
    } catch (error) {
      setEntries((current) => [...current, { kind: "error", text: String(error) }]);
    } finally {
      setPendingApprovals((current) => current.filter((id) => id !== approval.approvalId));
    }
  };

  const shell = isDesktopShell();
  const agentRunning = agentStatus.data?.running === true;

  return (
    <aside className="agent-panel">
      <header className="agent-header">
        <div className="agent-title"><span className="agent-orb">✦</span><div><p className="eyebrow">自动化工作区</p><h2>Agent</h2></div></div>
        {running && runState ? (
          <span className="agent-status agent-status-active"><span className="status-pulse" />{RUN_STATE_LABEL[runState] ?? runState}</span>
        ) : (
          <span className={`agent-status ${agentRunning ? "agent-status-ready" : "agent-status-idle"}`}>
            {agentRunning ? "agent 运行中" : "agent 未启动"}
          </span>
        )}
      </header>

      <div className="agent-feed">
        {entries.length === 0 ? (
          <div className="agent-empty"><span className="agent-empty-mark">✦</span><strong>从目标开始</strong><p>例如：为什么 staging API 一直在重启？</p></div>
        ) : (
          entries.map((entry, index) => <EntryView key={index} entry={entry} onApproval={respondApproval} approvalBusy={entry.kind === "approval" && pendingApprovals.includes(entry.approval.approvalId)} />)
        )}
        {!shell || !agentRunning ? (
          <p className="agent-notice">
            {!shell ? "浏览器预览：无法触达 Rust 核心。" : "先启动 agent（左下角），再配置 provider（设置 ▸ Provider）。"}
          </p>
        ) : null}
      </div>

      <footer className="agent-composer">
        {providers.data?.length ? (
          <div className="composer-meta">
            <select
              aria-label="AI Provider"
              value={selectedProviderId ?? ""}
              onChange={(event) => {
                const provider = providers.data?.find((item) => item.id === event.target.value);
                if (provider) selectProvider(provider.id, provider.model);
              }}
              className="composer-select"
            >
              {providers.data.map((provider) => (
                <option key={provider.id} value={provider.id} disabled={!provider.enabled}>
                  {provider.label}{provider.enabled ? "" : " (disabled)"}
                </option>
              ))}
            </select>
            {selectedModel ? <span className="composer-model">{selectedModel}</span> : null}
          </div>
        ) : null}
        <div className="composer-row">
          <input
            value={prompt}
            disabled={!shell || !agentRunning || running}
            onChange={(event) => setPrompt(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === "Enter" && !event.shiftKey) void send();
            }}
            placeholder={running ? "运行中…" : "输入你想让 agent 做的事"}
            className="composer-input"
          />
          {running ? (
            <button
              type="button"
              onClick={() => void stop()}
              className="composer-button composer-button-stop"
            >
              停止
            </button>
          ) : (
            <button
              type="button"
              disabled={!prompt.trim()}
              onClick={() => void send()}
              className="composer-button composer-button-send"
            >
              发送
            </button>
          )}
        </div>
      </footer>
    </aside>
  );
}

function EntryView({
  entry,
  onApproval,
  approvalBusy,
}: {
  entry: Entry;
  onApproval: (approval: ApprovalRequest, decision: "approve_once" | "approve_session" | "reject") => Promise<void>;
  approvalBusy: boolean;
}) {
  switch (entry.kind) {
    case "user":
      return <div className="agent-entry agent-entry-user"><span className="entry-label">你</span><div>{entry.text}</div></div>;
    case "assistant":
      return <div className="agent-entry agent-entry-assistant"><span className="entry-label">Agent</span><div className="whitespace-pre-wrap">{entry.text || "…"}</div></div>;
    case "tool_call":
      return <div className="tool-card tool-card-call"><span className="tool-card-label">工具调用</span><code>{entry.text}</code></div>;
    case "tool_result":
      return <div className="tool-card tool-card-result"><span className="tool-card-label">结果</span><code>{entry.text}</code></div>;
    case "error":
      return <div className="agent-error">{entry.text}</div>;
    case "approval":
      return (
        <div className="approval-card">
          <div className="approval-heading"><span className="approval-icon">!</span><div><strong>需要审批</strong><code>{entry.approval.toolName}</code></div></div>
          <p>{entry.approval.reason}</p>
          <div className="approval-actions">
            <button type="button" className="approval-button approval-button-reject" disabled={approvalBusy} onClick={() => void onApproval(entry.approval, "reject")}>
              拒绝
            </button>
            <button type="button" className="approval-button approval-button-approve" disabled={approvalBusy} onClick={() => void onApproval(entry.approval, "approve_once")}>
              批准一次
            </button>
            <button type="button" className="approval-button approval-button-approve" disabled={approvalBusy} onClick={() => void onApproval(entry.approval, "approve_session")}>
              本次会话批准
            </button>
          </div>
        </div>
      );
  }
}
