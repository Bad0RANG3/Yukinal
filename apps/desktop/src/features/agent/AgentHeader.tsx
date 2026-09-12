/**
 * 面板头部：标题、当前运行状态、以及「对话记录 / 收起」两个入口。
 *
 * 状态文案的唯一职责是回答「Agent 现在在干什么」，因此它把运行态
 * 放在存活态之上：运行中显示正在进行的步骤，空闲时才回落到就绪与否。
 */

import type { RefObject } from "react";

import { Icon } from "../../components/Icon.js";
import { runStateLabel } from "./transcript.js";

export function AgentHeader({
  running,
  runState,
  agentReady,
  agentOpen,
  shell,
  historyOpen,
  historyButtonRef,
  onToggleHistory,
  onToggle,
  closeButtonRef,
}: {
  running: boolean;
  runState: string | null;
  agentReady: boolean;
  agentOpen: boolean;
  shell: boolean;
  historyOpen: boolean;
  /** 记录视图关闭时把焦点还给这个按钮，键盘用户不会掉到 body 上。 */
  historyButtonRef: RefObject<HTMLButtonElement | null>;
  onToggleHistory: () => void;
  onToggle: () => void;
  closeButtonRef: RefObject<HTMLButtonElement | null>;
}) {
  const label = runStateLabel(runState);

  return (
    <header className="agent-header">
      <div className="agent-title">
        <span className="agent-orb"><Icon name="agent" size="md" /></span>
        <div><p className="eyebrow">自动化工作区</p><h2>Agent</h2></div>
      </div>
      <div className="agent-header-actions">
        {running && label ? (
          <span className="agent-status agent-status-active"><span className="status-pulse" />{label}</span>
        ) : (
          <span className={`agent-status ${agentReady ? "agent-status-ready" : "agent-status-idle"}`}>
            {label ?? (!shell ? "预览" : agentReady ? "已就绪" : "未启动")}
          </span>
        )}
        <button
          ref={historyButtonRef}
          type="button"
          className={`icon-button agent-history-toggle ${historyOpen ? "is-active" : ""}`}
          aria-label="打开对话记录"
          title={shell ? "打开对话记录" : "桌面应用中可查询对话记录"}
          disabled={!shell || running}
          aria-pressed={historyOpen}
          onClick={onToggleHistory}
        >
          <Icon name="logs" size="md" />
        </button>
        <button
          ref={closeButtonRef}
          type="button"
          className="icon-button agent-toggle"
          aria-label="收起 Agent 面板"
          title="收起 Agent 面板"
          aria-controls="agent-panel"
          aria-expanded={agentOpen}
          onClick={onToggle}
        >
          <Icon name="chevronRight" size="md" />
        </button>
      </div>
    </header>
  );
}
