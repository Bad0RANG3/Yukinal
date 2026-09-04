import { AgentPanel } from "../features/agent/AgentPanel.js";
import { RuntimeSettings } from "../features/settings/RuntimeSettings.js";
import { RuntimeStrip } from "../features/settings/RuntimeStrip.js";
import { ServerList } from "../features/servers/ServerList.js";
import { TerminalPane } from "../features/terminal/TerminalPane.js";
import { ServerOverview } from "../features/overview/ServerOverview.js";
import { RemoteFilesPane } from "../features/files/RemoteFilesPane.js";
import { ActivityFeed } from "../features/activity/ActivityFeed.js";
import {
  PRIMARY_NAV,
  SERVER_PAGES,
  useWorkspaceStore,
  type PrimaryNav,
  type ServerPage,
} from "../stores/workspace-store.js";

const PRIMARY_NAV_META: Record<PrimaryNav, { label: string; icon: string }> = {
  servers: { label: "服务器", icon: "▦" },
  projects: { label: "项目", icon: "⌁" },
  activity: { label: "动态", icon: "◷" },
  settings: { label: "设置", icon: "⚙" },
};

const SERVER_PAGE_LABELS: Record<ServerPage, string> = {
  overview: "概览",
  terminal: "终端",
  files: "文件",
  logs: "日志",
  services: "服务",
  activity: "活动",
};

export function AppShell() {
  const primary = useWorkspaceStore((state) => state.primary);
  const serverPage = useWorkspaceStore((state) => state.serverPage);
  const selectedServerId = useWorkspaceStore((state) => state.selectedServerId);
  const setPrimary = useWorkspaceStore((state) => state.setPrimary);
  const setServerPage = useWorkspaceStore((state) => state.setServerPage);

  return (
    <div className="app-shell">
      <nav className="app-rail" aria-label="主导航">
        <div className="brand-mark" aria-label="Yukinal">
          <span>Y</span>
        </div>
        <div className="rail-nav">
          {PRIMARY_NAV.map((item) => {
            const meta = PRIMARY_NAV_META[item];
            return (
              <button
                key={item}
                type="button"
                title={meta.label}
                aria-label={meta.label}
                onClick={() => setPrimary(item)}
                className={`rail-button ${primary === item ? "rail-button-active" : ""}`}
              >
                <span className="rail-icon" aria-hidden="true">
                  {meta.icon}
                </span>
                <span>{meta.label}</span>
              </button>
            );
          })}
        </div>
        <RuntimeStrip />
      </nav>

      <ServerList />

      <main className="workspace-main">
        <header className="workspace-header">
          <div className="workspace-heading">
            <p className="eyebrow">{primary === "servers" ? "服务器工作区" : "全局"}</p>
            <h1>{primary === "servers" ? "基础设施" : PRIMARY_NAV_META[primary].label}</h1>
          </div>
          <div className="workspace-header-meta">
            {selectedServerId && primary === "servers" ? <span className="context-chip">已选服务器</span> : null}
            <span className="shortcut-hint">Yukinal workspace</span>
          </div>
        </header>

        {primary === "servers" ? (
          <div className="server-tabs" role="tablist" aria-label="服务器视图">
            {SERVER_PAGES.map((page) => (
              <button
                key={page}
                type="button"
                role="tab"
                aria-selected={serverPage === page}
                onClick={() => setServerPage(page)}
                className={`server-tab ${serverPage === page ? "server-tab-active" : ""}`}
              >
                {SERVER_PAGE_LABELS[page]}
              </button>
            ))}
          </div>
        ) : null}

        <div className="workspace-content">
          {primary === "settings" ? <RuntimeSettings /> : null}
          {primary === "projects" ? <ComingSoon icon="⌁" title="项目视图" detail="项目与服务器的关联会在工作区能力完成后显示。" /> : null}
          {primary === "activity" ? <ActivityFeed /> : null}
          {primary === "servers" ? (
            <>
              {serverPage === "overview" ? <ServerOverview /> : null}
              {serverPage === "terminal" ? <TerminalPane /> : null}
              {serverPage === "files" ? <RemoteFilesPane /> : null}
              {serverPage === "activity" ? <ActivityFeed serverId={selectedServerId} /> : null}
              {serverPage === "logs" || serverPage === "services" ? (
                <ComingSoon icon="···" title={SERVER_PAGE_LABELS[serverPage]} detail="这个能力还没有接入真实 IPC，保持空态以避免误导。" />
              ) : null}
            </>
          ) : null}
        </div>
      </main>

      <AgentPanel />
    </div>
  );
}

function ComingSoon({ icon, title, detail }: { icon: string; title: string; detail: string }) {
  return (
    <div className="empty-state page-empty">
      <span className="empty-state-mark">{icon}</span>
      <h2>{title}</h2>
      <p>{detail}</p>
    </div>
  );
}
