/**
 * 动态的自动跟随：只在用户本来就在底部时贴底。
 *
 * 否则正在往上读历史的人会被新事件反复拽回底部。这条规则很小，
 * 但混在渲染里很难被发现，所以它单独成模块。
 */

import { useCallback, useEffect, useRef, type UIEvent } from "react";

export function useFeedFollow(active: boolean, dependency: unknown) {
  const feedRef = useRef<HTMLDivElement>(null);
  const following = useRef(true);

  useEffect(() => {
    const feed = feedRef.current;
    if (active && following.current && feed) feed.scrollTop = feed.scrollHeight;
  }, [active, dependency]);

  const onScroll = useCallback((event: UIEvent<HTMLDivElement>): void => {
    const feed = event.currentTarget;
    // 64px 的宽容度：接近底部就算「还在跟」。
    following.current = feed.scrollHeight - feed.scrollTop - feed.clientHeight < 64;
  }, []);

  /** 提交一次新输入时重新贴底，因为那是用户的明确意图。 */
  const pinToBottom = useCallback((): void => {
    following.current = true;
  }, []);

  return { feedRef, onScroll, pinToBottom };
}
