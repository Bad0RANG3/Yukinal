/**
 * 面板头部：当前运行状态、以及「对话记录 / 收起」两个入口。
 *
 * 状态文案的唯一职责是回答「Agent 现在在干什么」，因此它把运行态
 * 放在存活态之上：运行中显示正在进行的步骤，空闲时才回落到就绪与否。
 */

import type { ReactNode, RefObject } from "react";

import type { AgentContextUsage, AgentTokenUsage } from "@yukinal/shared";

import { Icon } from "../../components/Icon.js";
import { runStateLabel } from "./transcript.js";

export function AgentHeader({
  running,
  runState,
  tokenUsage,
  agentReady,
  agentOpen,
  shell,
  historyOpen,
  historyButtonRef,
  onToggleHistory,
  onToggle,
  closeButtonRef,
  workspace = false,
  workspaceControls,
  context,
  taskTitle = "新任务",
  contextUsage,
  showStatus = true,
}: {
  running: boolean;
  runState: string | null;
  tokenUsage: AgentTokenUsage | null;
  agentReady: boolean;
  agentOpen: boolean;
  shell: boolean;
  historyOpen: boolean;
  /** 记录视图关闭时把焦点还给这个按钮，键盘用户不会掉到 body 上。 */
  historyButtonRef: RefObject<HTMLButtonElement | null>;
  onToggleHistory: () => void;
  onToggle: () => void;
  closeButtonRef: RefObject<HTMLButtonElement | null>;
  workspace?: boolean;
  workspaceControls?: ReactNode;
  context?: ReactNode;
  taskTitle?: string;
  contextUsage?: AgentContextUsage | null;
  showStatus?: boolean;
}) {
  return (
    <header className="agent-header">
      <div className="agent-task-heading">
        <button
          ref={historyButtonRef}
          type="button"
          className={`agent-task-trigger ${historyOpen ? "is-active" : ""}`}
          aria-label={`切换任务：${taskTitle}`}
          title={running ? "运行中，结束后可切换任务" : "最近对话与搜索"}
          aria-disabled={running || undefined}
          aria-expanded={historyOpen}
          aria-haspopup="dialog"
          onClick={() => { if (!running) onToggleHistory(); }}
        >
          <span>{taskTitle}</span><Icon name="chevronDown" size="sm" />
        </button>
        {context}
      </div>
      <div className="agent-header-actions">
        {showStatus ? <AgentRunStatus running={running} runState={runState} tokenUsage={tokenUsage} contextUsage={contextUsage} agentReady={agentReady} shell={shell} /> : null}
        {workspaceControls ? <div className="agent-workspace-controls">{workspaceControls}</div> : null}
        {!workspace ? <button
          ref={closeButtonRef}
          type="button"
          className="icon-button agent-toggle"
          aria-label="收起对话"
          title="收起对话"
          aria-controls="agent-panel"
          aria-expanded={agentOpen}
          onClick={onToggle}
        >
          <Icon name="chevronRight" size="md" />
        </button> : null}
      </div>
    </header>
  );
}

/** Shared status content can live inside the composer without duplicating state. */
export function AgentRunStatus({ running, runState, tokenUsage, contextUsage, agentReady, shell, compact = false, preview = false }: {
  running: boolean;
  runState: string | null;
  tokenUsage: AgentTokenUsage | null;
  contextUsage?: AgentContextUsage | null;
  agentReady: boolean;
  shell: boolean;
  compact?: boolean;
  preview?: boolean;
}) {
  const label = runStateLabel(runState);
  const statusLabel = preview ? "模拟数据 · 发送已停用" : label ?? (!shell ? "预览" : agentReady ? "已就绪" : "未启动");
  return <>
    {compact ? <span className="agent-status is-icon" role="status" aria-label={statusLabel} title={statusLabel}><Icon name={preview ? "agent" : running ? "refresh" : agentReady ? "connect" : "disconnect"} size="sm" /></span> : running && label ? (
      <span className="agent-status agent-status-active" role="status"><span className="status-pulse" />{label}</span>
    ) : (
      <span className={`agent-status ${agentReady ? "agent-status-ready" : "agent-status-idle"}`} data-idle={!runState || undefined} role="status">
        {label ?? (!shell ? "预览" : agentReady ? "已就绪" : "未启动")}
      </span>
    )}
    {contextUsage ? <span className="agent-context-usage" title={`历史上下文：保留 ${contextUsage.includedTurns} / ${contextUsage.totalTurns} 条，已压缩 ${contextUsage.compactedTurns} 条；输入 ${(contextUsage.inputBytes / 1024).toFixed(1)} / ${(contextUsage.maxInputBytes / 1024).toFixed(0)} KiB${tokenUsage ? `；本次运行输入 ${tokenUsage.inputTokens.toLocaleString()}，输出 ${tokenUsage.outputTokens.toLocaleString()} tokens` : ""}${contextUsage.memorySources?.length ? `；使用记忆：${contextUsage.memorySources.map((source) => source.title).join("、")}` : ""}`}>
      {compact ? <Icon name="archive" size="xs" /> : <span className="agent-context-caption">上下文 </span>}<span>{contextUsage.includedTurns}/{contextUsage.totalTurns}</span>{!compact && contextUsage.compactedTurns ? <span className="agent-context-compacted"> · 已压缩</span> : null}
    </span> : null}
    {tokenUsage && !contextUsage ? <span className="agent-token-usage" title={`本次运行累计：输入 ${tokenUsage.inputTokens.toLocaleString()} tokens，输出 ${tokenUsage.outputTokens.toLocaleString()} tokens`}>
      {(tokenUsage.inputTokens + tokenUsage.outputTokens).toLocaleString()} tokens
    </span> : null}
  </>;
}
