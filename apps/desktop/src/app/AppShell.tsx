import { AgentPanel } from "../features/agent/AgentPanel.js";
import { RuntimeSettings } from "../features/settings/RuntimeSettings.js";
import { ServerList } from "../features/servers/ServerList.js";
import { TerminalPane } from "../features/terminal/TerminalPane.js";
import { ServerOverview } from "../features/overview/ServerOverview.js";
import { RemoteFilesPane } from "../features/files/RemoteFilesPane.js";
import { ActivityFeed } from "../features/activity/ActivityFeed.js";
import { ServicesPane } from "../features/services/ServicesPane.js";
import { LogsPane } from "../features/logs/LogsPane.js";
import { ProjectsPane } from "../features/projects/ProjectsPane.js";
import { Icon, type IconName } from "../components/Icon.js";
import {
  PRIMARY_NAV,
  SERVER_PAGES,
  useWorkspaceStore,
  type PrimaryNav,
  type ServerPage,
} from "../stores/workspace-store.js";
import { usePreferencesStore } from "../stores/preferences-store.js";
import { useServers } from "../lib/servers.js";
import { useStartupProviderImport } from "../lib/providers.js";
import { isDesktopShell } from "../lib/ipc.js";
import { useCallback, useEffect, useState, type CSSProperties, type KeyboardEvent } from "react";

const PRIMARY_NAV_META: Record<PrimaryNav, { label: string; icon: IconName }> = {
  servers: { label: "服务器", icon: "servers" },
  projects: { label: "项目", icon: "projects" },
  activity: { label: "动态", icon: "activity" },
  settings: { label: "设置", icon: "settings" },
};

const SERVER_PAGE_META: Record<ServerPage, { label: string; icon: IconName }> = {
  overview: { label: "概览", icon: "servers" },
  terminal: { label: "终端", icon: "terminal" },
  files: { label: "文件", icon: "folder" },
  logs: { label: "日志", icon: "logs" },
  services: { label: "服务", icon: "services" },
  activity: { label: "活动", icon: "activity" },
};

