/**
 * Agent 面板 —— 真实接线：输入一句话，Rust 解析 provider + 凭据 → sidecar
 * `agent.run.start` → 事件流回 UI（thinking / 工具卡片 / 审批 / completed）。
 *
 * 规则：不造假 transcript。没有运行中的 run 就没有消息；Stop 立刻掐断在途请求。
 */

import { IPC_COMMANDS, type AgentStreamEvent, type ApprovalRequest } from "@yukinal/shared";
import { listen } from "@tauri-apps/api/event";
import { useEffect, useRef, useState } from "react";

import { callDesktop, isDesktopShell } from "../../lib/ipc.js";
import { useAgentStatus } from "../../lib/runtime.js";

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
  const [entries, setEntries] = useState<Entry[]>([]);
  const [running, setRunning] = useState(false);
  const [runState, setRunState] = useState<string | null>(null);
  const [prompt, setPrompt] = useState("");
  const [runId, setRunId] = useState<string | null>(null);

  // 流式文本按 run 累积（事件乱序也没关系，同 run 追加）。
  const activeRunRef = useRef<string | null>(null);

  useEffect(() => {
    if (!isDesktopShell()) return;
    const unlisteners: Array<() => void> = [];
    const on = (name: string, handler: (payload: unknown) => void): void => {
      listen(name, (event) => handler(event.payload)).then((unlisten) => unlisteners.push(unlisten));
    };

    const isActive = (payload: { runId?: string }): boolean => activeRunRef.current !== null && payload.runId === activeRunRef.current;

    on("agent.started", (payload) => {
      const event = payload as AgentStreamEvent;
      activeRunRef.current = event.runId;
      setRunId(event.runId);
      setRunning(true);
      setRunState("thinking");
      setEntries([]);
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

    return () => unlisteners.forEach((unlisten) => unlisten());
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
    await callDesktop(IPC_COMMANDS.agentApprovalRespond, {
      approvalId: approval.approvalId,
      decision,
      respondedAt: new Date().toISOString(),
    }).catch((error) => {
      setEntries((current) => [...current, { kind: "error", text: String(error) }]);
    });
  };

  const shell = isDesktopShell();
  const agentRunning = agentStatus.data?.running === true;

  return (
    <aside className="flex w-96 shrink-0 flex-col bg-zinc-950/60">
      <header className="flex items-center justify-between border-b border-zinc-800 px-3 py-2">
        <span className="text-sm font-medium">Agent</span>
        {running && runState ? (
          <span className="text-xs text-zinc-400">{RUN_STATE_LABEL[runState] ?? runState}</span>
        ) : (
          <span className="text-xs text-zinc-500">
            {agentRunning ? "agent 运行中" : "agent 未启动"}
          </span>
        )}
      </header>

      <div className="flex-1 space-y-2 overflow-auto p-3 text-sm">
        {entries.length === 0 ? (
          <p className="text-zinc-500">用目标提问，而不是命令：“为什么 staging API 在重启？”</p>
        ) : (
          entries.map((entry, index) => <EntryView key={index} entry={entry} onApproval={respondApproval} />)
        )}
        {!shell || !agentRunning ? (
          <p className="rounded border border-amber-500/40 bg-amber-500/5 p-2 text-xs text-amber-200">
            {!shell ? "浏览器预览：无法触达 Rust 核心。" : "先启动 agent（左下角），再配置 provider（设置 ▸ Provider）。"}
          </p>
        ) : null}
      </div>

      <footer className="space-y-2 border-t border-zinc-800 p-3">
        <div className="flex gap-2">
          <input
            value={prompt}
            disabled={!shell || !agentRunning || running}
            onChange={(event) => setPrompt(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === "Enter" && !event.shiftKey) void send();
            }}
            placeholder={running ? "运行中…" : "输入你想让 agent 做的事"}
            className="w-full rounded-md border border-zinc-800 bg-zinc-900 px-2 py-1.5 text-sm outline-none placeholder:text-zinc-600 focus:border-zinc-600 disabled:opacity-50"
          />
          {running ? (
            <button
              type="button"
              onClick={() => void stop()}
              className="rounded-md border border-rose-700 px-3 py-1.5 text-sm text-rose-300"
            >
              停止
            </button>
          ) : (
            <button
              type="button"
              disabled={!prompt.trim()}
              onClick={() => void send()}
              className="rounded-md bg-zinc-100 px-3 py-1.5 text-sm font-medium text-zinc-900 disabled:opacity-40"
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
}: {
  entry: Entry;
  onApproval: (approval: ApprovalRequest, decision: "approve_once" | "approve_session" | "reject") => Promise<void>;
}) {
  switch (entry.kind) {
    case "user":
      return <div className="rounded-md bg-zinc-900 px-2 py-1.5">{entry.text}</div>;
    case "assistant":
      return <div className="whitespace-pre-wrap text-zinc-200">{entry.text || "…"}</div>;
    case "tool_call":
      return <div className="rounded-md border border-zinc-800 px-2 py-1.5 font-mono text-xs text-zinc-300">{entry.text}</div>;
    case "tool_result":
      return <div className="rounded-md border border-zinc-800 px-2 py-1.5 font-mono text-xs text-zinc-500">{entry.text}</div>;
    case "error":
      return <div className="rounded-md border border-rose-500/40 px-2 py-1.5 text-xs text-rose-300">{entry.text}</div>;
    case "approval":
      return (
        <div className="rounded-md border border-amber-500/40 bg-amber-500/5 p-2 text-xs">
          <div className="mb-1 font-medium text-amber-200">需要审批：{entry.approval.toolName}</div>
          <p className="mb-2 text-amber-200/70">{entry.approval.reason}</p>
          <div className="flex gap-1.5">
            <button type="button" className="rounded border border-zinc-600 px-2 py-0.5 text-zinc-200" onClick={() => void onApproval(entry.approval, "reject")}>
              拒绝
            </button>
            <button type="button" className="rounded bg-amber-400 px-2 py-0.5 text-zinc-900" onClick={() => void onApproval(entry.approval, "approve_once")}>
              批准一次
            </button>
            <button type="button" className="rounded bg-amber-400 px-2 py-0.5 text-zinc-900" onClick={() => void onApproval(entry.approval, "approve_session")}>
              本次会话批准
            </button>
          </div>
        </div>
      );
  }
}