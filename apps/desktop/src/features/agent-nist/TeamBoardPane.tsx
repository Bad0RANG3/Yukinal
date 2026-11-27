import { lazy, Suspense, useState } from "react";
import { Icon } from "../../components/Icon.js";
import { errorMessage, formatRelativeTime, formatTimestamp } from "../../lib/format.js";
import { isPreviewFixture } from "../../lib/preview-fixtures.js";
import { useServers } from "../../lib/servers.js";
import { useWorkspaceStore } from "../../stores/workspace-store.js";
import { useInvestigationOverview } from "../investigations/investigation-overview.js";
import { useTeamRuntimeStore } from "./team-runtime-store.js";
import { BOARD_COLUMNS, boardCardFeedback, boardPreviewCards, teamBoardCards, type TeamBoardCard } from "./team-board.js";
import { TeamFeedbackPreview } from "./TeamActivity.js";
import { TaskPropertiesWindow } from "./TaskPropertiesWindow.js";

const InvestigationsPane = lazy(async () => ({ default: (await import("../investigations/InvestigationsPane.js")).InvestigationsPane }));

/**
 * The task board is one surface of the operations workbench, not a page of its
 * own. Goal tasks stay one click away inside it, and both views read the same
 * `investigation-overview` cache, so switching costs no request.
 */
export function TeamBoardPane({ serverId }: { serverId: string | null; expanded?: boolean }) {
  const runs = useTeamRuntimeStore((state) => state.runs);
  const cues = useTeamRuntimeStore((state) => state.cues);
  const selectedServerId = useWorkspaceStore((state) => state.selectedServerId);
  const [goalsOpen, setGoalsOpen] = useState(false);
  const [scope, setScope] = useState(serverId ?? "all");
  const [search, setSearch] = useState("");
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const servers = useServers({ enabled: false });
  const names = new Map(servers.data?.map((server) => [server.id, server.name]));
  const { tasks } = useInvestigationOverview();
  const stored = tasks.data?.tasks;
  const allCards = [...(isPreviewFixture() ? boardPreviewCards(cues) : []), ...teamBoardCards(stored ?? [], runs)];
  const cards = allCards.filter((card) => (scope === "all" || card.serverId === scope)
    && (!search.trim() || `${card.goal} ${card.serverId ? names.get(card.serverId) ?? card.serverId : "全局工作区"}`.toLocaleLowerCase().includes(search.trim().toLocaleLowerCase())));
  const activeCount = cards.filter((card) => !card.preview && boardCardFeedback(card, cues)?.phase === "working").length;
  const waitingCount = cards.filter((card) => card.column === "waiting").length;
  const selectedCard = allCards.find((card) => card.id === selectedId);
  return <div className="team-board is-expanded">
    <header className="team-board-heading"><div><h2>{goalsOpen ? "目标任务" : "看板"}</h2>{!goalsOpen ? <span>{cards.filter((card) => !card.preview).length} 项任务</span> : null}</div><div className="team-board-heading-actions">
      <button type="button" className="button-secondary" onClick={() => { setGoalsOpen((open) => !open); if (goalsOpen) void tasks.refetch(); }}>{goalsOpen ? "返回看板" : "目标任务"}</button>
      {!goalsOpen && isPreviewFixture() ? <TeamFeedbackPreview serverId={scope !== "all" ? scope : selectedServerId ?? servers.data?.[0]?.id ?? null} /> : null}
      {!goalsOpen ? <button type="button" className="icon-button" disabled={tasks.isFetching} aria-label="刷新任务看板" title="刷新" onClick={() => void tasks.refetch()}><Icon name="refresh" size="sm" /></button> : null}
    </div></header>
    {goalsOpen ? <Suspense fallback={<p className="agent-nist-notice" role="status">加载目标任务…</p>}><InvestigationsPane /></Suspense> : <>
    <div className="team-board-controls"><label className="team-board-search"><Icon name="search" size="sm" /><input aria-label="搜索看板任务" placeholder="搜索任务或服务器" value={search} onChange={(event) => setSearch(event.target.value)} /></label>
      <div className="team-board-filters"><select aria-label="看板服务器范围" value={scope} onChange={(event) => setScope(event.target.value)}><option value="all">全部服务器</option>{servers.data?.map((server) => <option key={server.id} value={server.id}>{server.name}</option>)}{serverId && !names.has(serverId) ? <option value={serverId}>当前服务器</option> : null}</select></div>
    </div>
    <div className="team-board-status" role="status"><span>{activeCount ? `本窗口 ${activeCount} 个 AI 执行中` : "本窗口无 AI 执行"}</span>{waitingCount ? <strong>{waitingCount} 项待确认</strong> : null}<span>点击卡片查看属性</span></div>
    {tasks.isLoading ? <p className="agent-nist-notice" role="status">读取任务…</p> : null}
    {tasks.isError ? <p className="agent-nist-notice is-error" role="alert">已存任务读取失败：{errorMessage(tasks.error)}<button type="button" className="agent-nist-text-action" onClick={() => void tasks.refetch()}>重试</button></p> : null}
    <div className="team-board-lanes"><div className="team-board-columns">{BOARD_COLUMNS.map((column) => {
      const items = cards.filter((card) => card.column === column.id);
      return <section key={column.id} className={`team-board-column column-${column.id}`} aria-label={column.label}>
        <header><span aria-hidden="true" /><h3>{column.label}</h3><span>{items.length}</span></header>
        <div className="team-board-cards">{items.map((card) => <BoardCard key={card.id} card={card} serverName={card.serverId ? names.get(card.serverId) ?? card.serverId : "全局工作区"} onOpen={() => setSelectedId(card.id)} />)}{!items.length ? <p className="team-board-empty">{tasks.isLoading ? "读取中…" : tasks.isError ? "任务状态未同步" : search || scope !== "all" ? "无匹配任务" : "暂无任务"}</p> : null}</div>
      </section>;
    })}</div></div>
    <p className="team-board-footnote">已存任务与本窗口运行同步呈现。{stored?.length === 100 ? "当前显示最近 100 个已存任务。" : ""}</p>
    {selectedCard ? <TaskPropertiesWindow key={selectedCard.id} card={selectedCard} serverName={selectedCard.serverId ? names.get(selectedCard.serverId) ?? selectedCard.serverId : "全局工作区"} onClose={() => setSelectedId(null)} /> : null}
    </>}
  </div>;
}

