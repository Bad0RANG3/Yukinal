import { useLayoutEffect, useRef, useState, type KeyboardEvent, type PointerEvent, type RefObject } from "react";
import { conversationScrollGeometry, conversationScrollTarget } from "./conversation-scroll.js";

export function ConversationScrollRail({ viewportRef, viewportId }: { viewportRef: RefObject<HTMLDivElement | null>; viewportId: string }) {
  const railRef = useRef<HTMLDivElement>(null);
  const drag = useRef<{ pointerId: number; grabOffset: number } | null>(null);
  const [geometry, setGeometry] = useState(() => conversationScrollGeometry(0, 0, 0, 0));

  useLayoutEffect(() => {
    const viewport = viewportRef.current;
    const rail = railRef.current;
    if (!viewport || !rail) return;
    let frame = 0;
    const measure = () => {
      frame = 0;
      const next = conversationScrollGeometry(viewport.clientHeight, viewport.scrollHeight, viewport.scrollTop, rail.clientHeight);
      setGeometry((previous) => previous.enabled === next.enabled && previous.maxScroll === next.maxScroll && previous.progress === next.progress && previous.trackHeight === next.trackHeight && previous.thumbHeight === next.thumbHeight ? previous : next);
    };
    const schedule = () => { if (!frame) frame = requestAnimationFrame(measure); };
    const resize = new ResizeObserver(schedule);
    const observed = new Set<Element>();
    // Entries remain direct children so alignment and message animations stay intact.
    // Watching their sizes also catches streaming text, images and open details.
    const observeEntries = () => {
      const children = new Set(Array.from(viewport.children));
      for (const entry of observed) if (!children.has(entry)) { resize.unobserve(entry); observed.delete(entry); }
      for (const entry of children) if (!observed.has(entry)) { resize.observe(entry); observed.add(entry); }
      schedule();
    };
    const mutation = new MutationObserver(observeEntries);
    resize.observe(viewport);
    resize.observe(rail);
    observeEntries();
    mutation.observe(viewport, { childList: true });
    viewport.addEventListener("scroll", schedule, { passive: true });
    const wheel = (event: WheelEvent) => {
      if (!event.deltaY || event.ctrlKey || viewport.scrollHeight <= viewport.clientHeight) return;
      const unit = event.deltaMode === 1 ? 20 : event.deltaMode === 2 ? viewport.clientHeight : 1;
      viewport.scrollTop += event.deltaY * unit;
      event.preventDefault();
      event.stopPropagation();
    };
    rail.addEventListener("wheel", wheel, { passive: false });
    return () => {
      if (frame) cancelAnimationFrame(frame);
      resize.disconnect();
      mutation.disconnect();
      viewport.removeEventListener("scroll", schedule);
      rail.removeEventListener("wheel", wheel);
    };
  }, [viewportRef]);

  const pointerGeometry = () => {
    const viewport = viewportRef.current;
    const bounds = railRef.current?.getBoundingClientRect();
    return viewport && bounds ? { viewport, bounds, geometry: conversationScrollGeometry(viewport.clientHeight, viewport.scrollHeight, viewport.scrollTop, bounds.height) } : null;
  };
  const start = (event: PointerEvent<HTMLDivElement>) => {
    if (event.button !== 0 || !event.isPrimary) return;
    const current = pointerGeometry();
    if (!current?.geometry.enabled) return;
    const position = event.clientY - current.bounds.top;
    const { thumbTop, thumbHeight } = current.geometry;
    const grabOffset = position >= thumbTop && position <= thumbTop + thumbHeight ? position - thumbTop : thumbHeight / 2;
    drag.current = { pointerId: event.pointerId, grabOffset };
    event.currentTarget.setPointerCapture(event.pointerId);
    event.currentTarget.focus({ preventScroll: true });
    current.viewport.scrollTop = conversationScrollTarget(position, grabOffset, current.geometry);
    event.preventDefault();
  };
  const move = (event: PointerEvent<HTMLDivElement>) => {
    if (drag.current?.pointerId !== event.pointerId) return;
    const current = pointerGeometry();
    if (current) current.viewport.scrollTop = conversationScrollTarget(event.clientY - current.bounds.top, drag.current.grabOffset, current.geometry);
    event.preventDefault();
  };
  const end = (event: PointerEvent<HTMLDivElement>) => {
    if (drag.current?.pointerId !== event.pointerId) return;
    drag.current = null;
    if (event.currentTarget.hasPointerCapture(event.pointerId)) event.currentTarget.releasePointerCapture(event.pointerId);
  };
  const key = (event: KeyboardEvent<HTMLDivElement>) => {
    const viewport = viewportRef.current;
    if (!viewport || !geometry.enabled) return;
    const targets: Record<string, number> = {
      ArrowUp: viewport.scrollTop - 40,
      ArrowDown: viewport.scrollTop + 40,
      PageUp: viewport.scrollTop - viewport.clientHeight * .9,
      PageDown: viewport.scrollTop + viewport.clientHeight * .9,
      Home: 0,
      End: viewport.scrollHeight,
    };
    if (!Object.hasOwn(targets, event.key)) return;
    viewport.scrollTop = targets[event.key]!;
    event.preventDefault();
    event.stopPropagation();
  };

  return <div ref={railRef} className="conversation-scroll-rail" data-overflow={geometry.enabled} role="scrollbar" aria-label="对话滚动位置" aria-controls={viewportId} aria-orientation="vertical" aria-valuemin={0} aria-valuemax={100} aria-valuenow={Math.round(geometry.progress * 100)} aria-hidden={!geometry.enabled || undefined} tabIndex={geometry.enabled ? 0 : -1} onPointerDown={start} onPointerMove={move} onPointerUp={end} onPointerCancel={end} onLostPointerCapture={() => { drag.current = null; }} onKeyDown={key}>
    <span className="conversation-scroll-ticks" aria-hidden="true" />
    <span className="conversation-scroll-ticks is-current" aria-hidden="true" style={{ clipPath: `inset(${geometry.thumbTop}px 0 ${Math.max(0, geometry.trackHeight - geometry.thumbTop - geometry.thumbHeight)}px 0)` }} />
  </div>;
}
