import { useLayoutEffect, useRef, useState, type CSSProperties, type KeyboardEvent, type PointerEvent, type ReactNode } from "react";
import { Icon } from "../../components/Icon.js";
import { centerConversationFrame, fitConversationFrame, moveConversationFrame, resizeConversationFrame, type ConversationBounds, type ConversationFrame } from "./conversation-frame.js";
import { useWorkpagesStore } from "./workpages-store.js";

/** The conversation stays mounted as information moves into and out of view. */
export function WorkspaceLayers({ tools, children }: { tools?: ReactNode; children: ReactNode }) {
  const informationOpen = useWorkpagesStore((state) => !state.collapsed && (state.pages.length > 0 || state.operationsOpen));
  const expanded = useWorkpagesStore((state) => Boolean(state.expandedId));
  if (!tools) return children;
  return <div className={`workspace-layers ${informationOpen ? "has-information" : ""} ${expanded ? "information-expanded" : ""}`}>
    {tools}
    {children}
  </div>;
}

interface Gesture {
  pointerId: number;
  mode: "move" | "resize";
  x: number;
  y: number;
  frame: ConversationFrame;
  bounds: ConversationBounds;
}

export function WorkspaceConversation({ enabled, empty, children }: { enabled: boolean; empty: boolean; children: ReactNode }) {
  const informationOpen = useWorkpagesStore((state) => !state.collapsed && (state.pages.length > 0 || state.operationsOpen));
  const conversationRef = useRef<HTMLDivElement>(null);
  const gestureRef = useRef<Gesture | null>(null);
  const [bounds, setBounds] = useState<ConversationBounds | null>(null);
  const [placement, setPlacement] = useState<ConversationFrame>();
  const [interacting, setInteracting] = useState(false);

  useLayoutEffect(() => {
    if (!enabled || !informationOpen) { setBounds(null); return; }
    const panel = conversationRef.current?.closest<HTMLElement>(".agent-panel");
    const inspector = panel?.closest(".app-shell")?.querySelector<HTMLElement>(".workspace-inspector");
    if (!panel || !inspector) return;
    const measure = () => {
      const side = inspector.getBoundingClientRect();
      const anchor = panel.getBoundingClientRect();
      // Compact layouts already hide the reference column; keep their original tracks.
      if (side.width <= 32 || panel.clientHeight <= 64 || side.left < anchor.right - 2) { setBounds(null); return; }
      const next = { left: side.left - anchor.left - panel.clientLeft + 16, top: 16, width: side.width - 32, height: panel.clientHeight - 32 };
      setBounds((previous) => previous && Object.keys(next).every((key) => previous[key as keyof ConversationBounds] === next[key as keyof ConversationBounds]) ? previous : next);
    };
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(panel); observer.observe(inspector);
    window.addEventListener("resize", measure);
    return () => { observer.disconnect(); window.removeEventListener("resize", measure); };
  }, [enabled, informationOpen]);

  const frame = bounds ? fitConversationFrame(bounds, placement) : null;
  const style = bounds && frame ? {
    "--conversation-left": `${bounds.left + (bounds.width - frame.width) / 2}px`,
    "--conversation-top": `${frame.top}px`,
    "--conversation-width": `${frame.width}px`,
    "--conversation-height": `${frame.height}px`,
  } as CSSProperties : undefined;

  const begin = (event: PointerEvent<HTMLButtonElement>, mode: Gesture["mode"]) => {
    if (!bounds || !frame || event.button !== 0) return;
    event.currentTarget.setPointerCapture(event.pointerId);
    gestureRef.current = { pointerId: event.pointerId, mode, x: event.clientX, y: event.clientY, frame, bounds };
    setInteracting(true);
  };
  const move = (event: PointerEvent<HTMLButtonElement>) => {
    const gesture = gestureRef.current;
    if (!gesture || gesture.pointerId !== event.pointerId) return;
    setPlacement(gesture.mode === "move"
      ? moveConversationFrame(gesture.bounds, gesture.frame, event.clientY - gesture.y)
      : resizeConversationFrame(gesture.bounds, gesture.frame, 2 * (event.clientX - gesture.x), event.clientY - gesture.y));
  };
  const end = (event: PointerEvent<HTMLButtonElement>, cancelled = false) => {
    const gesture = gestureRef.current;
    if (!gesture || gesture.pointerId !== event.pointerId) return;
    if (cancelled) setPlacement(gesture.frame);
    gestureRef.current = null;
    setInteracting(false);
    if (event.currentTarget.hasPointerCapture(event.pointerId)) event.currentTarget.releasePointerCapture(event.pointerId);
  };
  const onKey = (event: KeyboardEvent<HTMLButtonElement>, mode: Gesture["mode"]) => {
    if (!bounds || !frame) return;
    const step = event.shiftKey ? 48 : 16;
    if (event.key === "Home") setPlacement(centerConversationFrame(bounds, mode === "move" ? frame : undefined));
    else if (mode === "move" && (event.key === "ArrowUp" || event.key === "ArrowDown")) setPlacement(moveConversationFrame(bounds, frame, event.key === "ArrowUp" ? -step : step));
    else if (mode === "resize" && ["ArrowUp", "ArrowDown", "ArrowLeft", "ArrowRight"].includes(event.key)) setPlacement(resizeConversationFrame(bounds, frame, event.key === "ArrowLeft" ? -step : event.key === "ArrowRight" ? step : 0, event.key === "ArrowUp" ? -step : event.key === "ArrowDown" ? step : 0));
    else return;
    event.preventDefault();
  };
  const gestureProps = (mode: Gesture["mode"]) => ({
    onPointerDown: (event: PointerEvent<HTMLButtonElement>) => begin(event, mode),
    onPointerMove: move,
    onPointerUp: (event: PointerEvent<HTMLButtonElement>) => end(event),
    onPointerCancel: (event: PointerEvent<HTMLButtonElement>) => end(event, true),
    onLostPointerCapture: () => { gestureRef.current = null; setInteracting(false); },
    onKeyDown: (event: KeyboardEvent<HTMLButtonElement>) => onKey(event, mode),
  });

  return <div ref={conversationRef} style={style} data-floating={Boolean(bounds && enabled && informationOpen)} data-interacting={interacting} className={`agent-conversation ${empty ? "is-empty" : "has-messages"}`}>
    {enabled ? <header className="workspace-conversation-placement">
      <button type="button" disabled={!bounds} className="workspace-conversation-grip" aria-label="上下移动临时对话" title="上下拖动；方向键移动，Home 居中" {...gestureProps("move")}><Icon name="agent" size="sm" /><span>临时对话</span></button>
      <div className="workspace-conversation-actions">
        <button type="button" className="text-button workspace-conversation-center" title="将临时对话居中" onClick={() => bounds && setPlacement(centerConversationFrame(bounds, frame ?? undefined))}>居中</button>
        <button type="button" className="text-button" title="结束借用右侧，回到中间对话" onClick={() => {
          useWorkpagesStore.getState().setCollapsed(true);
          requestAnimationFrame(() => conversationRef.current?.querySelector("textarea")?.focus({ preventScroll: true }));
        }}>返回中间<Icon name="chevronRight" size="sm" /></button>
      </div>
    </header> : null}
    {children}
    {enabled ? <button type="button" className="workspace-conversation-resize" aria-label="调整临时对话大小" title="拖动调整大小；方向键微调，Home 恢复默认大小" {...gestureProps("resize")} /> : null}
  </div>;
}