function BoardCard({ card, serverName, onOpen }: { card: TeamBoardCard; serverName: string; onOpen: () => void }) {
  const cues = useTeamRuntimeStore((state) => state.cues);
  const feedback = boardCardFeedback(card, cues);
  return <article className={`team-board-card ${card.failed ? "has-failure" : ""} ${feedback ? "team-color-blue" : ""}`} data-operation={feedback?.phase}>
    {feedback ? <span key={feedback.key} aria-hidden="true" className="team-operation-outline team-color-blue" data-operation={feedback.phase} /> : null}
    <button type="button" className="team-board-card-toggle" aria-haspopup="dialog" aria-label={`查看任务属性：${card.goal || "附件任务"}`} onClick={onOpen}>
      <strong title={card.goal || "附件任务"}>{card.goal || "附件任务"}</strong>
      <span className="team-board-card-meta"><span>{card.preview ? "模拟 · " : "运维 Agent"}</span><span className={feedback?.phase === "working" ? "team-board-working" : ""}>{feedback?.phase === "working" ? <i aria-hidden="true" /> : null}{card.status}</span></span>
      <span className="team-board-card-target"><Icon name={card.serverId ? "servers" : "projects"} size="xs" /><span title={serverName}>{serverName}</span>{card.updatedAt > 0 ? <time dateTime={new Date(card.updatedAt).toISOString()} title={formatTimestamp(new Date(card.updatedAt).toISOString())}>{formatRelativeTime(new Date(card.updatedAt).toISOString())}</time> : null}</span>
    </button>
  </article>;
}
