/**
 * `usePresence` —— 把 presence 状态机接到 React 上的薄适配层。
 *
 * 它只做三件事：订阅 `open` 的变化、在元素退场动画结束时收尾、在动画事件
 * 永远不来时用计时器兜底。所有判断都在 presence.ts 的纯 reducer 里。
 *
 * 用法：
 *
 *   const presence = usePresence(open, { exitAnimation: "popover-exit" });
 *   ...
 *   {presence.mounted ? (
 *     <div className={`my-popover ${presence.closing ? "is-closing" : ""}`}
 *          onAnimationEnd={presence.onAnimationEnd}>…</div>
 *   ) : null}
 *
 * 重要：`open` 仍然承担全部逻辑含义（ARIA、键盘、外部点击关闭）。`closing`
 * 只影响渲染，绝不能让 aria-expanded 之类的状态跟着它走 —— 那样弹层在退场
 * 期间会被读成「还开着」。
 */

import { useCallback, useEffect, useReducer, useRef, type AnimationEvent } from "react";

import {
  EXIT_FALLBACK_MS,
  EXIT_GRACE_MS,
  initialPresence,
  presenceReducer,
  type PresenceState,
} from "./presence.js";

export type UsePresenceOptions = {
  /**
   * 退场 `@keyframes` 的名字。只有与它同名、且事件源就是承载动画的那个元素
   * 时才会提前收尾 —— `animationend` 会从子元素冒泡上来，不校验的话子元素
   * 任何一次动画都会把弹层提前卸载掉。
   */
  exitAnimation: string;
  /** 与 CSS 里那条退场动画的时长一致，用作 animationend 不出现时的兜底。 */
  exitMs?: number;
};

export type Presence = PresenceState & {
  /** 挂在承载退场动画的元素上。 */
  onAnimationEnd: (event: AnimationEvent<HTMLElement>) => void;
};

export function usePresence(open: boolean, { exitAnimation, exitMs = EXIT_FALLBACK_MS }: UsePresenceOptions): Presence {
  const [state, dispatch] = useReducer(presenceReducer, open, initialPresence);
  // 计时器只在「正在退场」时存在；重新打开或卸载都要清掉，否则一个迟到的
  // settled 会把刚刚重新打开的弹层卸载掉。
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);

  const clearTimer = useCallback(() => {
    if (timer.current === null) return;
    clearTimeout(timer.current);
    timer.current = null;
  }, []);

  useEffect(() => {
    dispatch({ type: open ? "open" : "close" });
  }, [open]);

  useEffect(() => {
    if (!state.closing) {
      clearTimer();
      return;
    }
    timer.current = setTimeout(() => {
      timer.current = null;
      dispatch({ type: "settled" });
    }, exitMs + EXIT_GRACE_MS);
    return clearTimer;
  }, [state.closing, exitMs, clearTimer]);

  // 卸载时不留悬挂计时器。
  useEffect(() => clearTimer, [clearTimer]);

  const onAnimationEnd = useCallback(
    (event: AnimationEvent<HTMLElement>): void => {
      if (event.target !== event.currentTarget) return;
      if (event.animationName !== exitAnimation) return;
      clearTimer();
      dispatch({ type: "settled" });
    },
    [exitAnimation, clearTimer],
  );

  return { ...state, onAnimationEnd };
}