export function AppShell() {
  const primary = useWorkspaceStore((state) => state.primary);
  const serverPage = useWorkspaceStore((state) => state.serverPage);
  const selectedServerId = useWorkspaceStore((state) => state.selectedServerId);
  const agentOpen = useWorkspaceStore((state) => state.agentOpen);
  const setAgentOpen = useWorkspaceStore((state) => state.setAgentOpen);
  const setPrimary = useWorkspaceStore((state) => state.setPrimary);
  const setServerPage = useWorkspaceStore((state) => state.setServerPage);
  const preferences = usePreferencesStore();
  const servers = useServers();
  const selectedServer = servers.data?.find((server) => server.id === selectedServerId);
  const [sidebarOpen, setSidebarOpen] = useState(false);
  const [agentLayoutOpen, setAgentLayoutOpen] = useState(agentOpen);
  const [agentToggleVisible, setAgentToggleVisible] = useState(!agentOpen);
  const terminalActive = primary === "servers" && serverPage === "terminal";
  const layoutAgentOpen = agentOpen || agentLayoutOpen;
  const onAgentCloseStart = useCallback(() => {
    setAgentLayoutOpen(false);
    setAgentToggleVisible(false);
  }, []);
  const onAgentCloseEnd = useCallback(() => setAgentToggleVisible(true), []);

  // Import local OpenCode/Codex/CC Switch provider material before the first
  // Agent request. The command is native, idempotent and silent when no source
  // exists, so the shell can render immediately.
  useStartupProviderImport();

  useEffect(() => setSidebarOpen(false), [primary, selectedServerId]);
  useEffect(() => {
    if (agentOpen) {
      setAgentLayoutOpen(true);
      setAgentToggleVisible(false);
    }
  }, [agentOpen]);

  const navigateTab = (event: KeyboardEvent<HTMLButtonElement>, index: number) => {
    const next = event.key === "ArrowRight" ? (index + 1) % SERVER_PAGES.length
      : event.key === "ArrowLeft" ? (index + SERVER_PAGES.length - 1) % SERVER_PAGES.length
      : event.key === "Home" ? 0 : event.key === "End" ? SERVER_PAGES.length - 1 : null;
    if (next === null) return;
    event.preventDefault();
    const page = SERVER_PAGES[next];
    if (!page) return;
    setServerPage(page);
    document.getElementById(`server-tab-${page}`)?.focus();
  };

  const shellStyle = { "--ui-font-size": `${preferences.uiFontSize}px` } as CSSProperties;

  return (
    <div
      className={`app-shell ${layoutAgentOpen ? "" : "agent-collapsed"} ${sidebarOpen ? "sidebar-open" : ""}`}
      data-primary={primary}
      data-density={preferences.density}
      data-reduce-motion={preferences.reduceMotion ? "true" : "false"}
      style={shellStyle}
    >
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
                aria-current={primary === item ? "page" : undefined}
                onClick={() => setPrimary(item)}
                className={`rail-button ${primary === item ? "rail-button-active" : ""}`}
              >
                <span className="rail-icon" aria-hidden="true">
                  <Icon name={meta.icon} size={17} />
                </span>
                <span>{meta.label}</span>
              </button>
            );
          })}
        </div>
      </nav>

      <ServerList onClose={() => setSidebarOpen(false)} />
      {sidebarOpen ? <button type="button" className="sidebar-scrim" aria-label="关闭服务器列表" onClick={() => setSidebarOpen(false)} /> : null}

      <main className="workspace-main">
        <header className="workspace-header">
          <div className="workspace-heading">
            <p className="eyebrow">YUKINAL <span className="breadcrumb-divider">/</span> {primary === "servers" ? "服务器工作区" : "工作区管理"}</p>
            <h1 title={selectedServer?.name}>{primary === "servers" ? selectedServer?.name ?? "基础设施" : PRIMARY_NAV_META[primary].label}</h1>
          </div>
          <div className="workspace-header-meta">
            {primary === "servers" ? <button type="button" className="icon-button sidebar-toggle" aria-label={sidebarOpen ? "关闭服务器列表" : "打开服务器列表"} aria-controls="server-sidebar" aria-expanded={sidebarOpen} onClick={() => setSidebarOpen((open) => !open)}><Icon name="servers" size={16} /></button> : null}
            {!isDesktopShell() ? <span className="preview-chip">预览模式</span> : selectedServer && primary === "servers" ? <span className={`context-chip server-status-${selectedServer.status}`}>{selectedServer.status === "connected" ? "已连接" : selectedServer.status === "connecting" ? "连接中" : selectedServer.status === "error" ? "连接异常" : "未连接"}</span> : null}
            {!agentOpen && agentToggleVisible ? (
              <button
                type="button"
                className="agent-toggle-inline"
                aria-controls="agent-panel"
                aria-expanded={false}
                onClick={() => setAgentOpen(true)}
              >
                <Icon name="agent" size={14} />
                Agent
              </button>
            ) : null}
          </div>
        </header>

        {primary === "servers" ? (
          <div className="server-tabs" role="tablist" aria-label="服务器视图">
            {SERVER_PAGES.map((page, index) => (
              <button
                key={page}
                type="button"
                role="tab"
                id={`server-tab-${page}`}
                aria-controls="server-view"
                aria-selected={serverPage === page}
                tabIndex={serverPage === page ? 0 : -1}
                onKeyDown={(event) => navigateTab(event, index)}
                onClick={() => setServerPage(page)}
                className={`server-tab ${serverPage === page ? "server-tab-active" : ""}`}
              >
                <Icon name={SERVER_PAGE_META[page].icon} size={14} />
                <span>{SERVER_PAGE_META[page].label}</span>
              </button>
            ))}
          </div>
        ) : null}

        <div className="workspace-content" id={primary === "servers" ? "server-view" : undefined} role={primary === "servers" ? "tabpanel" : undefined} aria-labelledby={primary === "servers" ? `server-tab-${serverPage}` : undefined} tabIndex={0}>
          {primary === "settings" ? <RuntimeSettings /> : null}
          {primary === "projects" ? <ProjectsPane /> : null}
          {primary === "activity" ? <ActivityFeed /> : null}
          {primary === "servers" ? (
            <>
              {serverPage === "overview" ? <ServerOverview key={selectedServerId} /> : null}
              {serverPage === "files" ? <RemoteFilesPane key={selectedServerId} /> : null}
              {serverPage === "logs" ? <LogsPane /> : null}
              {serverPage === "services" ? <ServicesPane /> : null}
              {serverPage === "activity" ? <ActivityFeed serverId={selectedServerId} /> : null}
            </>
          ) : null}
          <div hidden={!terminalActive} className="terminal-container"><TerminalPane key={selectedServerId} active={terminalActive} /></div>
        </div>
      </main>

      <AgentPanel onCloseStart={onAgentCloseStart} onCloseEnd={onAgentCloseEnd} />
    </div>
  );
}
