import { lazy, Suspense, useEffect, useRef, useState, type KeyboardEvent } from "react";
import { AgentPanel } from "../features/agent/AgentPanel.js";
import { useOnboardingStore } from "../features/onboarding/onboarding-store.js";
import { ServerList } from "../features/servers/ServerList.js";
import { ServerAuthChallengeModal } from "../features/servers/ServerAuthChallengeModal.js";
import { HostKeyGate } from "../features/servers/HostKeyGate.js";
import { InfrastructureDashboard } from "../features/overview/InfrastructureDashboard.js";
import { Icon, type IconName } from "../components/Icon.js";
import brandMark from "../assets/brand-mark.png";
import { PRIMARY_NAV, SERVER_PAGES, useWorkspaceStore, type PrimaryNav, type ServerPage } from "../stores/workspace-store.js";
import { LivePreviewControl } from "./LivePreviewControl.js";
import { isPreviewFixture } from "../lib/preview-fixtures.js";

const TerminalPane = lazy(async () => ({ default: (await import("../features/terminal/TerminalPane.js")).TerminalPane }));
const GettingStarted = lazy(async () => ({ default: (await import("../features/onboarding/GettingStarted.js")).GettingStarted }));
const RuntimeSettings = lazy(async () => ({ default: (await import("../features/settings/RuntimeSettings.js")).RuntimeSettings }));
const ServerOverview = lazy(async () => ({ default: (await import("../features/overview/ServerOverview.js")).ServerOverview }));
const RemoteFilesPane = lazy(async () => ({ default: (await import("../features/files/RemoteFilesPane.js")).RemoteFilesPane }));
const ActivityFeed = lazy(async () => ({ default: (await import("../features/activity/ActivityFeed.js")).ActivityFeed }));
const ServicesPane = lazy(async () => ({ default: (await import("../features/services/ServicesPane.js")).ServicesPane }));
const LogsPane = lazy(async () => ({ default: (await import("../features/logs/LogsPane.js")).LogsPane }));
const InvestigationsPane = lazy(async () => ({ default: (await import("../features/investigations/InvestigationsPane.js")).InvestigationsPane }));
const PRIMARY_NAV_META: Record<PrimaryNav, { label: string; icon: IconName }> = {
  servers: { label: "仪表台", icon: "servers" },
  tasks: { label: "排查任务", icon: "activity" }, activity: { label: "动态", icon: "activity" }, settings: { label: "设置", icon: "settings" },
};
const SERVER_PAGE_META: Record<ServerPage, { label: string; icon: IconName }> = {
  overview: { label: "概览", icon: "servers" }, terminal: { label: "终端", icon: "terminal" },
  files: { label: "文件", icon: "folder" }, logs: { label: "日志", icon: "logs" },
  services: { label: "服务", icon: "services" }, activity: { label: "活动", icon: "activity" },
};
function PageLoadingFallback() { return <div className="compact-empty" role="status">正在加载页面…</div>; }

