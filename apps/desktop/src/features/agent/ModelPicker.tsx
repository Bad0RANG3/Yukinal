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

import { useEffect, useId, useRef, useState, type KeyboardEvent } from "react";

import { Icon } from "../../components/Icon.js";

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
  const tail = label.slice(label.lastIndexOf("/") + 1).trim();
  return tail || label;
}

export function ModelPicker({
  choices,
  selectedKey,
  selectedTitle,
  onSelect,
  disabled = false,
}: {
  choices: readonly ModelPickerChoice[];
  selectedKey: string | null;
  /** 触发器的悬停提示：完整的「provider · model」，用来补回短名省掉的信息。 */
  selectedTitle: string | null;
  onSelect: (key: string) => void;
  disabled?: boolean;
}) {
  const [open, setOpen] = useState(false);
  const [active, setActive] = useState(0);
  const rootRef = useRef<HTMLDivElement>(null);
  const listboxId = useId();

  const selectedIndex = choices.findIndex((choice) => choice.key === selectedKey);
  const selected = selectedIndex >= 0 ? choices[selectedIndex] : choices[0];
  const activeIndex = Math.min(active, Math.max(0, choices.length - 1));

  // 每次展开都把高亮落在当前选中项上，而不是永远从第一项开始。
  useEffect(() => {
    if (open && selectedIndex >= 0) setActive(selectedIndex);
  }, [open, selectedIndex]);

  // 点到组件之外就收起。用 pointerdown 而不是 click：在别处拖动选择文本时，
  // 浏览器会补一次 click，弹层会在用户还没做完动作时意外关闭。
  useEffect(() => {
    if (!open) return;
    const onPointerDown = (event: PointerEvent) => {
      if (!rootRef.current?.contains(event.target as Node)) setOpen(false);
    };
    document.addEventListener("pointerdown", onPointerDown);
    return () => document.removeEventListener("pointerdown", onPointerDown);
  }, [open]);

  const choose = (key: string): void => {
    onSelect(key);
    setOpen(false);
  };

  const onKeyDown = (event: KeyboardEvent<HTMLButtonElement>): void => {
    if (!open) {
      // 关闭态下这几个键负责展开，与原生 select 的预期一致。
      if (event.key === "ArrowDown" || event.key === "ArrowUp" || event.key === "Enter" || event.key === " ") {
        event.preventDefault();
        setOpen(true);
      }
      return;
    }
    if (event.key === "ArrowDown") {
      event.preventDefault();
      setActive((current) => (current + 1) % choices.length);
      return;
    }
    if (event.key === "ArrowUp") {
      event.preventDefault();
      setActive((current) => (current - 1 + choices.length) % choices.length);
      return;
    }
    if (event.key === "Home") {
      event.preventDefault();
      setActive(0);
      return;
    }
    if (event.key === "End") {
      event.preventDefault();
      setActive(choices.length - 1);
      return;
    }
    if (event.key === "Enter" || event.key === " ") {
      event.preventDefault();
      const choice = choices[activeIndex];
      if (choice) choose(choice.key);
      return;
    }
    if (event.key === "Escape") {
      event.preventDefault();
      setOpen(false);
      return;
    }
    // Tab 交给浏览器：焦点正常移走，这里只把弹层收起。
    if (event.key === "Tab") setOpen(false);
  };

  if (!selected) return null;

  return (
    <div className="agent-model-picker" ref={rootRef}>
      <button
        type="button"
        role="combobox"
        className={`agent-model-select ${open ? "is-open" : ""}`}
        aria-label="选择模型"
        aria-haspopup="listbox"
        aria-expanded={open}
        aria-controls={listboxId}
        aria-activedescendant={open ? `${listboxId}-${activeIndex}` : undefined}
        title={selectedTitle ?? selected.label}
        disabled={disabled}
        onClick={() => setOpen((current) => !current)}
        onKeyDown={onKeyDown}
      >
        <span className="agent-model-value">{shortModelLabel(selected.label)}</span>
        <Icon name="chevronDown" size="xs" />
      </button>

      {open ? (
        <ul className="agent-model-menu" role="listbox" id={listboxId} aria-label="可用模型">
          {choices.map((choice, index) => (
            <li
              key={choice.key}
              id={`${listboxId}-${index}`}
              role="option"
              aria-selected={choice.key === selected.key}
              className={`agent-model-option ${index === activeIndex ? "is-active" : ""}`}
              onMouseEnter={() => setActive(index)}
              // 不阻止 mousedown 的话，浏览器会先把焦点从触发器挪走，
              // 触发器的 keydown/activedescendant 状态就断了。
              onMouseDown={(event) => event.preventDefault()}
              onClick={() => choose(choice.key)}
            >
              <span className="agent-model-option-name">{shortModelLabel(choice.label)}</span>
              {choice.providerLabel ? <span className="agent-model-option-from">{choice.providerLabel}</span> : null}
            </li>
          ))}
        </ul>
      ) : null}
    </div>
  );
}
