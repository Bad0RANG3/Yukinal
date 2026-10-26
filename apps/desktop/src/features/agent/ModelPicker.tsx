/**
 * 模型选择器：底栏里唯一说明「这次运行用哪个模型」的控件。
 *
 * 为什么不用原生 `<select>`：
 *
 * 1. 关闭态只能显示所选 option 的完整文本，而模型 ID 常见形如
 *    `anthropic/claude-sonnet-4.5`。这个长度要么把底栏挤到换行，要么被截成
 *    一个省略号 —— 两种都回答不了「我现在到底用的是哪个模型」。自绘之后，
 *    关闭态只显示模型自己的短名，完整名称留给 title 与列表项。
 * 2. 原生下拉的弹层由操作系统绘制，排版与配色都跟不上这套界面。
 *
 * 无障碍按 ARIA 1.2 的 select-only combobox 实现：焦点始终留在触发器上，
 * 用 `aria-activedescendant` 指向高亮项。这样 Tab 顺序不会被弹层劫持，
 * 关闭弹层也不需要额外把焦点搬回来。
 */

import { useCallback, useEffect, useLayoutEffect, useId, useRef, useState, type KeyboardEvent } from "react";

import { Icon } from "../../components/Icon.js";
import { useDismissOnOutsidePointer } from "../../hooks/useDismissOnOutsidePointer.js";
import { usePresence } from "../../hooks/usePresence.js";
import { useAnchoredPopover } from "../../hooks/useAnchoredPopover.js";

/** 一个可选模型。`key` 是 provider:model 复合键，因为同名模型可能挂在多个 provider 下。 */
export type ModelPickerChoice = {
  key: string;
  label: string;
  /** provider 的显示名；有值时在列表里作为归属提示出现。 */
  providerLabel?: string;
};

/**
 * 去掉 `vendor/` 前缀。
 *
 * 关闭态只显示模型自己的名字：`anthropic/claude-sonnet-4.5` → `claude-sonnet-4.5`。
 * 没有斜杠时原样返回，避免把 `gpt-4` 之类的名字切坏。
 */
export function shortModelLabel(label: string): string {
  if (/\s/.test(label.trim())) return label;
  const tail = label.slice(label.lastIndexOf("/") + 1).trim();
  return tail || label;
}

export type ModelPickerHighlight = { choiceKey: string | null; selectedKey: string | null };

/** Highlight follows a stable choice key; it never changes the actual selected model. */
export function activeModelChoice(choices: readonly ModelPickerChoice[], selectedKey: string | null, highlight: ModelPickerHighlight): ModelPickerChoice | undefined {
  const selected = choices.find((choice) => choice.key === selectedKey);
  const active = highlight.selectedKey === selectedKey ? choices.find((choice) => choice.key === highlight.choiceKey) : selected;
  return active ?? selected ?? choices[0];
}

export type ModelPickerIntent =
  | { type: "none" }
  | { type: "open" }
  | { type: "highlight"; key: string }
  | { type: "select"; key: string }
  | { type: "close"; reason: "escape" | "tab" };

/** Pure keyboard policy shared by the real combobox and embedded listbox. */
export function modelPickerKeyIntent(choices: readonly ModelPickerChoice[], activeKey: string | null, key: string, state: { open: boolean; inline: boolean; disabled: boolean }): ModelPickerIntent {
  if (state.disabled || choices.length === 0) return { type: "none" };
  if (!state.open && !state.inline && ["ArrowDown", "ArrowUp", "Enter", " "].includes(key)) return { type: "open" };
  const index = Math.max(0, choices.findIndex((choice) => choice.key === activeKey));
  const nextIndex = key === "Home" ? 0 : key === "End" ? choices.length - 1
    : key === "ArrowDown" ? (index + 1) % choices.length
      : key === "ArrowUp" ? (index - 1 + choices.length) % choices.length : null;
  if (nextIndex !== null) return { type: "highlight", key: choices[nextIndex]!.key };
  if (!state.open && !state.inline) return { type: "none" };
  if (key === "Enter" || key === " ") return { type: "select", key: choices[index]!.key };
  if (!state.inline && key === "Escape") return { type: "close", reason: "escape" };
  if (!state.inline && key === "Tab") return { type: "close", reason: "tab" };
  return { type: "none" };
}

