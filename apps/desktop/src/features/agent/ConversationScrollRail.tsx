import { useLayoutEffect, useRef, useState, type KeyboardEvent, type RefObject } from "react";
import { CONVERSATION_LATEST_EVENT, CONVERSATION_NAVIGATE_EVENT, currentConversationTurn, conversationTurnTarget, type ConversationTurn } from "./conversation-scroll.js";
import { listenForFeedLayout } from "./feed-follow.js";

export function ConversationScrollRail({ viewportRef, viewportId, turns, contentVersion }: { viewportRef: RefObject<HTMLDivElement | null>; viewportId: string; turns: ConversationTurn[]; contentVersion?: unknown }) {
  const railRef = useRef<HTMLElement>(null);
  const listRef = useRef<HTMLDivElement>(null);
  const scheduleRef = useRef<() => void>(() => {});
  const selectedTurn = useRef<string | null>(null);
  const turnsRef = useRef(turns);
  turnsRef.current = turns;
  const [active, setActive] = useState(0);
  const [preview, setPreview] = useState<{ id: string; top: number } | null>(null);
  const previewTurn = preview ? turns.find((turn) => turn.id === preview.id) : undefined;

  useLayoutEffect(() => {
    const viewport = viewportRef.current;
    if (!viewport) return;
    let disposed = false;
    let frame = 0;
    const measure = () => {
      frame = 0;
      if (disposed || document.hidden) return;
      const bounds = viewport.getBoundingClientRect();
      const tops = turnsRef.current.map((turn) => document.getElementById(turn.id)).filter((element): element is HTMLElement => element !== null && viewport.contains(element)).map((element) => element.getBoundingClientRect().top - bounds.top + viewport.scrollTop);
      // Several short tail turns may share the same clamped scroll position.
      // Preserve an explicit clicked/key-selected turn until the user scrolls
      // the reading viewport or asks for latest; token/layout growth is not input.
      const selected = turnsRef.current.findIndex((turn) => turn.id === selectedTurn.current);
      if (selected < 0) selectedTurn.current = null;
      const next = selected >= 0 ? selected : currentConversationTurn(tops, viewport.scrollTop, viewport.clientHeight, viewport.scrollHeight);
      setActive((previous) => previous === next ? previous : next);
    };
    const schedule = () => { if (!disposed && !document.hidden && !frame) frame = requestAnimationFrame(measure); };
    scheduleRef.current = schedule;
    const visibility = () => { if (document.hidden) { if (frame) cancelAnimationFrame(frame); frame = 0; setPreview(null); } else schedule(); };
    const resize = typeof ResizeObserver === "undefined" ? null : new ResizeObserver(schedule);
    const observed = new Set<Element>();
    const observeEntries = () => {
      const children = new Set(Array.from(viewport.children));
      for (const entry of observed) if (!children.has(entry)) { resize?.unobserve(entry); observed.delete(entry); }
      for (const entry of children) if (!observed.has(entry)) { resize?.observe(entry); observed.add(entry); }
      schedule();
    };
    const mutation = typeof MutationObserver === "undefined" ? null : new MutationObserver(observeEntries);
    resize?.observe(viewport);
    observeEntries();
    mutation?.observe(viewport, { childList: true });
    const stopLayoutEvents = listenForFeedLayout(viewport, window, schedule, document.fonts ?? null);
    const readInput = () => { selectedTurn.current = null; setPreview(null); schedule(); };
    for (const type of ["wheel", "touchstart", "pointerdown", "keydown", CONVERSATION_LATEST_EVENT]) viewport.addEventListener(type, readInput, { capture: true, passive: true });
    document.addEventListener("visibilitychange", visibility);
    viewport.addEventListener("scroll", schedule, { passive: true });
    return () => {
      disposed = true;
      scheduleRef.current = () => {};
      if (frame) cancelAnimationFrame(frame);
      resize?.disconnect(); mutation?.disconnect(); stopLayoutEvents();
      for (const type of ["wheel", "touchstart", "pointerdown", "keydown", CONVERSATION_LATEST_EVENT]) viewport.removeEventListener(type, readInput, true);
      document.removeEventListener("visibilitychange", visibility);
      viewport.removeEventListener("scroll", schedule);
    };
  }, [viewportRef]);

  useLayoutEffect(() => { scheduleRef.current(); }, [contentVersion, turns]);
  useLayoutEffect(() => {
    const list = listRef.current;
    const button = list?.querySelector<HTMLButtonElement>(`[data-turn-index="${active}"]`);
    if (!list || !button) return;
    // Only the navigation's own list moves; the reading viewport stays untouched.
    if (button.offsetTop < list.scrollTop) list.scrollTop = button.offsetTop;
    else if (button.offsetTop + button.offsetHeight > list.scrollTop + list.clientHeight) list.scrollTop = button.offsetTop + button.offsetHeight - list.clientHeight;
  }, [active]);

  const jump = (index: number, focus = false) => {
    const turn = turns[index], viewport = viewportRef.current;
    const anchor = turn ? document.getElementById(turn.id) : null;
    if (!turn || !viewport || !anchor || !viewport.contains(anchor)) return;
    selectedTurn.current = turn.id;
    viewport.dispatchEvent(new Event(CONVERSATION_NAVIGATE_EVENT));
    const top = anchor.getBoundingClientRect().top - viewport.getBoundingClientRect().top + viewport.scrollTop;
    viewport.scrollTop = conversationTurnTarget(top, viewport.clientHeight, viewport.scrollHeight);
    setActive(index);
    setPreview(null);
    if (focus) listRef.current?.querySelector<HTMLButtonElement>(`[data-turn-index="${index}"]`)?.focus({ preventScroll: true });
    scheduleRef.current();
  };
  const key = (event: KeyboardEvent<HTMLButtonElement>, index: number) => {
    const target = event.key === "ArrowUp" ? Math.max(0, index - 1) : event.key === "ArrowDown" ? Math.min(turns.length - 1, index + 1) : event.key === "Home" ? 0 : event.key === "End" ? turns.length - 1 : null;
    if (target === null) return;
    event.preventDefault(); event.stopPropagation(); jump(target, true);
  };
  const showPreview = (id: string, button: HTMLButtonElement) => {
    const rail = railRef.current;
    if (rail) setPreview({ id, top: Math.max(0, Math.min(rail.clientHeight - 75, button.getBoundingClientRect().top - rail.getBoundingClientRect().top - 12)) });
  };
  if (turns.length === 0) return null;
  return <nav ref={railRef} className="conversation-scroll-rail" aria-label="对话轮次" onPointerLeave={() => setPreview(null)}>
    <span className="conversation-turn-count" aria-label={`当前第 ${Math.max(0, active) + 1} 轮，共 ${turns.length} 轮`}>{Math.max(0, active) + 1}<span aria-hidden="true">/</span>{turns.length}</span>
    <div ref={listRef} className="conversation-turn-list">
      {turns.map((turn, index) => <button key={turn.id} type="button" className="conversation-turn-mark" data-turn-index={index} aria-label={`第 ${index + 1} 轮：${turn.preview}`} aria-controls={viewportId} aria-current={active === index ? "step" : undefined} tabIndex={active === index ? 0 : -1}
        onClick={() => jump(index)} onKeyDown={(event) => key(event, index)} onPointerEnter={(event) => showPreview(turn.id, event.currentTarget)} onFocus={(event) => showPreview(turn.id, event.currentTarget)} onBlur={() => setPreview(null)}>
        <span aria-hidden="true" />
      </button>)}
    </div>
    {previewTurn ? <div className="conversation-turn-preview" role="tooltip" style={{ top: preview!.top }}><strong>第 {turns.indexOf(previewTurn) + 1} 轮</strong><span>{previewTurn.preview}</span></div> : null}
  </nav>;
}
