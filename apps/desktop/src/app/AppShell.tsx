import { AgentPanel } from "../features/agent/AgentPanel.js";
import { GettingStarted } from "../features/onboarding/GettingStarted.js";
import { useOnboardingStore } from "../features/onboarding/onboarding-store.js";
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
import brandMark from "../assets/brand-mark.png";
import {
  PRIMARY_NAV,
  SERVER_PAGES,
  useWorkspaceStore,
  type PrimaryNav,
  type ServerPage,
} from "../stores/workspace-store.js";
import { usePreferencesStore } from "../stores/preferences-store.js";
import { useServers } from "../lib/servers.js";
import { isDesktopShell } from "../lib/ipc.js";
import { SERVER_STATUS_LABEL } from "../lib/labels.js";
import { useCallback, useEffect, useRef, useState, type CSSProperties, type KeyboardEvent } from "react";

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
  const [guideOpen, setGuideOpen] = useState(() => !useOnboardingStore.getState().dismissed);
  const closeGuide = () => { useOnboardingStore.getState().dismiss(); setGuideOpen(false); };
  const [agentLayoutOpen, setAgentLayoutOpen] = useState(agentOpen);
  const [agentToggleVisible, setAgentToggleVisible] = useState(!agentOpen);
  const agentToggleRef = useRef<HTMLButtonElement>(null);
  const workspaceContentRef = useRef<HTMLDivElement>(null);
  const focusAgentToggleOnRender = useRef(false);
  const terminalActive = primary === "servers" && serverPage === "terminal";
  const layoutAgentOpen = agentOpen || agentLayoutOpen;
  const onAgentCloseStart = useCallback(() => {
    if (document.activeElement instanceof HTMLElement && document.activeElement.closest("#agent-panel")) {
      workspaceContentRef.current?.focus({ preventScroll: true });
    }
    setAgentLayoutOpen(false);
    setAgentToggleVisible(false);
  }, []);
  const onAgentCloseEnd = useCallback(() => {
    focusAgentToggleOnRender.current = true;
    setAgentToggleVisible(true);
  }, []);

  useEffect(() => setSidebarOpen(false), [primary, selectedServerId]);
  useEffect(() => {
    if (agentOpen) {
      setAgentLayoutOpen(true);
      setAgentToggleVisible(false);
    }
  }, [agentOpen]);

  useEffect(() => {
    if (agentOpen || !agentToggleVisible || !focusAgentToggleOnRender.current) return;
    focusAgentToggleOnRender.current = false;
    const frame = requestAnimationFrame(() => agentToggleRef.current?.focus({ preventScroll: true }));
    return () => cancelAnimationFrame(frame);
  }, [agentOpen, agentToggleVisible]);

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
          <img src={brandMark} alt="" />
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
                onClick={() => { setPrimary(item); if (guideOpen) closeGuide(); }}
                className={`rail-button ${primary === item ? "rail-button-active" : ""}`}
              >
                <span className="rail-icon" aria-hidden="true">
                  <Icon name={meta.icon} size="lg" />
                </span>
                <span>{meta.label}</span>
              </button>
            );
          })}
        </div>
      </nav>

      {primary === "servers" ? <ServerList onClose={() => setSidebarOpen(false)} /> : null}
      {sidebarOpen ? <button type="button" className="sidebar-scrim" aria-label="关闭服务器列表" onClick={() => setSidebarOpen(false)} /> : null}

      <main className="workspace-main">
        <header className="workspace-header">
          <div className="workspace-heading">
            {/* 这个斜杠是纯装饰：它把「产品名」和「工作区名」在视觉上分开，本身
                不携带信息。标记成 aria-hidden 之后，读屏不会念出一个孤立的
                「斜杠」，而它极低的对比度（--line-strong，约 2:1）也就名正言顺 ——
                WCAG 对纯装饰性文字不作对比度要求。 */}
            <p className="eyebrow">YUKINAL <span className="breadcrumb-divider" aria-hidden="true">/</span> {primary === "servers" ? "服务器工作区" : "工作区管理"}</p>
            <h1 title={selectedServer?.name}>{primary === "servers" ? selectedServer?.name ?? "基础设施" : PRIMARY_NAV_META[primary].label}</h1>
          </div>
          <div className="workspace-header-meta">
            <button type="button" className="button-secondary" aria-expanded={guideOpen} onClick={() => guideOpen ? closeGuide() : setGuideOpen(true)}>使用引导</button>
            {primary === "servers" ? <button type="button" className="icon-button sidebar-toggle" aria-label={sidebarOpen ? "关闭服务器列表" : "打开服务器列表"} aria-controls="server-sidebar" aria-expanded={sidebarOpen} onClick={() => setSidebarOpen((open) => !open)}><Icon name="servers" size="md" /></button> : null}
            {!isDesktopShell() ? <span className="preview-chip">预览模式</span> : selectedServer && primary === "servers" ? <span className={`context-chip server-status-${selectedServer.status}`}>{SERVER_STATUS_LABEL[selectedServer.status]}</span> : null}
            {!agentOpen && agentToggleVisible ? (
              <button
                type="button"
                className="agent-toggle-inline"
                ref={agentToggleRef}
                id="agent-toggle-inline"
                aria-controls="agent-panel"
                aria-expanded={false}
                onClick={() => setAgentOpen(true)}
              >
                <Icon name="agent" size="sm" />
                Agent
              </button>
            ) : null}
          </div>
        </header>

        {primary === "servers" && !guideOpen ? (
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
                <Icon name={SERVER_PAGE_META[page].icon} size="sm" />
                <span>{SERVER_PAGE_META[page].label}</span>
              </button>
            ))}
          </div>
        ) : null}

        <div ref={workspaceContentRef} className="workspace-content" id={primary === "servers" ? "server-view" : undefined} role={primary === "servers" && !guideOpen ? "tabpanel" : undefined} aria-labelledby={primary === "servers" && !guideOpen ? `server-tab-${serverPage}` : undefined} tabIndex={0}>
          {guideOpen ? <GettingStarted onClose={closeGuide} /> : null}
          <div hidden={guideOpen} className="workspace-pages">
          {primary === "settings" && !guideOpen ? <RuntimeSettings /> : null}
          {primary === "projects" && !guideOpen ? <ProjectsPane /> : null}
          {primary === "activity" && !guideOpen ? <ActivityFeed /> : null}
          {primary === "servers" && !guideOpen ? (
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
        </div>
      </main>

      {agentOpen ? <button type="button" className="agent-scrim" aria-label="关闭 Agent 面板" onClick={() => setAgentOpen(false)} /> : null}
      <AgentPanel onCloseStart={onAgentCloseStart} onCloseEnd={onAgentCloseEnd} />
    </div>
  );
}
