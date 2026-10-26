import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import type { AgentPermissionMode } from "@yukinal/shared";
import { Icon } from "../../components/Icon.js";
import { useDismissOnOutsidePointer } from "../../hooks/useDismissOnOutsidePointer.js";

const OPTIONS = [
  { value: "auto", label: "自动批准", summary: "变更直接执行，宿主仍复核目标与输入" },
  { value: "ask", label: "每次询问", summary: "写入与执行变更前先确认" },
] as const;

export function PermissionPicker({ value, readOnly, disabled, onChange }: { value: AgentPermissionMode; readOnly: boolean; disabled: boolean; onChange: (value: AgentPermissionMode) => void }) {
  const [open, setOpen] = useState(false);
  const root = useRef<HTMLDivElement>(null);
  const trigger = useRef<HTMLButtonElement>(null);
  const menu = useRef<HTMLDivElement>(null);
  useDismissOnOutsidePointer(open, [root], useCallback(() => setOpen(false), []));
  useEffect(() => { if (disabled) setOpen(false); }, [disabled]);
  useLayoutEffect(() => { if (open) menu.current?.querySelector<HTMLButtonElement>(`[aria-checked="true"]`)?.focus(); }, [open]);
  const label = readOnly ? "只读访问" : value === "auto" ? "自动批准" : "每次询问";
  return <div className="agent-permission-picker" ref={root} onKeyDown={(event) => {
    if (event.key === "Escape" && open) { event.preventDefault(); event.stopPropagation(); setOpen(false); trigger.current?.focus(); }
  }}>
    <button type="button" ref={trigger} className="agent-permission-trigger" aria-label={`访问权限：${label}`} aria-haspopup="dialog" aria-expanded={open} disabled={disabled} onClick={() => setOpen((current) => !current)}><Icon name="shield" size="sm" /><span>{label}</span><Icon name="chevronDown" size="xs" /></button>
    {open ? <div ref={menu} className="agent-permission-menu" role="dialog" aria-label="访问权限">
      <p>如何批准操作</p>
      <div role="radiogroup" aria-label="批准方式" onKeyDown={(event) => {
        if (!["ArrowUp", "ArrowDown", "ArrowLeft", "ArrowRight"].includes(event.key)) return;
        event.preventDefault();
        const next = value === "ask" ? "auto" : "ask";
        onChange(next);
        menu.current?.querySelector<HTMLButtonElement>(`[data-value="${next}"]`)?.focus();
      }}>
      {OPTIONS.map((option) => <button key={option.value} type="button" role="radio" data-value={option.value} tabIndex={value === option.value ? 0 : -1} aria-checked={value === option.value} disabled={disabled} onClick={() => { onChange(option.value); setOpen(false); trigger.current?.focus(); }}><Icon name={option.value === "ask" ? "shield" : "agent"} size="md" /><span><strong>{option.label}</strong><small>{option.summary}</small></span>{value === option.value ? <Icon name="check" size="sm" /> : null}</button>)}
      </div>
      <button type="button" disabled title="当前运行时的主机与变更审批仍须保留"><Icon name="warning" size="md" /><span><strong>完全访问权限</strong><small>当前运行时未开放</small></span></button>
    </div> : null}
  </div>;
}
