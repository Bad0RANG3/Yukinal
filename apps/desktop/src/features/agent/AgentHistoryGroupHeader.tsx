/**
 * 一个时间分组的小标题（`今天` / `昨天` / `最近 7 天` / `更早`），后面跟着这一组的条数。
 *
 * 条数是**本组已载入的条数**，不是「这一组一共有多少」—— 后者只有宿主侧的计数才说得准，
 * 而那个计数按的是关键字与归档状态，不按日期。
 */

export function AgentHistoryGroupHeader({ label, count }: { label: string; count: number }) {
  return (
    <h4 className="agent-history-group-label">
      {label}
      <span>{count}</span>
    </h4>
  );
}
