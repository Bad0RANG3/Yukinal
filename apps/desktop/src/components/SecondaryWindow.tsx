import { useEffect, useId, useRef, useState, type ReactNode, type PointerEvent, type KeyboardEvent } from "react";
import { createPortal } from "react-dom";
import { Icon, type IconName } from "./Icon.js";
import { fitSecondaryWindow, moveSecondaryWindow, resizeSecondaryWindow, type WindowFrame } from "./secondary-window-frame.js";
import "./secondary-window.css";

let windowOrder = 80;
const viewport = () => ({ width: window.innerWidth, height: window.innerHeight });

/** Modeless properties: inspect a pinned object while the workspace remains usable. */
export function SecondaryWindow({ title, subtitle, icon = "file", tabs, activeTab, onTabChange, footer, children, onClose }: {
  title: string; subtitle?: string; icon?: IconName; children: ReactNode; footer?: ReactNode; onClose: () => void;
  tabs?: readonly { id: string; label: string }[]; activeTab?: string; onTabChange?: (id: string) => void;
}) {
  const id = useId();
  const root = useRef<HTMLDivElement>(null);
  const close = useRef<HTMLButtonElement>(null);
  const [frame, setFrame] = useState(() => fitSecondaryWindow(viewport()));
  const [order, setOrder] = useState(() => ++windowOrder);
  const [maximized, setMaximized] = useState(false);
  const restore = useRef<WindowFrame | null>(null);
  const gesture = useRef<{ kind: "move" | "resize"; x: number; y: number; frame: WindowFrame } | null>(null);
  useEffect(() => {
    const previous = document.activeElement;
    close.current?.focus({ preventScroll: true });
    const element = root.current;
    const fit = () => setFrame((current) => fitSecondaryWindow(viewport(), current));
    window.addEventListener("resize", fit);
    return () => {
      window.removeEventListener("resize", fit);
      if (previous instanceof HTMLElement && previous.isConnected && (document.activeElement === document.body || element?.contains(document.activeElement))) previous.focus({ preventScroll: true });
    };
  }, []);
  const start = (event: PointerEvent<HTMLButtonElement>, kind: "move" | "resize") => {
    if (event.button !== 0 || maximized) return;
    gesture.current = { kind, x: event.clientX, y: event.clientY, frame };
    event.currentTarget.setPointerCapture(event.pointerId);
    event.preventDefault();
  };
  const move = (event: PointerEvent<HTMLButtonElement>) => {
    const current = gesture.current;
    if (current) setFrame((current.kind === "move" ? moveSecondaryWindow : resizeSecondaryWindow)(viewport(), current.frame, event.clientX - current.x, event.clientY - current.y));
  };
  const end = () => { gesture.current = null; };
  const keyboardFrame = (event: KeyboardEvent<HTMLButtonElement>, resize: boolean) => {
    if (maximized || !["ArrowLeft", "ArrowRight", "ArrowUp", "ArrowDown"].includes(event.key)) return;
    event.preventDefault();
    const step = event.shiftKey ? 48 : 16;
    setFrame((current) => (resize ? resizeSecondaryWindow : moveSecondaryWindow)(viewport(), current,
      event.key === "ArrowLeft" ? -step : event.key === "ArrowRight" ? step : 0,
      event.key === "ArrowUp" ? -step : event.key === "ArrowDown" ? step : 0));
  };
  return createPortal(<div ref={root} role="dialog" aria-modal="false" aria-labelledby={`${id}-title`} className={`secondary-window ${maximized ? "is-maximized" : ""}`} style={{ ...frame, zIndex: order }}
    onPointerDownCapture={() => setOrder(++windowOrder)} onFocusCapture={() => setOrder(++windowOrder)}
    onKeyDown={(event) => { if (event.key === "Escape" && !event.defaultPrevented) { event.preventDefault(); event.stopPropagation(); onClose(); } }}>
    <header className="secondary-window-header">
      <button type="button" className="secondary-window-grip" aria-label={`移动${title}窗口，方向键调整位置`} onPointerDown={(event) => start(event, "move")} onPointerMove={move} onPointerUp={end} onPointerCancel={end} onLostPointerCapture={end} onKeyDown={(event) => keyboardFrame(event, false)}><Icon name={icon} size="sm" /><strong id={`${id}-title`}>{title}</strong></button>
      <div className="secondary-window-actions"><button type="button" className="text-button" onClick={() => { setMaximized(false); setFrame(fitSecondaryWindow(viewport(), { ...frame, left: (window.innerWidth - frame.width) / 2, top: (window.innerHeight - frame.height) / 2 })); }}>居中</button>
        <button type="button" className="icon-button" aria-label={maximized ? "还原属性窗口" : "放大属性窗口"} title={maximized ? "还原" : "放大"} onClick={() => { if (maximized) { setFrame(fitSecondaryWindow(viewport(), restore.current ?? undefined)); setMaximized(false); } else { restore.current = frame; setMaximized(true); } }}><Icon name="externalLink" size="sm" /></button>
        <button ref={close} type="button" className="icon-button" aria-label={`关闭${title}`} title="关闭" onClick={onClose}><Icon name="close" size="sm" /></button>
      </div>
    </header>
    {subtitle ? <div className="secondary-window-subtitle" title={subtitle}>{subtitle}</div> : null}
    {tabs?.length ? <div className="secondary-window-tabs" role="tablist" aria-label={`${title}分类`}>{tabs.map((tab, index) => <button type="button" key={tab.id} role="tab" id={`${id}-tab-${tab.id}`} aria-selected={activeTab === tab.id} aria-controls={`${id}-body`} tabIndex={activeTab === tab.id ? 0 : -1} onClick={() => onTabChange?.(tab.id)} onKeyDown={(event) => {
      const next = event.key === "ArrowRight" ? (index + 1) % tabs.length : event.key === "ArrowLeft" ? (index + tabs.length - 1) % tabs.length : event.key === "Home" ? 0 : event.key === "End" ? tabs.length - 1 : null;
      if (next !== null) { event.preventDefault(); onTabChange?.(tabs[next]!.id); document.getElementById(`${id}-tab-${tabs[next]!.id}`)?.focus(); }
    }}>{tab.label}</button>)}</div> : null}
    <div className="secondary-window-body" id={`${id}-body`} role={tabs?.length ? "tabpanel" : undefined} aria-labelledby={tabs?.length ? `${id}-tab-${activeTab}` : undefined} tabIndex={0}>{children}</div>
    {footer ? <footer className="secondary-window-footer">{footer}</footer> : null}
    <button type="button" className="secondary-window-resize" aria-label="调整属性窗口大小，方向键缩放" title="拖动调整大小" disabled={maximized} onPointerDown={(event) => start(event, "resize")} onPointerMove={move} onPointerUp={end} onPointerCancel={end} onLostPointerCapture={end} onKeyDown={(event) => keyboardFrame(event, true)} />
  </div>, document.body);
}

export function PropertySection({ title, children }: { title: string; children: ReactNode }) {
  return <section className="property-section"><h3>{title}</h3>{children}</section>;
}

export function PropertyList({ items }: { items: { label: string; value: ReactNode; mono?: boolean }[] }) {
  return <dl className="property-list">{items.map((item) => <div key={item.label}><dt>{item.label}</dt><dd className={item.mono ? "is-mono" : undefined}>{item.value ?? "未记录"}</dd></div>)}</dl>;
}
