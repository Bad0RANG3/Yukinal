/**
 * Agent 面板的「外壳」行为：焦点管理、Escape、收起动画与惰性化。
 *
 * 这些规则跟 Agent 一点关系都没有 —— 任何右侧抽屉都需要它们 —— 但它们极其
 * 容易写错：收起动画期间面板必须留在 DOM 里（否则动画消失）、必须 inert
 * （否则键盘还能进去）、窄屏下必须是模态（否则 Tab 会跑到背后的工作区）。
 */

import { useCallback, useEffect, useRef, useState, type AnimationEvent, type RefObject } from "react";

const OVERLAY_QUERY = "(max-width: 1150px)";
const FOCUSABLE = "button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [href], [tabindex]:not([tabindex=\"-1\"])";

export type AgentPanelShell = {
  panelRef: RefObject<HTMLElement | null>;
  closeButtonRef: RefObject<HTMLButtonElement | null>;
  panelClass: string;
  /** 收起入口：先启动退出动画，再改变 store 状态。 */
  toggle: () => void;
  onAnimationEnd: (event: AnimationEvent<HTMLElement>) => void;
};

export function useAgentPanelShell({
  agentOpen,
  close,
  togglePanel,
  onCloseStart,
  onCloseEnd,
}: {
  agentOpen: boolean;
  close: () => void;
  togglePanel: () => void;
  onCloseStart?: () => void;
  onCloseEnd?: () => void;
}): AgentPanelShell {
  const panelRef = useRef<HTMLElement>(null);
  const closeButtonRef = useRef<HTMLButtonElement>(null);
  const [isClosing, setIsClosing] = useState(false);
  /** 展开过一次之后才需要退出动画；首次挂载且收起时不该播。 */
  const hasBeenOpen = useRef(agentOpen);
  const wasOpen = useRef(agentOpen);

  useEffect(() => {
    if (agentOpen) {
      hasBeenOpen.current = true;
      setIsClosing(false);
    } else if (hasBeenOpen.current && !isClosing) {
      setIsClosing(true);
      onCloseStart?.();
    }
  }, [agentOpen, isClosing, onCloseStart]);

  useEffect(() => {
    const panel = panelRef.current;
    if (!panel) return;
    panel.inert = !agentOpen;
    const openedInOverlay = agentOpen && !wasOpen.current && window.matchMedia(OVERLAY_QUERY).matches;
    wasOpen.current = agentOpen;
    // 窄屏下打开的是模态，焦点应该直接落到收起按钮上，而不是留在背后。
    const focusFrame = openedInOverlay
      ? requestAnimationFrame(() => closeButtonRef.current?.focus({ preventScroll: true }))
      : undefined;
    if (!agentOpen) return () => { if (focusFrame !== undefined) cancelAnimationFrame(focusFrame); };

    const onKeyDown = (event: globalThis.KeyboardEvent): void => {
      // 面板之上还有真正的模态（例如添加服务器），让它先处理键盘。
      if (event.target instanceof HTMLElement && event.target.closest("dialog[open]")) return;
      if (event.key === "Escape") {
        event.preventDefault();
        close();
        return;
      }
      if (event.key !== "Tab" || !window.matchMedia(OVERLAY_QUERY).matches) return;
      const focusable = Array.from(panel.querySelectorAll<HTMLElement>(FOCUSABLE))
        .filter((element) => element.getClientRects().length > 0);
      if (!focusable.length) {
        event.preventDefault();
        return;
      }
      const first = focusable[0];
      const last = focusable.at(-1);
      if (!first || !last) return;
      if (event.shiftKey && document.activeElement === first) {
        event.preventDefault();
        last.focus();
      } else if (!event.shiftKey && document.activeElement === last) {
        event.preventDefault();
        first.focus();
      }
    };
    document.addEventListener("keydown", onKeyDown);
    return () => {
      document.removeEventListener("keydown", onKeyDown);
      if (focusFrame !== undefined) cancelAnimationFrame(focusFrame);
    };
  }, [agentOpen, close]);

  const toggle = useCallback((): void => {
    if (agentOpen) {
      setIsClosing(true);
      onCloseStart?.();
    }
    togglePanel();
  }, [agentOpen, onCloseStart, togglePanel]);

  const onAnimationEnd = useCallback(
    (event: AnimationEvent<HTMLElement>): void => {
      if (event.animationName !== "panel-exit" || agentOpen || !isClosing) return;
      hasBeenOpen.current = false;
      setIsClosing(false);
      onCloseEnd?.();
    },
    [agentOpen, isClosing, onCloseEnd],
  );

  const panelClosing = !agentOpen && (isClosing || hasBeenOpen.current);
  const panelClass = panelClosing ? "agent-panel-closing" : !agentOpen ? "agent-panel-hidden" : "";

  return { panelRef, closeButtonRef, panelClass, toggle, onAnimationEnd };
}
