import { useCallback, useEffect, useLayoutEffect, useId, useRef, useState, type CSSProperties } from "react";
import type { AgentReasoningEffort } from "@yukinal/shared";
import { Icon } from "../../components/Icon.js";
import { usePresence } from "../../hooks/usePresence.js";
import { useDismissOnOutsidePointer } from "../../hooks/useDismissOnOutsidePointer.js";
import { ModelPicker, shortModelLabel, type ModelPickerChoice } from "./ModelPicker.js";
import { REASONING_LABELS } from "./reasoning.js";

export function ReasoningPicker({ choices, selectedModelKey, selectedModelTitle, onSelectModel, levels, value, disabled, onChange }: {
  choices: readonly ModelPickerChoice[];
  selectedModelKey: string | null;
  selectedModelTitle: string | null;
  onSelectModel: (key: string) => void;
  levels: readonly AgentReasoningEffort[];
  value: AgentReasoningEffort | null;
  disabled: boolean;
  onChange: (value: AgentReasoningEffort | null) => void;
}) {
  const [open, setOpen] = useState(false);
  const enabled = !disabled && choices.length > 0;
  const menuOpen = open && enabled;
  const presence = usePresence(menuOpen, { exitAnimation: "popover-exit" });
  const menuId = useId();
  const root = useRef<HTMLDivElement>(null);
  const trigger = useRef<HTMLButtonElement>(null);
  const menu = useRef<HTMLDivElement>(null);
  const range = useRef<HTMLInputElement>(null);
  const model = choices.find((choice) => choice.key === selectedModelKey);
  const modelLabel = model?.label ?? "选择模型";
  const selected = value !== null && levels.includes(value) ? value : null;
  const effortLabel = selected ? REASONING_LABELS[selected] : "默认";
  const index = selected ? levels.indexOf(selected) : Math.max(0, levels.indexOf("high"));
  useDismissOnOutsidePointer(menuOpen, [root], useCallback(() => setOpen(false), []));
  useEffect(() => { if (!enabled) setOpen(false); }, [enabled]);
  useLayoutEffect(() => {
    if (menuOpen && presence.mounted) menu.current?.querySelector<HTMLElement>('[role="listbox"]')?.focus({ preventScroll: true });
  }, [menuOpen, presence.mounted]);
  if (!choices.length) return null;
  return <div className="agent-reasoning-picker" ref={root} onBlur={(event) => {
    if (!event.currentTarget.contains(event.relatedTarget as Node | null)) setOpen(false);
  }} onKeyDown={(event) => {
    if (event.key === "Escape" && menuOpen) { event.preventDefault(); event.stopPropagation(); setOpen(false); trigger.current?.focus(); }
  }}>
    <button ref={trigger} type="button" className="agent-effort-trigger" aria-label={`模型与推理强度：${shortModelLabel(modelLabel)}${levels.length ? `，${effortLabel}` : ""}`} title={selectedModelTitle ?? modelLabel} aria-haspopup="dialog" aria-expanded={menuOpen} aria-controls={presence.mounted ? menuId : undefined} disabled={disabled} onClick={() => setOpen((current) => !current)}>
      <span className="agent-model-value">{shortModelLabel(modelLabel)}</span>
      {levels.length ? <span className="agent-effort-value">{effortLabel}</span> : null}
      <Icon name="chevronDown" size="xs" />
    </button>
    {presence.mounted ? <div ref={menu} id={menuId} className={`agent-effort-menu${!menuOpen ? " is-closing" : ""}`} role="dialog" aria-label="模型与推理强度" aria-hidden={!menuOpen} inert={!menuOpen} onAnimationEnd={presence.onAnimationEnd}>
      <ModelPicker inline choices={choices} selectedKey={selectedModelKey} selectedTitle={selectedModelTitle} onSelect={onSelectModel} disabled={disabled || !menuOpen} />
      {levels.length ? <div className="agent-effort-heading">
        <div className="agent-effort-selection">
          {levels.length ? <strong>{effortLabel}</strong> : null}
        </div>
        {levels.length ? <button type="button" className="icon-button" title="恢复模型默认强度" aria-label="恢复模型默认强度" disabled={disabled || !menuOpen || selected === null} onClick={() => { if (menuOpen) { onChange(null); range.current?.focus({ preventScroll: true }); } }}><Icon name="refresh" size="sm" /></button> : null}
      </div> : null}
      {levels.length ? <>
        <input ref={range} type="range" aria-label="推理强度" aria-valuetext={selected === null ? "模型默认，尚未指定强度" : REASONING_LABELS[selected]} min={0} max={levels.length - 1} step={1} value={index} disabled={disabled || !menuOpen} style={{ "--effort-progress": `${index / Math.max(1, levels.length - 1) * 100}%` } as CSSProperties} onChange={(event) => { if (menuOpen) onChange(levels[Number(event.target.value)] ?? null); }} />
        <div className="agent-effort-stops" aria-hidden="true">{levels.map((level) => <span key={level}>{REASONING_LABELS[level]}</span>)}</div>
      </> : null}
    </div> : null}
  </div>;
}
