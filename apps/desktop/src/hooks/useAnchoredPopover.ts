import { useLayoutEffect, type RefObject } from "react";

interface Rect { left: number; top: number; right: number; bottom: number }

/** Fit the surface to the viewport while keeping its edge beside the trigger. */
export function anchoredPopoverPosition(anchor: Rect, size: { width: number; height: number }, viewport: { width: number; height: number }, align: "start" | "end" = "start") {
  const margin = 12, gap = 8;
  const width = Math.min(size.width, Math.max(0, viewport.width - margin * 2));
  const above = Math.max(0, anchor.top - margin - gap);
  const below = Math.max(0, viewport.height - anchor.bottom - margin - gap);
  const up = above >= Math.min(size.height, 240) || above >= below;
  const maxHeight = Math.max(0, Math.min(420, up ? above : below));
  const height = Math.min(size.height, maxHeight);
  const left = Math.max(margin, Math.min(align === "end" ? anchor.right - width : anchor.left, viewport.width - width - margin));
  return { left, top: up ? anchor.top - gap - height : anchor.bottom + gap, width, maxHeight };
}

/** Native top-layer rendering escapes the chat's clipping without moving its DOM. */
export function useAnchoredPopover(open: boolean, anchorRef: RefObject<HTMLElement | null>, surfaceRef: RefObject<HTMLElement | null>, width = 340, align: "start" | "end" = "start") {
  useLayoutEffect(() => {
    const surface = surfaceRef.current, anchor = anchorRef.current;
    if (!open || !surface || !anchor) return;
    const place = () => {
      if (!anchor.isConnected || !surface.isConnected) return;
      const viewport = { width: document.documentElement.clientWidth, height: window.innerHeight };
      surface.style.position = "fixed";
      surface.style.inset = "auto";
      surface.style.margin = "0";
      surface.style.width = `${Math.min(width, Math.max(0, viewport.width - 24))}px`;
      surface.style.maxWidth = "none";
      const position = anchoredPopoverPosition(anchor.getBoundingClientRect(), { width, height: surface.scrollHeight + surface.offsetHeight - surface.clientHeight }, viewport, align);
      surface.style.left = `${position.left}px`;
      surface.style.top = `${position.top}px`;
      surface.style.maxHeight = `${position.maxHeight}px`;
      surface.style.overflowY = "auto";
    };
    surface.showPopover();
    place();
    const observer = new ResizeObserver(place);
    observer.observe(anchor); observer.observe(surface);
    window.addEventListener("resize", place);
    document.addEventListener("scroll", place, true);
    return () => {
      observer.disconnect();
      window.removeEventListener("resize", place);
      document.removeEventListener("scroll", place, true);
      if (surface.isConnected && surface.matches(":popover-open")) surface.hidePopover();
    };
  }, [open, anchorRef, surfaceRef, width, align]);
}
