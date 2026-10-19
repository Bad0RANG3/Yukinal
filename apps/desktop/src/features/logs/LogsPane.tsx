import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { type LogLevel, type LogSource } from "@yukinal/shared";

import { errorMessage } from "../../lib/format.js";
import { isDesktopShell } from "../../lib/ipc.js";
import { serverLogsQueryOptions } from "../../lib/remote-feeds.js";
import { useWorkspaceStore } from "../../stores/workspace-store.js";
import { EmptyPanel, ErrorPanel, PreviewEmpty } from "../../components/PanelStates.js";
import { Icon } from "../../components/Icon.js";
import { KeywordText } from "../../components/KeywordText.js";
import { PageToolbar } from "../../components/PageToolbar.js";
import { RemoteListPager } from "../../components/RemoteListPager.js";

const PAGE_SIZE = 30;
const LEVEL_LABEL: Record<LogLevel, string> = { error: "错误", warning: "警告", info: "信息" };
const SOURCE_LABEL: Record<LogSource, string> = {
  journalctl: "journalctl", syslog: "/var/log/syslog", messages: "/var/log/messages", unavailable: "未发现日志源",
};

export function LogsPane({ serverId, active = true }: { serverId?: string | null; active?: boolean } = {}) {
  const workspaceServerId = useWorkspaceStore((state) => state.selectedServerId);
  const selectedServerId = serverId === undefined ? workspaceServerId : serverId;
  const shell = isDesktopShell();
  const logsQuery = useQuery(serverLogsQueryOptions(selectedServerId, active));
  const [search, setSearch] = useState("");
  const [level, setLevel] = useState<LogLevel | "all">("all");
  const [page, setPage] = useState(0);
  // Sharing the query also shares an in-flight refresh; native IPC cannot be cancelled.
  const refresh = () => void logsQuery.refetch({ cancelRefetch: false });

  if (!active) return null;
  if (!shell) return <PreviewEmpty icon="logs" body="桌面功能不可用" />;
  if (!selectedServerId) return <EmptyPanel icon="logs" title="选择一台服务器" />;
  if (logsQuery.isLoading) return <div className="loading-panel" role="status" aria-label="读取日志"><div className="loading-spinner" /></div>;
  if (!logsQuery.data) return <ErrorPanel showIcon title="无法读取远端日志" message={errorMessage(logsQuery.error)} onRetry={refresh} />;

  const response = logsQuery.data;
  const needle = search.trim().toLocaleLowerCase();
  const filtered = response.lines.filter((line) => (level === "all" || line.level === level) && (!needle || line.text.toLocaleLowerCase().includes(needle)));
  const currentPage = Math.min(page, Math.max(0, Math.ceil(filtered.length / PAGE_SIZE) - 1));
  const visible = filtered.slice(currentPage * PAGE_SIZE, (currentPage + 1) * PAGE_SIZE);

  return <div className="logs-page">
    <PageToolbar actions={<button type="button" className="button-secondary" onClick={refresh} disabled={logsQuery.isFetching} title="刷新日志" aria-label="刷新日志"><Icon name="refresh" size="sm" /> {logsQuery.isFetching ? "读取中" : "刷新"}</button>}>
      <span className={`log-source log-source-${response.source}`}>{SOURCE_LABEL[response.source]}</span>
      <span>最近 {response.lines.length} 行 · 按需刷新</span>
    </PageToolbar>
    <div className="remote-list-filters">
      <input type="search" aria-label="筛选已加载日志" placeholder="筛选日志" value={search} onChange={(event) => { setSearch(event.target.value); setPage(0); }} />
      <select aria-label="日志级别" value={level} onChange={(event) => { setLevel(event.target.value as LogLevel | "all"); setPage(0); }}>
        <option value="all">全部级别</option><option value="error">错误</option><option value="warning">警告</option><option value="info">信息</option>
      </select>
    </div>
    {logsQuery.isError ? <div className="log-notice" role="status">刷新失败，保留上次结果。{errorMessage(logsQuery.error)}</div> : null}
    {response.message ? <div className="log-notice">{response.message}</div> : null}
    {filtered.length === 0 ? <p className="muted-copy">{response.lines.length ? "没有匹配日志" : "暂无日志"}</p> : <ol className="log-list" aria-label="远端日志列表" start={currentPage * PAGE_SIZE + 1}>
      {visible.map((line, index) => <li className="log-row" key={`${currentPage * PAGE_SIZE + index}:${line.text}`}>
        <span className="log-line-number" aria-hidden="true">{String(currentPage * PAGE_SIZE + index + 1).padStart(3, "0")}</span>
        <span className={`log-level log-level-${line.level}`}>{LEVEL_LABEL[line.level]}</span>
        <code><KeywordText text={line.text} /></code>
      </li>)}
    </ol>}
    <RemoteListPager page={currentPage} pageSize={PAGE_SIZE} count={filtered.length} onPage={setPage} />
  </div>;
}
