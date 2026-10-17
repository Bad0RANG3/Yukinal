export type WindowFrame = { left: number; top: number; width: number; height: number };
export type WindowViewport = { width: number; height: number };
const EDGE = 16;

/** Keep the title and resize handle reachable after moving or changing monitors. */
export function fitSecondaryWindow(viewport: WindowViewport, frame?: WindowFrame): WindowFrame {
  const availableWidth = Math.max(1, viewport.width - EDGE * 2);
  const availableHeight = Math.max(1, viewport.height - EDGE * 2);
  const width = Math.min(availableWidth, Math.max(Math.min(360, availableWidth), frame?.width ?? 640));
  const height = Math.min(availableHeight, Math.max(Math.min(300, availableHeight), frame?.height ?? 560));
  return { width, height,
    left: Math.max(EDGE, Math.min(frame?.left ?? (viewport.width - width) / 2, viewport.width - width - EDGE)),
    top: Math.max(EDGE, Math.min(frame?.top ?? (viewport.height - height) / 2, viewport.height - height - EDGE)),
  };
}

export function moveSecondaryWindow(viewport: WindowViewport, frame: WindowFrame, dx: number, dy: number): WindowFrame {
  return fitSecondaryWindow(viewport, { ...frame, left: frame.left + dx, top: frame.top + dy });
}

export function resizeSecondaryWindow(viewport: WindowViewport, frame: WindowFrame, dx: number, dy: number): WindowFrame {
  return fitSecondaryWindow(viewport, { ...frame,
    width: Math.min(viewport.width - frame.left - EDGE, frame.width + dx),
    height: Math.min(viewport.height - frame.top - EDGE, frame.height + dy),
  });
}
