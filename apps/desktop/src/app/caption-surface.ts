import type { WindowShellSurface } from "@yukinal/shared";

/** Adjacent fractional grid tracks can differ by a subpixel after layout. */
export function captionEdges(left: number, right: number, inspectorLeft: number, viewportWidth: number) {
  if (![left, right, inspectorLeft, viewportWidth].every(Number.isFinite) ||
      viewportWidth <= 0 || left < 0 || left >= right ||
      right - inspectorLeft > 1 || inspectorLeft - viewportWidth > 1) return null;
  const edge = Math.min(inspectorLeft, viewportWidth);
  const fittedRight = Math.min(right, edge);
  return left < fittedRight ? { left, right: fittedRight, inspectorLeft: edge, viewportWidth } : null;
}

/** Flatten the translucent CSS edge onto its opaque content surface for GDI. */
function opaqueColor(value: string, background: readonly number[] = [0, 0, 0]): string {
  const parts = value.match(/[\d.]+/gu)?.map(Number) ?? [];
  const alpha = parts[3] ?? 1;
  return `#${[0, 1, 2].map((channel) => Math.round(
    (parts[channel] ?? 0) * alpha + (background[channel] ?? 0) * (1 - alpha),
  ).toString(16).padStart(2, "0")).join("")}`;
}

export function readCaptionSurface(root: HTMLElement): WindowShellSurface {
  // At compact widths the inspector can replace AI, and the native buttons
  // occupy its top-right space. Keep the complete native caption in that mode.
  if (!window.matchMedia("(min-width: 1181px)").matches) return null;
  const panel = root.querySelector<HTMLElement>(".agent-panel.agent-workspace");
  const bounds = panel?.getBoundingClientRect();
  const inspector = root.querySelector<HTMLElement>(".workspace-inspector")?.getBoundingClientRect();
  if (!panel || !bounds || !inspector || bounds.width === 0 || bounds.height === 0) return null;
  const edges = captionEdges(bounds.left, bounds.right, inspector.left, document.documentElement.clientWidth);
  if (!edges) return null;
  const style = getComputedStyle(panel);
  const background = opaqueColor(style.backgroundColor);
  const channels = [1, 3, 5].map((offset) => Number.parseInt(background.slice(offset, offset + 2), 16));
  return {
    ...edges,
    radius: Number.parseFloat(style.borderBottomLeftRadius) || 0,
    background,
    border: opaqueColor(style.borderLeftColor, channels),
  };
}
