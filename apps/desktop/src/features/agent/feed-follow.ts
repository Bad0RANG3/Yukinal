/** Scroll intent is independent of layout: a resize is never permission to follow. */
export type FeedScrollDirection = "backward" | "forward";
export interface FeedFollowMetrics { scrollTop: number; clientHeight: number; scrollHeight: number }
interface Geometry { top: number; height: number; max: number }
export interface FeedFollowDecision { target: number | null; showJumpControl: boolean }
const EDGE_EPSILON = 1;
const finite = (value: number) => Number.isFinite(value) ? Math.max(0, value) : 0;
const geometry = (metrics: FeedFollowMetrics): Geometry => {
  const height = finite(metrics.clientHeight);
  const max = Math.max(0, finite(metrics.scrollHeight) - height);
  return { height, max, top: Math.min(max, finite(metrics.scrollTop)) };
};
const atBottom = (value: Geometry) => value.max - value.top <= EDGE_EPSILON;
const sameRange = (left: Geometry, right: Geometry) => left.height === right.height && left.max === right.max;

export class FeedFollowIntent {
  private following = true;
  private previous: Geometry | null = null;
  private input: { direction: FeedScrollDirection; geometry: Geometry } | null = null;
  private pointerActive = false;

  reset(): void {
    this.following = true;
    this.previous = null;
    this.input = null;
    this.pointerActive = false;
  }

  pinToLatest(): void { this.reset(); }

  userInput(direction: FeedScrollDirection, metrics: FeedFollowMetrics): void {
    const current = geometry(metrics);
    if (current.height <= 0 || current.max <= EDGE_EPSILON) return;
    this.input = { direction, geometry: current };
    this.previous = current;
    if (direction === "backward") this.following = false;
    // A down gesture at the actual bottom is an explicit request to follow.
    else if (!this.pointerActive && atBottom(current)) this.following = true;
  }

  /** A turn index is an explicit request to read history, including keyboard activation. */
  navigateToTurn(metrics: FeedFollowMetrics): void {
    this.following = false;
    this.pointerActive = false;
    this.input = null;
    this.previous = geometry(metrics);
  }

  beginPointer(metrics: FeedFollowMetrics): void {
    this.pointerActive = true;
    this.following = false;
    this.input = null;
    this.previous = geometry(metrics);
  }

  endPointer(metrics: FeedFollowMetrics): void {
    if (!this.pointerActive) return;
    const current = geometry(metrics);
    this.pointerActive = false;
    this.input = null;
    this.previous = current;
    this.following = current.height > 0 && atBottom(current);
  }

  cancelPointer(): void {
    this.pointerActive = false;
    this.input = null;
    // Hidden/reactivated layouts may restore scrollTop before our next frame.
    // Keep the reading intent, but never compare against stale geometry.
    this.previous = null;
  }

  scrolled(metrics: FeedFollowMetrics): void {
    const current = geometry(metrics);
    const input = this.input;
    let continuedInput: typeof this.input = null;
    if (input?.direction === "backward") this.following = false;
    else if (!this.pointerActive && input?.direction === "forward"
      && sameRange(input.geometry, current) && current.top >= input.geometry.top) {
      if (atBottom(current)) this.following = true;
      // A single wheel/touch gesture can produce several momentum scrolls.
      // Keep only monotonic forward movement within the same layout range.
      else continuedInput = { direction: "forward", geometry: current };
    } else if (this.previous && sameRange(this.previous, current) && current.top < this.previous.top) {
      // Also catches native scrollbar/assistive scrolling with no wheel event.
      this.following = false;
    }
    this.previous = current;
    // Do not let a stale downward gesture reinterpret a later resize as intent.
    this.input = continuedInput;
  }

  measure(metrics: FeedFollowMetrics): FeedFollowDecision {
    const current = geometry(metrics);
    if (this.input && !sameRange(this.input.geometry, current)) this.input = null;
    const measurable = current.height > 0;
    const target = measurable && this.following && !this.pointerActive
      && current.max - current.top > EDGE_EPSILON ? current.max : null;
    this.previous = target === null ? current : { ...current, top: target };
    return { target, showJumpControl: measurable && !this.following && current.max > EDGE_EPSILON };
  }
}

export function feedWheelDirection(event: { deltaX: number; deltaY: number; ctrlKey: boolean; shiftKey: boolean }): FeedScrollDirection | null {
  if (event.ctrlKey || event.shiftKey || !Number.isFinite(event.deltaY) || Math.abs(event.deltaY) <= Math.abs(event.deltaX)) return null;
  return event.deltaY < 0 ? "backward" : "forward";
}

export function feedTouchDirection(previous: { x: number; y: number }, next: { x: number; y: number }): FeedScrollDirection | null {
  const deltaY = previous.y - next.y;
  if (Math.abs(deltaY) <= Math.abs(previous.x - next.x)) return null;
  return deltaY < 0 ? "backward" : "forward";
}

export interface FeedKeyTarget {
  tagName: string;
  parentElement: FeedKeyTarget | null;
  getAttribute(name: string): string | null;
}
const INPUT_ROLES = new Set(["button", "textbox", "searchbox", "checkbox", "radio", "switch", "slider", "spinbutton", "combobox", "listbox", "option", "menuitem", "menuitemcheckbox", "menuitemradio", "tab", "treeitem", "gridcell"]);
const INPUT_TAGS = new Set(["INPUT", "TEXTAREA", "SELECT", "BUTTON", "SUMMARY", "AUDIO", "VIDEO"]);

export function feedKeyDirection(
  event: { key: string; shiftKey: boolean; ctrlKey: boolean; metaKey: boolean; altKey: boolean; isComposing: boolean; defaultPrevented?: boolean },
  target: FeedKeyTarget | null = null,
  onRail = false,
): FeedScrollDirection | null {
  if (event.defaultPrevented || event.isComposing || event.ctrlKey || event.metaKey || event.altKey) return null;
  for (let element = target; element; element = element.parentElement) {
    const tag = element.tagName.toUpperCase();
    const editable = element.getAttribute("contenteditable");
    const roles = element.getAttribute("role")?.toLowerCase().split(/\s+/) ?? [];
    if (INPUT_TAGS.has(tag) || (tag === "A" && element.getAttribute("href") !== null)
      || (editable !== null && editable.toLowerCase() !== "false") || roles.some((role) => INPUT_ROLES.has(role))) return null;
  }
  // The sibling rail implements arrows/page/home/end, not Space scrolling.
  if (onRail && event.key === " ") return null;
  if (event.key === "ArrowUp" || event.key === "PageUp" || event.key === "Home" || (event.key === " " && event.shiftKey)) return "backward";
  if (event.key === "ArrowDown" || event.key === "PageDown" || event.key === "End" || event.key === " ") return "forward";
  return null;
}

/** Event fallback for late media/details/fonts and missing resize observers. */
export function listenForFeedLayout(feed: EventTarget, viewport: EventTarget, schedule: () => void, fonts: EventTarget | null = null): () => void {
  const listeners = [
    { target: viewport, type: "resize", capture: false },
    { target: feed, type: "load", capture: true },
    { target: feed, type: "error", capture: true },
    { target: feed, type: "toggle", capture: true },
    ...(fonts ? [{ target: fonts, type: "loadingdone", capture: false }] : []),
  ];
  for (const { target, type, capture } of listeners) target.addEventListener(type, schedule, { capture });
  return () => {
    for (const { target, type, capture } of listeners) target.removeEventListener(type, schedule, { capture });
  };
}