export function AppShell() {
  const primary = useWorkspaceStore((state) => state.primary);
  const serverPage = useWorkspaceStore((state) => state.serverPage);
  const serverDetailOpen = useWorkspaceStore((state) => state.serverDetailOpen);
  const selectedServerId = useWorkspaceStore((state) => state.selectedServerId);
  const setPrimary = useWorkspaceStore((state) => state.setPrimary);
  const setServerPage = useWorkspaceStore((state) => state.setServerPage);
  const [sidebarOpen, setSidebarOpen] = useState(false);
  const [contextOpen, setContextOpen] = useState(false);
  const [guideOpen, setGuideOpen] = useState(() => !isPreviewFixture() && !useOnboardingStore.getState().dismissed);
  const sidebarRef = useRef<HTMLElement>(null);
  const sidebarToggleRef = useRef<HTMLButtonElement>(null);
  const contextToggleRef = useRef<HTMLButtonElement>(null);
  const inspectorRef = useRef<HTMLElement>(null);
  const aiWorkspaceRef = useRef<HTMLDivElement>(null);
  const previousPrimary = useRef(primary);
  const closeGuide = () => { useOnboardingStore.getState().dismiss(); setGuideOpen(false); };
  const detail = primary === "servers" && serverDetailOpen;
  const terminalActive = detail && !guideOpen && serverPage === "terminal";
  const [terminalVisited, setTerminalVisited] = useState(terminalActive);
  useEffect(() => { if (terminalActive) setTerminalVisited(true); }, [terminalActive]);
  useEffect(() => {
    if (previousPrimary.current !== primary && window.matchMedia("(max-width: 1180px)").matches) setContextOpen(true);
    previousPrimary.current = primary;
  }, [primary]);

  // Only the compact navigation is modal. The AI workspace never traps Tab or Escape.
  useEffect(() => {
    if (!sidebarOpen) return;
    const sidebar = sidebarRef.current;
    if (!sidebar) return;
    const frame = requestAnimationFrame(() => sidebar.querySelector<HTMLElement>("button")?.focus());
    const onKey = (event: globalThis.KeyboardEvent) => {
      if (event.target instanceof HTMLElement && event.target.closest("dialog[open]")) return;
      if (event.key === "Escape") { event.preventDefault(); setSidebarOpen(false); return; }
      if (event.key !== "Tab") return;
      const items = Array.from(sidebar.querySelectorAll<HTMLElement>("button:not([disabled]), input:not([disabled]), [href]"))
        .filter((item) => item.getClientRects().length > 0);
      const first = items[0], last = items.at(-1);
      if (event.shiftKey && document.activeElement === first) { event.preventDefault(); last?.focus(); }
      else if (!event.shiftKey && document.activeElement === last) { event.preventDefault(); first?.focus(); }
    };
    document.addEventListener("keydown", onKey);
    return () => { cancelAnimationFrame(frame); document.removeEventListener("keydown", onKey); sidebarToggleRef.current?.focus({ preventScroll: true }); };
  }, [sidebarOpen]);
  useEffect(() => {
    const compact = window.matchMedia("(max-width: 1180px)"), navigation = window.matchMedia("(max-width: 720px)");
    const reset = () => { setSidebarOpen(false); setContextOpen(false); };
    compact.addEventListener("change", reset); navigation.addEventListener("change", reset);
    return () => { compact.removeEventListener("change", reset); navigation.removeEventListener("change", reset); };
  }, []);
  const openContext = () => { setContextOpen(true); requestAnimationFrame(() => inspectorRef.current?.focus({ preventScroll: true })); };
  const returnToAI = () => {
    setContextOpen(false); setSidebarOpen(false);
    requestAnimationFrame(() => {
      if (window.matchMedia("(max-width: 1180px)").matches) contextToggleRef.current?.focus({ preventScroll: true });
      else aiWorkspaceRef.current?.focus({ preventScroll: true });
    });
  };
  const navigate = (item: PrimaryNav) => { setPrimary(item); closeGuide(); setSidebarOpen(false); if (window.matchMedia("(max-width: 1180px)").matches) openContext(); };
  const navigateTab = (event: KeyboardEvent<HTMLButtonElement>, index: number) => {
    const next = event.key === "ArrowRight" ? (index + 1) % SERVER_PAGES.length : event.key === "ArrowLeft" ? (index + SERVER_PAGES.length - 1) % SERVER_PAGES.length : event.key === "Home" ? 0 : event.key === "End" ? SERVER_PAGES.length - 1 : null;
    if (next === null) return;
    event.preventDefault(); const page = SERVER_PAGES[next];
    if (page) { setServerPage(page); document.getElementById(`server-tab-${page}`)?.focus(); }
  };
  return (
    <div className={`app-shell ai-first ${sidebarOpen ? "navigation-open" : ""} ${contextOpen ? "context-open" : ""}`} data-primary={primary}>
      <aside className="workspace-sidebar" id="workspace-sidebar" ref={sidebarRef} aria-label="工作区导航">
        <div className="workspace-brand"><img src={brandMark} alt="" /><span>Yukinal</span><button type="button" className="icon-button navigation-close" aria-label="关闭导航" onClick={() => setSidebarOpen(false)}><Icon name="close" /></button></div>
        <nav className="workspace-navigation" aria-label="主导航">{PRIMARY_NAV.map((item) => <button type="button" key={item} className={`workspace-nav-item ${primary === item && (item !== "servers" || !serverDetailOpen) ? "is-active" : ""}`} aria-current={primary === item && (item !== "servers" || !serverDetailOpen) ? "page" : undefined} onClick={() => navigate(item)}><Icon name={PRIMARY_NAV_META[item].icon} size="md" /><span>{PRIMARY_NAV_META[item].label}</span></button>)}</nav>
        <ServerList embedded onClose={() => setSidebarOpen(false)} onSelect={() => { setSidebarOpen(false); closeGuide(); if (window.matchMedia("(max-width: 1180px)").matches) openContext(); }} />
      </aside>
      {sidebarOpen ? <button type="button" className="navigation-scrim" aria-label="关闭导航" onClick={() => setSidebarOpen(false)} /> : null}
      <div className="ai-workspace-column" ref={aiWorkspaceRef} tabIndex={-1}>
        <AgentPanel workspace onAttentionRequested={returnToAI} workspaceControls={<>
          {import.meta.env.DEV ? <LivePreviewControl /> : null}
          <button type="button" ref={sidebarToggleRef} className="icon-button navigation-toggle" aria-label="打开导航" aria-controls="workspace-sidebar" aria-expanded={sidebarOpen} onClick={() => setSidebarOpen(true)}><Icon name="projects" /></button>
          <button type="button" className="text-button workspace-guide-toggle" onClick={() => { setGuideOpen((open) => !open); openContext(); }}>连接设置</button>
          <button type="button" ref={contextToggleRef} className="icon-button context-toggle" aria-label="打开仪表台" aria-controls="workspace-inspector" aria-expanded={contextOpen} onClick={openContext}><Icon name="servers" /></button>
        </>} />
      </div>
      <main className="workspace-main workspace-inspector" id="workspace-inspector" ref={inspectorRef} tabIndex={-1} aria-label="仪表台与参考资料">
        <header className="workspace-header is-titleless">
          <div className="workspace-header-meta">
            {detail && !guideOpen ? <button type="button" className="text-button" onClick={() => setPrimary("servers")}>全部</button> : null}
            <button type="button" className="icon-button context-close" aria-label="回到对话" onClick={returnToAI}><Icon name="close" /></button>
          </div>
        </header>
        {detail && !guideOpen ? <div className="server-tabs" role="tablist" aria-label="服务器视图">{SERVER_PAGES.map((page, index) => <button key={page} type="button" role="tab" id={`server-tab-${page}`} aria-controls="server-view" aria-selected={serverPage === page} tabIndex={serverPage === page ? 0 : -1} onKeyDown={(event) => navigateTab(event, index)} onClick={() => setServerPage(page)} className={`server-tab ${serverPage === page ? "server-tab-active" : ""}`}><Icon name={SERVER_PAGE_META[page].icon} size="sm" /><span>{SERVER_PAGE_META[page].label}</span></button>)}</div> : null}
        <div className="workspace-content" id={detail ? "server-view" : undefined} role={detail && !guideOpen ? "tabpanel" : undefined} aria-labelledby={detail && !guideOpen ? `server-tab-${serverPage}` : undefined} tabIndex={0}>
          {guideOpen ? <Suspense fallback={<PageLoadingFallback />}><GettingStarted onClose={closeGuide} /></Suspense> : null}
          <div hidden={guideOpen} className="workspace-pages">
            {primary === "settings" && !guideOpen ? <Suspense fallback={<PageLoadingFallback />}><RuntimeSettings /></Suspense> : null}
            {primary === "tasks" && !guideOpen ? <Suspense fallback={<PageLoadingFallback />}><InvestigationsPane /></Suspense> : null}
            {primary === "activity" && !guideOpen ? <Suspense fallback={<PageLoadingFallback />}><ActivityFeed /></Suspense> : null}
            {primary === "servers" && !guideOpen && !detail ? <InfrastructureDashboard onAttentionRequested={returnToAI} /> : null}
            {detail && !guideOpen ? <>
              {serverPage === "overview" ? <HostKeyGate serverId={selectedServerId}><Suspense fallback={<PageLoadingFallback />}><ServerOverview key={selectedServerId} /></Suspense></HostKeyGate> : null}
              {serverPage === "files" ? <HostKeyGate serverId={selectedServerId}><Suspense fallback={<PageLoadingFallback />}><RemoteFilesPane key={selectedServerId} /></Suspense></HostKeyGate> : null}
              {serverPage === "logs" ? <HostKeyGate serverId={selectedServerId}><Suspense fallback={<PageLoadingFallback />}><LogsPane /></Suspense></HostKeyGate> : null}
              {serverPage === "services" ? <HostKeyGate serverId={selectedServerId}><Suspense fallback={<PageLoadingFallback />}><ServicesPane /></Suspense></HostKeyGate> : null}
              {serverPage === "activity" ? <Suspense fallback={<PageLoadingFallback />}><ActivityFeed serverId={selectedServerId} /></Suspense> : null}
            </> : null}
            {terminalVisited || terminalActive ? <div hidden={!terminalActive} className="terminal-container"><HostKeyGate serverId={selectedServerId}><Suspense fallback={<PageLoadingFallback />}><TerminalPane key={selectedServerId} active={terminalActive} /></Suspense></HostKeyGate></div> : null}
          </div>
        </div>
      </main>
      <ServerAuthChallengeModal />
    </div>
  );
}