export function ModelPicker({
  choices,
  selectedKey,
  selectedTitle,
  onSelect,
  disabled = false,
  inline = false,
}: {
  choices: readonly ModelPickerChoice[];
  selectedKey: string | null;
  /** 触发器的悬停提示：完整的「provider · model」，用来补回短名省掉的信息。 */
  selectedTitle: string | null;
  onSelect: (key: string) => void;
  disabled?: boolean;
  /** 在同一设置弹层中直接列出模型，避免再套一层弹出菜单。 */
  inline?: boolean;
}) {
  const [open, setOpen] = useState(false);
  const [highlight, setHighlight] = useState<ModelPickerHighlight>(() => ({ choiceKey: selectedKey, selectedKey }));
  const rootRef = useRef<HTMLDivElement>(null);
  const listRef = useRef<HTMLUListElement>(null);
  const surfaceRef = useRef<HTMLDivElement>(null);
  const listboxId = useId();
  // 逻辑上的 open 驱动全部行为；presence 只决定「还在不在 DOM 里」，
  // 好让 popover-exit 有机会播完。两者绝不能混用。
  const menuOpen = open && !disabled && choices.length > 0;
  const interactive = !disabled && (inline || menuOpen);
  const presence = usePresence(menuOpen, { exitAnimation: "popover-exit" });
  useAnchoredPopover(!inline && presence.mounted, rootRef, surfaceRef, 300);

  useEffect(() => { if (disabled || !choices.length) setOpen(false); }, [disabled, choices.length]);

  // An embedded list survives its dialog's exit animation. Reopening that same
  // node or receiving a new model must start at the selected item before focus.
  useLayoutEffect(() => {
    setHighlight((current) => current.choiceKey === selectedKey && current.selectedKey === selectedKey
      ? current : { choiceKey: selectedKey, selectedKey });
  }, [selectedKey, disabled]);

  const selectedIndex = choices.findIndex((choice) => choice.key === selectedKey);
  const selected = selectedIndex >= 0 ? choices[selectedIndex] : undefined;
  const selectedLabel = selected?.label ?? "选择模型";
  const activeChoice = activeModelChoice(choices, selectedKey, highlight);
  const activeIndex = activeChoice ? choices.indexOf(activeChoice) : -1;
  const activeId = activeIndex >= 0 ? `${listboxId}-${activeIndex}` : undefined;

  const highlightChoice = (key: string) => setHighlight({ choiceKey: key, selectedKey });
  const openMenu = () => {
    setHighlight({ choiceKey: selected?.key ?? choices[0]?.key ?? null, selectedKey });
    setOpen(true);
  };

  useEffect(() => {
    if (!(interactive && (inline || presence.mounted))) return;
    listRef.current?.children[activeIndex]?.scrollIntoView({ block: "nearest", inline: "nearest" });
  }, [interactive, inline, presence.mounted, activeIndex]);

  useDismissOnOutsidePointer(menuOpen && !inline, [rootRef], useCallback(() => setOpen(false), []));

  const choose = (key: string): void => {
    if (!interactive || !choices.some((choice) => choice.key === key)) return;
    onSelect(key);
    setOpen(false);
  };

  const onKeyDown = (event: KeyboardEvent<HTMLElement>): void => {
    const intent = modelPickerKeyIntent(choices, activeChoice?.key ?? null, event.key, { open: menuOpen, inline, disabled });
    switch (intent.type) {
      case "none": return;
      case "open":
        event.preventDefault();
        openMenu();
        return;
      case "highlight":
        event.preventDefault();
        highlightChoice(intent.key);
        if (!inline) setOpen(true);
        return;
      case "select":
        event.preventDefault();
        choose(intent.key);
        return;
      case "close":
        if (intent.reason === "escape") {
          // Close this menu before the outer Agent panel sees Escape.
          event.preventDefault();
          event.stopPropagation();
        }
        // Tab retains native focus movement; embedded lists leave dismissal to their dialog.
        setOpen(false);
    }
  };

  if (!choices.length) return null;

  const list = <ul
          ref={listRef}
          className="agent-model-menu"
          role="listbox"
          id={listboxId}
          aria-label="可用模型"
          aria-activedescendant={inline && interactive ? activeId : undefined}
          aria-disabled={disabled || undefined}
          inert={disabled}
          tabIndex={inline ? (disabled ? -1 : 0) : undefined}
          onKeyDown={inline ? onKeyDown : undefined}
        >
          {choices.map((choice, index) => (
            <li
              key={choice.key}
              id={`${listboxId}-${index}`}
              role="option"
              aria-selected={choice.key === selected?.key}
              title={choice.providerLabel ? `${choice.providerLabel} · ${choice.label}` : choice.label}
              className={`agent-model-option ${index === activeIndex ? "is-active" : ""}`}
              onMouseEnter={() => { if (interactive) highlightChoice(choice.key); }}
              onMouseDown={(event) => { if (interactive) event.preventDefault(); }}
              onClick={() => { if (interactive) { if (inline) listRef.current?.focus({ preventScroll: true }); choose(choice.key); } }}
            >
              <span className="agent-model-option-name">{shortModelLabel(choice.label)}</span>
              {choice.key === selected?.key ? <Icon name="check" size="sm" /> : null}
              {choice.providerLabel && choices.some((other) => other.key !== choice.key && shortModelLabel(other.label) === shortModelLabel(choice.label)) ? <span className="agent-model-option-from">{choice.providerLabel}</span> : null}
            </li>
          ))}
        </ul>;

  return (
    <div className={`agent-model-picker${inline ? " agent-model-picker-inline" : ""}`} ref={rootRef} onBlur={(event) => {
      if (!inline && !event.currentTarget.contains(event.relatedTarget as Node | null)) setOpen(false);
    }}>
      {inline ? list : <button
        type="button"
        role="combobox"
        className={`agent-model-select ${menuOpen ? "is-open" : ""}`}
        aria-label="选择模型"
        aria-haspopup="listbox"
        aria-expanded={menuOpen}
        aria-controls={presence.mounted ? listboxId : undefined}
        aria-activedescendant={menuOpen && presence.mounted ? activeId : undefined}
        title={selectedTitle ?? selectedLabel}
        disabled={disabled}
        onClick={() => { if (menuOpen) setOpen(false); else openMenu(); }}
        onKeyDown={onKeyDown}
      >
        <span className="agent-model-value">{shortModelLabel(selectedLabel)}</span>
        <Icon name="chevronDown" size="xs" />
      </button>}

      {!inline && presence.mounted ? (
        <div ref={surfaceRef} popover="manual" className={`agent-model-popover anchored-popover ${!menuOpen ? "is-closing" : ""}`} aria-hidden={!menuOpen} inert={!menuOpen} onAnimationEnd={presence.onAnimationEnd}>
        <p className="agent-model-menu-title">选择模型</p>
        {list}
        </div>
      ) : null}
    </div>
  );
}
