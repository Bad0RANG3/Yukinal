import { useCallback, useEffect, useId, useLayoutEffect, useRef, useState, type KeyboardEvent, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { useDismissOnOutsidePointer } from "../../hooks/useDismissOnOutsidePointer.js";

/** Row operations stay available without displacing the connection identity. */
export function ServerRowActions({ name, children }: { name: string; children: ReactNode }) {
  const [open, setOpen] = useState(false);
  const [position, setPosition] = useState<{ left: number; top: number } | null>(null);
  const root = useRef<HTMLDivElement>(null);
  const trigger = useRef<HTMLButtonElement>(null);
  const menu = useRef<HTMLDivElement>(null);
  const menuId = useId();
  const dismiss = useCallback(() => { setOpen(false); setPosition(null); }, []);
  const closeAndFocus = () => { dismiss(); trigger.current?.focus({ preventScroll: true }); };
  useDismissOnOutsidePointer(open, [root, menu], dismiss);
  useLayoutEffect(() => {
    if (!open || !trigger.current || !menu.current) return;
    const anchor = trigger.current.getBoundingClientRect();
    const bounds = menu.current.getBoundingClientRect();
    setPosition({ left: Math.max(8, Math.min(anchor.right - bounds.width, window.innerWidth - bounds.width - 8)), top: Math.max(8, anchor.bottom + bounds.height + 4 <= window.innerHeight - 8 ? anchor.bottom + 4 : anchor.top - bounds.height - 4) });
  }, [open]);
  useEffect(() => { if (open && position) menu.current?.querySelector<HTMLButtonElement>("button:not(:disabled)")?.focus({ preventScroll: true }); }, [open, position]);
  useEffect(() => {
    if (!open) return;
    const onScroll = (event: Event) => { if (!(event.target instanceof Node) || !menu.current?.contains(event.target)) dismiss(); };
    window.addEventListener("resize", dismiss);
    document.addEventListener("scroll", onScroll, true);
    return () => { window.removeEventListener("resize", dismiss); document.removeEventListener("scroll", onScroll, true); };
  }, [open, dismiss]);
  const navigate = (event: KeyboardEvent<HTMLDivElement>) => {
    if (event.key === "Escape") { event.preventDefault(); event.stopPropagation(); closeAndFocus(); return; }
    if (event.key === "Tab") { closeAndFocus(); return; }
    if (!["ArrowDown", "ArrowUp", "Home", "End"].includes(event.key)) return;
    event.preventDefault();
    const items = [...(menu.current?.querySelectorAll<HTMLButtonElement>("button:not(:disabled)") ?? [])];
    if (!items.length) return;
    const index = items.indexOf(document.activeElement as HTMLButtonElement);
    const next = event.key === "Home" ? 0 : event.key === "End" ? items.length - 1 : (index + (event.key === "ArrowUp" ? -1 : 1) + items.length) % items.length;
    items[next]?.focus();
  };
  return <div className="server-row-actions" ref={root}>
    <button ref={trigger} type="button" className="server-more-trigger" aria-label={`${name} 的更多操作`} aria-haspopup="menu" aria-expanded={open} aria-controls={open ? menuId : undefined} onClick={() => open ? dismiss() : setOpen(true)} onKeyDown={(event) => { if (event.key === "ArrowDown") { event.preventDefault(); setOpen(true); } }}>
      <svg width="14" height="14" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true"><circle cx="5" cy="12" r="1.8" /><circle cx="12" cy="12" r="1.8" /><circle cx="19" cy="12" r="1.8" /></svg>
    </button>
    {open ? createPortal(<div ref={menu} id={menuId} className="server-actions-menu" role="menu" aria-label={`${name} 的操作`} style={{ left: position?.left ?? 0, top: position?.top ?? 0, visibility: position ? "visible" : "hidden" }} onKeyDown={navigate} onClick={(event) => { if ((event.target as HTMLElement).closest("button:not(:disabled)")) closeAndFocus(); }}>{children}</div>, document.body) : null}
  </div>;
}
