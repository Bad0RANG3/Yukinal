import { useCallback, useEffect, useLayoutEffect, useRef, useState, type CSSProperties } from "react";
import type { AgentReasoningEffort } from "@yukinal/shared";
import { Icon } from "../../components/Icon.js";
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
  const root = useRef<HTMLDivElement>(null);
  const trigger = useRef<HTMLButtonElement>(null);
  const menu = useRef<HTMLDivElement>(null);
  const range = useRef<HTMLInputElement>(null);
  const model = choices.find((choice) => choice.key === selectedModelKey) ?? choices[0];
  const selected = value !== null && levels.includes(value) ? value : null;
  const effortLabel = selected ? REASONING_LABELS[selected] : "默认";
  const index = selected ? levels.indexOf(selected) : Math.max(0, levels.indexOf("high"));
  useDismissOnOutsidePointer(open, [root], useCallback(() => setOpen(false), []));
  useEffect(() => { if (disabled) setOpen(false); }, [disabled]);
  useLayoutEffect(() => {
    if (open) menu.current?.querySelector<HTMLButtonElement>('[role="combobox"]')?.focus();
  }, [open]);
  if (!model) return null;
  return <div className="agent-reasoning-picker" ref={root} onKeyDown={(event) => {
    if (event.key === "Escape" && open) { event.preventDefault(); event.stopPropagation(); setOpen(false); trigger.current?.focus(); }
  }}>
    <button ref={trigger} type="button" className="agent-effort-trigger" aria-label={`模型与推理强度：${shortModelLabel(model.label)}${levels.length ? `，${effortLabel}` : ""}`} title={selectedModelTitle ?? model.label} aria-haspopup="dialog" aria-expanded={open} disabled={disabled} onClick={() => setOpen((current) => !current)}>
      <span className="agent-model-value">{shortModelLabel(model.label)}</span>
      {levels.length ? <span className="agent-effort-value">{effortLabel}</span> : null}
      <Icon name="chevronDown" size="xs" />
    </button>
    {open ? <div ref={menu} className="agent-effort-menu" role="dialog" aria-label="模型与推理强度">
      <div className="agent-effort-heading">
        <div className="agent-effort-selection">
          {levels.length ? <strong>{effortLabel}</strong> : null}
          <ModelPicker choices={choices} selectedKey={selectedModelKey} selectedTitle={selectedModelTitle} onSelect={onSelectModel} disabled={disabled} />
        </div>
        {levels.length ? <button type="button" className="icon-button" title="恢复模型默认强度" aria-label="恢复模型默认强度" disabled={disabled || selected === null} onClick={() => { onChange(null); range.current?.focus(); }}><Icon name="refresh" size="sm" /></button> : null}
      </div>
      {levels.length ? <>
        <input ref={range} type="range" aria-label="推理强度" aria-valuetext={REASONING_LABELS[levels[index]!] + (selected === null ? "（模型默认）" : "")} min={0} max={levels.length - 1} step={1} value={index} disabled={disabled} style={{ "--effort-progress": `${index / Math.max(1, levels.length - 1) * 100}%` } as CSSProperties} onChange={(event) => onChange(levels[Number(event.target.value)] ?? null)} />
        <div className="agent-effort-stops" aria-hidden="true">{levels.map((level) => <span key={level}>{REASONING_LABELS[level]}</span>)}</div>
      </> : null}
    </div> : null}
  </div>;
}
