import { useCallback, useEffect, useLayoutEffect, useRef, useState, type CSSProperties } from "react";
import type { AgentReasoningEffort } from "@yukinal/shared";
import { Icon } from "../../components/Icon.js";
import { useDismissOnOutsidePointer } from "../../hooks/useDismissOnOutsidePointer.js";
import { shortModelLabel } from "./ModelPicker.js";
import { REASONING_LABELS } from "./reasoning.js";

export function ReasoningPicker({ levels, value, modelLabel, disabled, onChange }: {
  levels: readonly AgentReasoningEffort[];
  value: AgentReasoningEffort | null;
  modelLabel: string;
  disabled: boolean;
  onChange: (value: AgentReasoningEffort | null) => void;
}) {
  const [open, setOpen] = useState(false);
  const root = useRef<HTMLDivElement>(null);
  const trigger = useRef<HTMLButtonElement>(null);
  const range = useRef<HTMLInputElement>(null);
  const selected = value !== null && levels.includes(value) ? value : null;
  const index = selected ? levels.indexOf(selected) : Math.max(0, levels.indexOf("high"));
  useDismissOnOutsidePointer(open, [root], useCallback(() => setOpen(false), []));
  useEffect(() => { setOpen(false); }, [disabled, modelLabel, levels.length]);
  useLayoutEffect(() => { if (open) range.current?.focus(); }, [open]);
  if (!levels.length) return null;
  return <div className="agent-reasoning-picker" ref={root} onKeyDown={(event) => {
    if (event.key === "Escape" && open) { event.preventDefault(); event.stopPropagation(); setOpen(false); trigger.current?.focus(); }
  }}>
    <button ref={trigger} type="button" className="agent-effort-trigger" aria-label={`推理强度：${selected ? REASONING_LABELS[selected] : "默认"}`} title="推理强度" aria-haspopup="dialog" aria-expanded={open} disabled={disabled} onClick={() => setOpen((current) => !current)}>{selected ? REASONING_LABELS[selected] : "默认"}</button>
    {open ? <div className="agent-effort-menu" role="dialog" aria-label="推理强度">
      <div className="agent-effort-heading"><Icon name="agent" size="sm" /><strong>{selected ? REASONING_LABELS[selected] : "默认"}</strong><button type="button" className="icon-button" title="恢复模型默认强度" aria-label="恢复模型默认强度" disabled={disabled || selected === null} onClick={() => { onChange(null); range.current?.focus(); }}><Icon name="refresh" size="sm" /></button></div>
      <span className="agent-effort-model" title={modelLabel}>{shortModelLabel(modelLabel)}</span>
      <input ref={range} type="range" aria-label="推理强度" aria-valuetext={REASONING_LABELS[levels[index]!] + (selected === null ? "（模型默认）" : "")} min={0} max={levels.length - 1} step={1} value={index} disabled={disabled} style={{ "--effort-progress": `${index / Math.max(1, levels.length - 1) * 100}%` } as CSSProperties} onChange={(event) => onChange(levels[Number(event.target.value)] ?? null)} />
      <div className="agent-effort-stops" aria-hidden="true">{levels.map((level) => <span key={level}>{REASONING_LABELS[level]}</span>)}</div>
    </div> : null}
  </div>;
}
