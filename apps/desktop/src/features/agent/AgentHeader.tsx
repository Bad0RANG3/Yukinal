/**
 * 面板头部：当前运行状态、以及「对话记录 / 收起」两个入口。
 *
 * 状态文案的唯一职责是回答「Agent 现在在干什么」，因此它把运行态
 * 放在存活态之上：运行中显示正在进行的步骤，空闲时才回落到就绪与否。
 */

import type { ReactNode, RefObject } from "react";

import type { AgentContextUsage, AgentStage, AgentTokenUsage } from "@yukinal/shared";

import { Icon } from "../../components/Icon.js";
import { runStateLabel } from "./transcript.js";
import { agentStageLabel } from "./stage-labels.js";
import { REASONING_LABELS } from "./reasoning.js";
import { AgentUsageDetails } from "./AgentUsageDetails.js";

export function AgentHeader({
  running,
  runState,
  stage = null,
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
  /** Derived operating stage; omitted callers keep the run-state label. */
  stage?: AgentStage | null;
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
        {showStatus ? <AgentRunStatus running={running} runState={runState} stage={stage} tokenUsage={tokenUsage} contextUsage={contextUsage} agentReady={agentReady} shell={shell} /> : null}
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

/** Shared diagnostics for the header and the collapsed conversation settings. */
export function AgentRunStatus({ running, runState, stage = null, tokenUsage, contextUsage, agentReady, shell, compact = false, preview = false }: {
  running: boolean;
  runState: string | null;
  /** Derived operating stage; shown only while it says more than the run state. */
  stage?: AgentStage | null;
  tokenUsage: AgentTokenUsage | null;
  contextUsage?: AgentContextUsage | null;
  agentReady: boolean;
  shell: boolean;
  compact?: boolean;
  preview?: boolean;
}) {
  // A visible stage is more informative than the generic "thinking…" while the
  // run is working, and it is derived from executed results rather than prose.
  const label = (running ? agentStageLabel(stage) : null) ?? runStateLabel(runState);
  const usageLabel = tokenUsage ? tokenUsage.reportedRequests === 0 ? "用量未上报" : `${tokenUsage.unreportedRequests ? "≥ " : ""}${(tokenUsage.inputTokens + tokenUsage.outputTokens).toLocaleString()} tokens` : "";
  const lowerBound = tokenUsage?.unreportedRequests ? "≥ " : "";
  const statusLabel = preview ? "模拟数据 · 发送已停用" : label ?? (!shell ? "预览" : agentReady ? "已就绪" : "未启动");
  return <>
    {compact ? <span className="agent-status is-icon" role="status" aria-label={statusLabel} title={statusLabel}><Icon name={preview ? "agent" : running ? "refresh" : agentReady ? "connect" : "disconnect"} size="sm" /></span> : !preview && running && label ? (
      <span className="agent-status agent-status-active" role="status"><span className="status-pulse" />{label}</span>
    ) : (
      <span className={`agent-status ${!preview && agentReady ? "agent-status-ready" : "agent-status-idle"}`} data-idle={!runState || undefined} role="status">
        {statusLabel}
      </span>
    )}
    {contextUsage || tokenUsage ? <AgentUsageDetails className={contextUsage ? "agent-context-usage" : "agent-token-usage"} trigger={contextUsage ? <>
          {compact ? <Icon name="archive" size="xs" /> : <span className="agent-context-caption">上下文 </span>}
          <span>{contextUsage.includedTurns}/{contextUsage.totalTurns}</span>
          {!compact && contextUsage.compactedTurns ? <span className="agent-context-compacted"> · 已压缩</span> : null}
        </> : usageLabel}>
        <strong>上下文与用量</strong>
        <dl className="agent-context-facts">
          {contextUsage ? <>
            <div><dt>历史记录</dt><dd>保留 {contextUsage.includedTurns} / {contextUsage.totalTurns} 条</dd></div>
            <div><dt>历史压缩</dt><dd>已压缩 {contextUsage.compactedTurns} 条</dd></div>
            <div><dt>输入文本预算</dt><dd>{(contextUsage.inputBytes / 1024).toFixed(1)} / {(contextUsage.maxInputBytes / 1024).toFixed(0)} KiB</dd></div>
          </> : null}
          <div><dt>本次运行模型用量</dt><dd>{tokenUsage && tokenUsage.reportedRequests !== 0
            ? <>输入 {lowerBound}{tokenUsage.inputTokens.toLocaleString()} tokens；输出 {lowerBound}{tokenUsage.outputTokens.toLocaleString()} tokens</>
            : "模型用量尚未上报"}</dd></div>
          {tokenUsage?.reportedRequests !== undefined ? <div><dt>已上报请求</dt><dd>{tokenUsage.reportedRequests} 次</dd></div> : null}
          {tokenUsage?.unreportedRequests ? <div><dt>缺失用量</dt><dd>{tokenUsage.unreportedRequests} 次模型请求未上报用量，累计不含这些请求。</dd></div> : null}
          {tokenUsage?.lastRequest?.effort ? <div><dt>本次推理</dt><dd>{tokenUsage.lastRequest.routing === "auto" ? "自动 · " : ""}{REASONING_LABELS[tokenUsage.lastRequest.effort]}</dd></div> : null}
          {tokenUsage?.lastRequest?.firstAnswerMs !== undefined ? <div><dt>最近请求首段正文</dt><dd>{(tokenUsage.lastRequest.firstAnswerMs / 1000).toFixed(2)} 秒</dd></div> : null}
          {tokenUsage?.lastRequest ? <div><dt>最近请求耗时</dt><dd>{(tokenUsage.lastRequest.durationMs / 1000).toFixed(2)} 秒</dd></div> : null}
          {tokenUsage?.reasoningTokens !== undefined ? <div><dt>已上报推理用量</dt><dd>{tokenUsage.reasoningTokens.toLocaleString()} tokens · {tokenUsage.reasoningReportedRequests ?? 0} 次请求</dd></div> : null}
          {tokenUsage?.cacheReadTokens !== undefined ? <div><dt>已上报缓存命中</dt><dd>{tokenUsage.cacheReadTokens.toLocaleString()} tokens · {tokenUsage.cacheReportedRequests ?? 0} 次请求</dd></div> : null}
          {contextUsage?.memorySources?.length ? <div><dt>使用记忆</dt><dd>{contextUsage.memorySources.map((source) => source.title).join("、")}</dd></div> : null}
        </dl>
        <p className="agent-context-note">KiB 为文本大小；token 用量由模型上报。</p>
    </AgentUsageDetails> : null}
  </>;
}
