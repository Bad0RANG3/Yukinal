export interface ConversationBounds {
  left: number;
  top: number;
  width: number;
  height: number;
}

export interface ConversationFrame {
  top: number;
  width: number;
  height: number;
}

const clamp = (value: number, min: number, max: number) => Math.min(max, Math.max(min, value));

/** Keep the borrowed surface inside its column, including after a window resize. */
export function fitConversationFrame(bounds: ConversationBounds, frame?: ConversationFrame): ConversationFrame {
  const width = clamp(frame?.width ?? bounds.width, Math.min(220, bounds.width), bounds.width);
  const height = clamp(frame?.height ?? Math.min(560, bounds.height * .64), Math.min(260, bounds.height), bounds.height);
  const top = clamp(frame?.top ?? bounds.top + (bounds.height - height) / 2, bounds.top, bounds.top + bounds.height - height);
  return { top, width, height };
}

export function centerConversationFrame(bounds: ConversationBounds, frame?: ConversationFrame): ConversationFrame {
  const fitted = fitConversationFrame(bounds, frame);
  return { ...fitted, top: bounds.top + (bounds.height - fitted.height) / 2 };
}

export function moveConversationFrame(bounds: ConversationBounds, frame: ConversationFrame, delta: number): ConversationFrame {
  return fitConversationFrame(bounds, { ...frame, top: frame.top + delta });
}

export function resizeConversationFrame(bounds: ConversationBounds, frame: ConversationFrame, deltaWidth: number, deltaHeight: number): ConversationFrame {
  const fitted = fitConversationFrame(bounds, frame);
  return fitConversationFrame(bounds, {
    ...fitted,
    width: fitted.width + deltaWidth,
    height: clamp(fitted.height + deltaHeight, Math.min(260, bounds.height), bounds.top + bounds.height - fitted.top),
  });
}
