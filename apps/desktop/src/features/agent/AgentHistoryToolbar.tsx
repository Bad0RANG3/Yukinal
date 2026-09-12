/**
 * 对话记录的搜索框与状态筛选标签条。
 *
 * 两个控件在这里是一件事：它们都在改「看哪一批对话」这个问题，而且搜到的东西与标签上
 * 的数字必须是同一个关键字下的结果（计数由宿主侧按同一个关键字统计）。
 *
 * 标签条按 ARIA 的 tabs 模式用：左右箭头换筛选、`Home` / `End` 走两端，只有选中的
 * 那个进 Tab 序列。摆出 `role="tab"` 却不实现这套键盘行为，等于对读屏用户撒谎 ——
 * 他们会按箭头，然后什么都不发生。
 */

import { useRef, type RefObject } from "react";

import type { ChatSessionCounts } from "@yukinal/shared";

import { Icon } from "../../components/Icon.js";
import { historyFilterTotal, type HistoryFilter } from "./history.js";

const FILTERS: { key: HistoryFilter; label: string }[] = [
  { key: "active", label: "进行中" },
  { key: "archived", label: "已归档" },
  { key: "all", label: "全部" },
];

export function AgentHistoryToolbar({
  query,
  onSearch,
  onClearSearch,
  filter,
  counts,
  isFetching,
  onSelectFilter,
  searchRef,
}: {
  query: string;
  /** 打字路径：换关键字同时把行游标与行内编辑状态归零。 */
  onSearch: (value: string) => void;
  /**
   * 「清空搜索」按钮的路径，与打字**不是**同一条：它只清掉关键字本身。两件事拆成两个
   * 回调，是因为它们本来就不同 —— 合并成一个会顺手改掉清空时行的游标。
   */
  onClearSearch: () => void;
  filter: HistoryFilter;
  /** 计数为空只说明「这一份数字还没有」；换关键字时会短暂为空。 */
  counts: ChatSessionCounts | null;
  isFetching: boolean;
  onSelectFilter: (filter: HistoryFilter) => void;
  /** 聚焦搜索框的快捷键（`/`）由面板外壳处理，所以 ref 从那里传下来。 */
  searchRef: RefObject<HTMLInputElement | null>;
}) {
  const tabsRef = useRef<HTMLDivElement>(null);

  const moveFilter = (target: number): void => {
    const next = FILTERS[(target + FILTERS.length) % FILTERS.length];
    if (!next) return;
    onSelectFilter(next.key);
    const buttons = Array.from(tabsRef.current?.querySelectorAll<HTMLButtonElement>('[role="tab"]') ?? []);
    buttons[FILTERS.indexOf(next)]?.focus();
  };

  return (
    <>
      <label className="agent-history-search">
        <Icon name="search" size="sm" />
        <input
          ref={searchRef}
          type="search"
          aria-label="搜索对话记录"
          aria-keyshortcuts="/"
          title="搜索标题与消息正文（按 / 聚焦）"
          value={query}
          onChange={(event) => onSearch(event.target.value)}
          placeholder="搜索标题或消息内容"
          maxLength={200}
        />
        {isFetching ? <span className="loading-spinner agent-history-search-spinner" /> : null}
        {query ? (
          <button type="button" aria-label="清空搜索" title="清空搜索" onClick={onClearSearch}>
            <Icon name="close" size="xs" />
          </button>
        ) : null}
      </label>

      <div
        className="agent-history-tabs"
        role="tablist"
        aria-label="对话状态"
        ref={tabsRef}
        onKeyDown={(event) => {
          const index = FILTERS.findIndex((item) => item.key === filter);
          if (event.key === "ArrowRight" || event.key === "ArrowDown") {
            event.preventDefault();
            moveFilter(index + 1);
          } else if (event.key === "ArrowLeft" || event.key === "ArrowUp") {
            event.preventDefault();
            moveFilter(index - 1);
          } else if (event.key === "Home") {
            event.preventDefault();
            moveFilter(0);
          } else if (event.key === "End") {
            event.preventDefault();
            moveFilter(FILTERS.length - 1);
          }
        }}
      >
        {FILTERS.map(({ key, label }) => (
          <button
            key={key}
            type="button"
            role="tab"
            id={`history-tab-${key}`}
            aria-selected={filter === key}
            aria-controls="agent-history-panel"
            // 整个标签条只占一个 Tab 位（APG 的 roving tabindex）：Tab 进来落在选中的
            // 那一个上，剩下的用左右箭头走。
            tabIndex={filter === key ? 0 : -1}
            className={filter === key ? "is-selected" : ""}
            onClick={() => onSelectFilter(key)}
          >
            {label}
            {counts ? <span className="agent-history-tab-count">{historyFilterTotal(counts, key)}</span> : null}
          </button>
        ))}
      </div>
    </>
  );
}
