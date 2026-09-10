/**
 * 底部输入区：一句话 + 一次发送，外加两个真实的输入加速器。
 *
 * 有意保持「单卡片」的克制：高级任务配置不在主路径上。这里只负责呈现、
 * 键盘行为、输入框伸缩，以及 `@` / `/` 候选列表的交互。
 *
 * 候选列表的全部机制（触发词解析、过滤、替换、高亮、键盘导航）都留在这个
 * 模块内部，对外只暴露「有哪些服务器可提及」和一个「执行了这个命令」回调 ——
 * 这样面板不必知道弹窗的存在，而候选逻辑本身已在 composer-triggers 里被单独测试。
 *
 * 尚未接入的能力（语音）一律呈现为 disabled 并写明原因，
 * 不留「看起来能点、点了没反应」的假入口。
 */

import { useLayoutEffect, useRef, useState, type KeyboardEvent } from "react";

import type { AgentPermissionMode, AgentRunMode } from "@yukinal/shared";

import { Icon } from "../../components/Icon.js";
import {
  findActiveTrigger,
  matchCommands,
  matchMentions,
  replaceTrigger,
  resolveSubmission,
  type ActiveTrigger,
  type CommandContext,
  type CommandSpec,
  type MentionCandidate,
  type Submission,
} from "./composer-triggers.js";
import { APPROVAL_OPTIONS, APPROVAL_ORDER, RUN_MODE_ORDER, RUN_MODE_SPECS, approvalOptionSpec, runModeSpec } from "./run-mode.js";

/** 候选列表里的一项：命令与提及共用同一套渲染与键盘导航。 */
type Suggestion =
  | { kind: "command"; key: string; spec: CommandSpec; reason: string | null }
  | { kind: "mention"; key: string; candidate: MentionCandidate };

