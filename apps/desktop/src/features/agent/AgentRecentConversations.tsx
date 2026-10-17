import type { ChatSession } from "@yukinal/shared";
import { Icon } from "../../components/Icon.js";

function recentDate(value: string): string {
  const date = new Date(value);
  return Number.isFinite(date.getTime()) ? new Intl.DateTimeFormat("zh-CN", { month: "numeric", day: "numeric" }).format(date) : "";
}

export function AgentRecentConversations({ sessions, busy, error, onOpen, onShowAll, onRetry }: {
  sessions: ChatSession[];
  busy: boolean;
  error: boolean;
  onOpen: (id: string) => void;
  onShowAll: () => void;
  onRetry: () => void;
}) {
  if (!sessions.length && !error) return null;
  return <section className="agent-recent-conversations" aria-label="最近对话">
    <div className="agent-recent-heading"><span>最近对话</span><button type="button" disabled={busy} onClick={onShowAll}>全部<Icon name="chevronRight" size="xs" /></button></div>
    {error ? <div className="agent-recent-error" role="status">读取失败<button type="button" disabled={busy} onClick={onRetry}>重试</button></div> : null}
    {sessions.map((session) => <button type="button" className="agent-recent-row" key={session.id} title={session.title} disabled={busy} onClick={() => onOpen(session.id)}>
      <Icon name="agent" size="sm" /><span>{session.title}</span><time dateTime={session.updatedAt}>{recentDate(session.updatedAt)}</time>
    </button>)}
  </section>;
}
