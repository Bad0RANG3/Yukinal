/** Geometry shared by the tick rail's indicator and its pointer controls. */
export interface ConversationScrollGeometry {
  enabled: boolean;
  maxScroll: number;
  progress: number;
  trackHeight: number;
  thumbHeight: number;
  thumbTop: number;
}

const positive = (value: number) => Number.isFinite(value) ? Math.max(0, value) : 0;
const clamp = (value: number, max: number) => Math.min(max, positive(value));

export function conversationScrollGeometry(viewportHeight: number, contentHeight: number, scrollTop: number, railHeight: number): ConversationScrollGeometry {
  const viewport = positive(viewportHeight);
  const content = positive(contentHeight);
  const trackHeight = positive(railHeight);
  const maxScroll = Math.max(0, content - viewport);
  const enabled = viewport > 0 && maxScroll > 1 && trackHeight > 0;
  const progress = enabled ? clamp(scrollTop, maxScroll) / maxScroll : 0;
  const minimumThumb = Math.min(36, trackHeight / 2);
  const thumbHeight = enabled ? Math.min(trackHeight, Math.max(minimumThumb, trackHeight * viewport / content)) : trackHeight;
  return { enabled, maxScroll, progress, trackHeight, thumbHeight, thumbTop: progress * (trackHeight - thumbHeight) };
}

export function conversationScrollTarget(pointerY: number, grabOffset: number, geometry: ConversationScrollGeometry): number {
  const travel = geometry.trackHeight - geometry.thumbHeight;
  if (!geometry.enabled || travel <= 0) return 0;
  return clamp(pointerY - grabOffset, travel) / travel * geometry.maxScroll;
}
