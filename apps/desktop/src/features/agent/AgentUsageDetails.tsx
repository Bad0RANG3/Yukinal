import { useCallback, useId, useRef, useState, type ReactNode } from "react";
import { Icon } from "../../components/Icon.js";
import { useAnchoredPopover } from "../../hooks/useAnchoredPopover.js";
import { useDismissOnOutsidePointer } from "../../hooks/useDismissOnOutsidePointer.js";

export function AgentUsageDetails({ trigger, className, children }: { trigger: ReactNode; className: string; children: ReactNode }) {
  const details = useRef<HTMLDetailsElement>(null);
  const summary = useRef<HTMLElement>(null);
  const surface = useRef<HTMLDivElement>(null);
  const id = useId();
  const [open, setOpen] = useState(false);
  const dismiss = useCallback(() => { if (details.current) details.current.open = false; setOpen(false); }, []);
  useAnchoredPopover(open, summary, surface, 360);
  useDismissOnOutsidePointer(open, [details], dismiss);
  return <details ref={details} className="agent-context-details" onToggle={(event) => {
    if (event.target === event.currentTarget) setOpen(event.currentTarget.open);
  }} onBlur={(event) => {
    if (!event.currentTarget.contains(event.relatedTarget as Node | null)) dismiss();
  }} onKeyDown={(event) => {
    if (event.key !== "Escape" || !event.currentTarget.open) return;
    event.preventDefault(); event.stopPropagation(); dismiss(); summary.current?.focus();
  }}>
    <summary ref={summary} className={`agent-context-trigger ${className}`} aria-label="查看上下文与用量" aria-haspopup="dialog" aria-controls={id} aria-expanded={open}>{trigger}<Icon name="chevronDown" size="xs" /></summary>
    <div ref={surface} id={id} popover="manual" className="agent-context-panel anchored-popover" tabIndex={0} role="region" aria-label="上下文与用量详情">
      <button type="button" className="icon-button agent-context-close" aria-label="关闭上下文与用量" onClick={() => { dismiss(); summary.current?.focus(); }}><Icon name="close" size="sm" /></button>
      {children}
    </div>
  </details>;
}