export function AgentComposer({
  prompt,
  onPromptChange,
  onSubmit,
  onStop,
  running,
  stopping,
  canSend,
  canStop,
  permissionMode,
  onPermissionModeChange,
  runMode,
  onRunModeChange,
  models,
  selectedModelKey,
  selectedModelLabel,
  onSelectModel,
  mentions,
  commandContext,
}: {
  prompt: string;
  onPromptChange: (value: string) => void;
  /**
   * 提交一段输入。这里已经把文本解析成「命令 / 未知命令 / 普通提问」，
   * 但**是否可用以及如何反馈由面板决定** —— 因为那取决于 sidecar、归档状态
   * 等等面板才知道的事实。关键是一条：未知或不可用的命令绝不能退化成提问。
   */
  onSubmit: (submission: Submission) => void;
  onStop: () => void;
  running: boolean;
  stopping: boolean;
  /** 当前是否允许提交（取决于 shell、sidecar、provider 与归档状态）。 */
  canSend: boolean;
  /** 运行中有 runId 且没有正在停止时才允许中断。 */
  canStop: boolean;
  permissionMode: AgentPermissionMode;
  onPermissionModeChange: (mode: AgentPermissionMode) => void;
  /** 这次运行允许做到什么程度；由 sidecar 权限引擎强制，不只是提示。 */
  runMode: AgentRunMode;
  onRunModeChange: (mode: AgentRunMode) => void;
  models: ReadonlyArray<{ key: string; label: string }>;
  selectedModelKey: string | null;
  /** 当前选中模型的完整名称，用于在输入框上方显示当前运行目标。 */
  selectedModelLabel: string | null;
  onSelectModel: (key: string) => void;
  /** 可被 @ 提及的服务器。 */
  mentions: readonly MentionCandidate[];
  commandContext: CommandContext;
}) {
  const modeSpec = runModeSpec(runMode);
  const approvalSpec = approvalOptionSpec(permissionMode);
  const inputRef = useRef<HTMLTextAreaElement>(null);
  const [trigger, setTrigger] = useState<ActiveTrigger | null>(null);
  const [highlight, setHighlight] = useState(0);
  /** Escape 之后本次触发词不再弹窗，直到内容变化。 */
  const [dismissed, setDismissed] = useState(false);
  /** 「+」展开的设置面板：运行模式与批准方式都在这里。 */
  const [menuOpen, setMenuOpen] = useState(false);

  /**
   * 输入框随内容长高，封顶由 CSS 的 max-height 决定。
   * 必须先把 height 归零再量 scrollHeight —— 否则删字时量到的还是旧高度，框不会缩回去。
   */
  useLayoutEffect(() => {
    const input = inputRef.current;
    if (!input) return;
    input.style.height = "auto";
    input.style.height = `${input.scrollHeight}px`;
  }, [prompt]);

  const suggestions: Suggestion[] = (() => {
    if (!trigger || dismissed) return [];
    if (trigger.kind === "command") {
      return matchCommands(trigger.query, commandContext).map(({ spec, reason }) => ({
        kind: "command" as const,
        key: `cmd:${spec.name}`,
        spec,
        reason,
      }));
    }
    return matchMentions(mentions, trigger.query).map((candidate) => ({
      kind: "mention" as const,
      key: `mention:${candidate.id}`,
      candidate,
    }));
  })();

  const open = suggestions.length > 0;
  const active = open ? Math.min(highlight, suggestions.length - 1) : 0;

  const syncTrigger = (value: string, caret: number | null): void => {
    const next = findActiveTrigger(value, caret ?? value.length);
    // 同一触发词内继续打字不算「重新打开」，Escape 的状态要保留。
    if (next?.start !== trigger?.start || next?.kind !== trigger?.kind) setDismissed(false);
    setTrigger(next);
    if (next?.start !== trigger?.start) setHighlight(0);
  };

  const applySuggestion = (suggestion: Suggestion): void => {
    if (!trigger) return;
    const insertion = suggestion.kind === "command" ? `/${suggestion.spec.name}` : `@${suggestion.candidate.label}`;
    const next = replaceTrigger(prompt, trigger, insertion);
    onPromptChange(next.text);
    setTrigger(null);
    setDismissed(false);
    setHighlight(0);
    // 插入后把光标放到内容末尾，用户可以接着打字。
    requestAnimationFrame(() => {
      const input = inputRef.current;
      if (!input) return;
      input.focus();
      input.setSelectionRange(next.caret, next.caret);
    });
  };

  const submit = (): void => {
    setTrigger(null);
    setDismissed(false);
    onSubmit(resolveSubmission(prompt));
  };

  const handleKeyDown = (event: KeyboardEvent<HTMLTextAreaElement>): void => {
    // 候选列表打开时，方向键与 Enter 归列表所有。
    if (open) {
      if (event.key === "ArrowDown") {
        event.preventDefault();
        setHighlight((current) => (current + 1) % suggestions.length);
        return;
      }
      if (event.key === "ArrowUp") {
        event.preventDefault();
        setHighlight((current) => (current - 1 + suggestions.length) % suggestions.length);
        return;
      }
      if (event.key === "Escape") {
        event.preventDefault();
        setDismissed(true);
        return;
      }
      if (event.key === "Tab" || (event.key === "Enter" && !event.shiftKey)) {
        const suggestion = suggestions[active];
        if (suggestion && (suggestion.kind === "mention" || suggestion.reason === null)) {
          event.preventDefault();
          applySuggestion(suggestion);
          return;
        }
        // 命令不可用时不吞掉 Enter，让用户照常提交并看到明确结果。
      }
    }
    // 输入法组合中的 Enter 是选词确认，不是发送。
    if (event.key !== "Enter" || event.shiftKey || event.nativeEvent.isComposing || event.keyCode === 229) return;
    event.preventDefault();
    submit();
  };

  return (
    <footer className="agent-composer">
      <div className="agent-composer-shell">
        <textarea
          ref={inputRef}
          rows={1}
          aria-label="Agent 输入"
          value={prompt}
          disabled={!canSend || running}
          onChange={(event) => {
            const value = event.target.value;
            onPromptChange(value);
            syncTrigger(value, event.target.selectionStart);
          }}
          onKeyDown={handleKeyDown}
          onClick={(event) => syncTrigger(prompt, event.currentTarget.selectionStart)}
          onBlur={() => setTrigger(null)}
          placeholder={running ? "运行中…" : "随心输入，/ 命令，@ 提及服务器"}
          className="agent-composer-input"
        />

        {open ? (
          <ul className="composer-suggestions" role="listbox" aria-label={trigger?.kind === "command" ? "可用命令" : "可提及的服务器"}>
            {suggestions.map((suggestion, index) => {
              const disabled = suggestion.kind === "command" && suggestion.reason !== null;
              return (
                <li key={suggestion.key}>
                  <button
                    type="button"
                    role="option"
                    aria-selected={index === active}
                    className={`composer-suggestion ${index === active ? "is-active" : ""}`}
                    disabled={disabled}
                    onMouseDown={(event) => event.preventDefault()}
                    onClick={() => applySuggestion(suggestion)}
                  >
                    <span className="composer-suggestion-name">
                      {suggestion.kind === "command" ? `/${suggestion.spec.name}` : `@${suggestion.candidate.label}`}
                    </span>
                    <span className="composer-suggestion-note">
                      {suggestion.kind === "command" ? suggestion.reason ?? suggestion.spec.description : suggestion.candidate.detail ?? ""}
                    </span>
                  </button>
                </li>
              );
            })}
          </ul>
        ) : null}

        {menuOpen ? (
          <div className="agent-settings-panel" role="dialog" aria-label="运行设置">
            <section className="agent-settings-group">
              <p className="agent-settings-title">运行模式</p>
              <p className="agent-settings-note">决定这次运行能做什么。只读类模式由权限引擎强制，模型无法绕过。</p>
              <ul className="agent-settings-options">
                {RUN_MODE_ORDER.map((key) => {
                  const spec = RUN_MODE_SPECS[key];
                  return (
                  <li key={spec.mode}>
                    <button
                      type="button"
                      className={`agent-settings-option ${spec.mode === runMode ? "is-selected" : ""}`}
                      aria-pressed={spec.mode === runMode}
                      // 运行中不允许切换：模式是随请求发出去的，中途改变会让
                      // 「这次运行到底受哪个模式约束」变得说不清。
                      disabled={running}
                      onClick={() => onRunModeChange(spec.mode)}
                    >
                      <span className="agent-settings-option-label">
                        {spec.label}
                        {spec.readOnly ? <span className="agent-settings-badge">不改动</span> : null}
                      </span>
                      <span className="agent-settings-option-note">{spec.summary}</span>
                    </button>
                  </li>
                  );
                })}
              </ul>
            </section>

            <section className="agent-settings-group">
              <p className="agent-settings-title">批准方式</p>
              <p className="agent-settings-note">决定允许的操作由谁点头。与运行模式互不冲突。</p>
              <ul className="agent-settings-options">
                {APPROVAL_ORDER.map((key) => {
                  const option = APPROVAL_OPTIONS[key];
                  return (
                  <li key={option.value}>
                    <button
                      type="button"
                      className={`agent-settings-option ${option.value === permissionMode ? "is-selected" : ""}`}
                      aria-pressed={option.value === permissionMode}
                      disabled={running}
                      onClick={() => onPermissionModeChange(option.value)}
                    >
                      <span className="agent-settings-option-label">{option.label}</span>
                      <span className="agent-settings-option-note">{option.summary}</span>
                    </button>
                  </li>
                  );
                })}
              </ul>
            </section>
          </div>
        ) : null}

        <div className="agent-composer-toolbar">
          <div className="agent-composer-toolbar-left">
            {/* 最左侧的「+」展开设置：运行模式（做到什么程度）与批准方式（谁
                点头）。只读模式下「+」保持可点 —— 用户必须能改主意。 */}
            <button
              type="button"
              className={`agent-composer-tool agent-settings-trigger ${menuOpen ? "is-open" : ""}`}
              aria-label="运行设置"
              aria-expanded={menuOpen}
              aria-haspopup="dialog"
              title="运行模式与批准方式"
              onClick={() => {
                setMenuOpen((open) => !open);
                setTrigger(null);
              }}
            >
              {/* A plus rotated 45° is the close glyph, so the control animates
                  between the two states instead of swapping one drawing for
                  another. Keep rendering `plus`: the rotation is the state. */}
              <Icon name="plus" size="lg" />
            </button>
            {/* 当前状态的常驻摘要：不展开菜单也能看出这次运行会怎样。 */}
            <span className={`agent-mode-summary ${modeSpec.readOnly ? "is-readonly" : ""}`}>
              {modeSpec.label}
            </span>
            <span className="agent-approval-summary" title={approvalSpec.summary}>
              {approvalSpec.label}
            </span>
          </div>
          <div className="agent-composer-toolbar-right">
            {models.length ? (
              <label className="agent-model-select" title={selectedModelLabel ? `当前模型：${selectedModelLabel}` : "选择模型"}>
                <select
                  aria-label="选择模型"
                  value={selectedModelKey ?? models[0]?.key ?? ""}
                  disabled={running}
                  onChange={(event) => onSelectModel(event.target.value)}
                >
                  {models.map((choice) => <option value={choice.key} key={choice.key}>{choice.label}</option>)}
                </select>
                <Icon name="chevronDown" size="xs" />
              </label>
            ) : <span className="agent-model-empty">未配置模型</span>}
            {/* 这里原本有一个恒为 disabled 的麦克风按钮，鼠标悬停只显示「禁止」，
                点不动也说不清原因 —— 一个不存在的能力不该占据位置。已删除。
                语音若要做，应作为真实能力连同它的权限与降级路径一起加回来。 */}
            {/* 发送按钮在有内容时才出现；它不再兼任停止。
                停止单独成一个按钮，只在运行中出现 —— 原来二者共用一个图标按钮，
                结果是「没有输入」和「等待运行」两种状态都表现为一个禁用的图标。 */}
            {prompt.trim() && !running ? (
              <button
                type="button"
                className="agent-send-button"
                aria-label="发送消息"
                title="发送消息（Enter）"
                disabled={!canSend}
                onClick={submit}
              >
                发送
              </button>
            ) : null}
          </div>
        </div>

        {running ? (
          <div className="agent-running-bar">
            <span className="agent-running-label">
              <span className="agent-running-dot" aria-hidden="true" />
              运行中
            </span>
            <button
              type="button"
              className="agent-stop-button"
              aria-label="停止 Agent"
              title={stopping ? "停止中" : "停止 Agent"}
              disabled={!canStop}
              onClick={onStop}
            >
              <Icon name="stop" size="sm" />
              停止
            </button>
          </div>
        ) : null}
      </div>
    </footer>
  );
}
