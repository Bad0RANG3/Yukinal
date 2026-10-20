/**
 * Follow only an explicit reading intent. Content/viewport changes never
 * re-enable following after the user has moved away from the latest entry.
 */
import { CONVERSATION_LATEST_EVENT, CONVERSATION_NAVIGATE_EVENT } from "./conversation-scroll.js";
import { useCallback, useLayoutEffect, useRef, useState, type UIEvent } from "react";
import { FeedFollowIntent, feedKeyDirection, feedTouchDirection, feedWheelDirection, listenForFeedLayout, type FeedScrollDirection } from "./feed-follow.js";

export function useFeedFollow(active: boolean, dependency: unknown, scope: unknown = null) {
  const feedRef = useRef<HTMLDivElement>(null);
  const frameRef = useRef<HTMLDivElement>(null);
  const intent = useRef(new FeedFollowIntent());
  const scheduleRef = useRef<() => void>(() => {});
  const activeRef = useRef(active);
  activeRef.current = active;
  const [showJumpControl, setShowJumpControl] = useState(false);
  const shown = useRef(false);
  const publish = useCallback((show: boolean) => {
    if (shown.current === show) return;
    shown.current = show;
    setShowJumpControl(show);
  }, []);

  useLayoutEffect(() => {
    const feed = feedRef.current;
    const frame = frameRef.current;
    if (!active || !feed || !frame) return;
    let disposed = false;
    let animationFrame: number | null = null;
    let pointerId: number | null = null;
    let touch: { id: number; x: number; y: number } | null = null;
    const measure = () => {
      animationFrame = null;
      if (disposed || document.hidden) return;
      const decision = intent.current.measure(feed);
      if (decision.target !== null) feed.scrollTop = decision.target;
      publish(decision.showJumpControl);
    };
    const schedule = () => {
      if (!disposed && !document.hidden && animationFrame === null) animationFrame = requestAnimationFrame(measure);
    };
    scheduleRef.current = schedule;
    // Listen on the existing frame in capture phase: the custom rail handles
    // its own scrolling and stops bubbling; its intent must still reach us.
    const direction = (value: FeedScrollDirection | null) => {
      if (!value) return;
      intent.current.userInput(value, feed);
      schedule();
    };
    const railTarget = (target: EventTarget | null) => target instanceof Element && Boolean(target.closest(".conversation-scroll-rail"));
    const reachesFeed = (target: EventTarget | null, value: FeedScrollDirection) => {
      if (railTarget(target)) return true;
      if (!(target instanceof Element) || !feed.contains(target)) return false;
      // Scrolling an inner code/log preview is not scrolling the conversation.
      for (let element: Element | null = target; element && element !== feed; element = element.parentElement) {
        if (element.scrollHeight <= element.clientHeight) continue;
        const overflow = getComputedStyle(element).overflowY;
        if (overflow !== "auto" && overflow !== "scroll") continue;
        if (value === "backward" ? element.scrollTop > 0 : element.scrollTop + element.clientHeight < element.scrollHeight - 1) return false;
      }
      return true;
    };
    const wheel = (event: WheelEvent) => {
      const value = feedWheelDirection(event);
      if (value && reachesFeed(event.target, value)) direction(value);
    };
    const touchStart = (event: TouchEvent) => {
      const point = event.touches.length === 1 ? event.touches[0] : undefined;
      touch = point ? { id: point.identifier, x: point.clientX, y: point.clientY } : null;
    };
    const touchMove = (event: TouchEvent) => {
      if (!touch || event.touches.length !== 1) return;
      const point = event.touches[0];
      if (!point || point.identifier !== touch.id) return;
      const next = { id: touch.id, x: point.clientX, y: point.clientY };
      const value = feedTouchDirection(touch, next);
      touch = next;
      if (value && reachesFeed(event.target, value)) direction(value);
    };
    const touchEnd = () => { touch = null; };
    const key = (event: KeyboardEvent) => {
      const target = event.target;
      if (!(target instanceof Element)) return;
      const value = feedKeyDirection(event, target, railTarget(target));
      if (value && reachesFeed(target, value)) direction(value);
    };
    const pointerDown = (event: PointerEvent) => {
      if (event.button !== 0 || !event.isPrimary) return;
      const bounds = feed.getBoundingClientRect();
      const gutter = feed.offsetWidth - feed.clientWidth - 2 * feed.clientLeft;
      const nativeScrollbar = event.target === feed && gutter > 0 && event.clientX >= bounds.left + feed.clientLeft + feed.clientWidth;
      if (!railTarget(event.target) && !nativeScrollbar) return;
      pointerId = event.pointerId;
      intent.current.beginPointer(feed);
      schedule();
    };
    const pointerEnd = (event: PointerEvent) => {
      if (pointerId !== event.pointerId) return;
      pointerId = null;
      if (event.type === "pointerup") intent.current.endPointer(feed);
      else intent.current.cancelPointer();
      schedule();
    };
    const navigateToTurn = () => { intent.current.navigateToTurn(feed); schedule(); };
    const visibility = () => {
      if (document.hidden) {
        if (animationFrame !== null) cancelAnimationFrame(animationFrame);
        animationFrame = null;
        touch = null;
        pointerId = null;
        intent.current.cancelPointer();
      } else schedule();
    };

    const resize = typeof ResizeObserver === "undefined" ? null : new ResizeObserver(schedule);
    const observed = new Set<Element>();
    const observeEntries = () => {
      const entries = new Set(Array.from(feed.children));
      for (const entry of observed) if (!entries.has(entry)) { resize?.unobserve(entry); observed.delete(entry); }
      for (const entry of entries) if (!observed.has(entry)) { resize?.observe(entry); observed.add(entry); }
      schedule();
    };
    resize?.observe(feed);
    observeEntries();
    const mutation = typeof MutationObserver === "undefined" ? null : new MutationObserver(observeEntries);
    mutation?.observe(feed, { childList: true });
    const stopLayoutEvents = listenForFeedLayout(feed, window, schedule, document.fonts ?? null);
    feed.addEventListener(CONVERSATION_NAVIGATE_EVENT, navigateToTurn);
    frame.addEventListener("wheel", wheel, { capture: true, passive: true });
    frame.addEventListener("touchstart", touchStart, { capture: true, passive: true });
    frame.addEventListener("touchmove", touchMove, { capture: true, passive: true });
    frame.addEventListener("touchend", touchEnd, true);
    frame.addEventListener("touchcancel", touchEnd, true);
    frame.addEventListener("keydown", key, true);
    frame.addEventListener("pointerdown", pointerDown, true);
    frame.addEventListener("lostpointercapture", pointerEnd, true);
    document.addEventListener("pointerup", pointerEnd, true);
    document.addEventListener("pointercancel", pointerEnd, true);
    document.addEventListener("visibilitychange", visibility);
    return () => {
      disposed = true;
      intent.current.cancelPointer();
      scheduleRef.current = () => {};
      if (animationFrame !== null) cancelAnimationFrame(animationFrame);
      resize?.disconnect();
      mutation?.disconnect();
      stopLayoutEvents();
      feed.removeEventListener(CONVERSATION_NAVIGATE_EVENT, navigateToTurn);
      frame.removeEventListener("wheel", wheel, true);
      frame.removeEventListener("touchstart", touchStart, true);
      frame.removeEventListener("touchmove", touchMove, true);
      frame.removeEventListener("touchend", touchEnd, true);
      frame.removeEventListener("touchcancel", touchEnd, true);
      frame.removeEventListener("keydown", key, true);
      frame.removeEventListener("pointerdown", pointerDown, true);
      frame.removeEventListener("lostpointercapture", pointerEnd, true);
      document.removeEventListener("pointerup", pointerEnd, true);
      document.removeEventListener("pointercancel", pointerEnd, true);
      document.removeEventListener("visibilitychange", visibility);
    };
  }, [active, publish]);

  useLayoutEffect(() => {
    intent.current.reset();
    publish(false);
    scheduleRef.current();
  }, [scope, publish]);

  useLayoutEffect(() => { if (active) scheduleRef.current(); }, [active, dependency]);

  const onScroll = useCallback((event: UIEvent<HTMLDivElement>): void => {
    if (!activeRef.current || document.hidden) return;
    intent.current.scrolled(event.currentTarget);
    scheduleRef.current();
  }, []);

  /** A submitted input or the explicit latest button restores following. */
  const pinToBottom = useCallback((): void => {
    feedRef.current?.dispatchEvent(new Event(CONVERSATION_LATEST_EVENT));
    intent.current.pinToLatest();
    publish(false);
    scheduleRef.current();
  }, [publish]);

  return { feedRef, frameRef, onScroll, pinToBottom, showJumpControl };
}
