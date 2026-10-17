import { lazy, Suspense, useEffect, useRef, useState, type KeyboardEvent } from "react";
import { TeamHeartbeat, TeamBoardNavFeedback, recordTeamAttention } from "../features/agent-nist/TeamActivity.js";
import { AgentPanel } from "../features/agent/AgentPanel.js";
import { useOnboardingStore } from "../features/onboarding/onboarding-store.js";
import { ServerList } from "../features/servers/ServerList.js";
import { ServerAuthChallengeModal } from "../features/servers/ServerAuthChallengeModal.js";
import { HostKeyGate } from "../features/servers/HostKeyGate.js";
import { Icon, type IconName } from "../components/Icon.js";
import { PRIMARY_NAV, SERVER_PAGES, useWorkspaceStore, type PrimaryNav, type ServerPage } from "../stores/workspace-store.js";
import { LivePreviewControl } from "./LivePreviewControl.js";
import { ServerInspectorHeader } from "./ServerInspectorHeader.js";
import { isPreviewFixture } from "../lib/preview-fixtures.js";
import { WorkpagesDock } from "../features/workpages/WorkpagesDock.js";
import { useWorkpagesStore } from "../features/workpages/workpages-store.js";
import { openNativeWorkpage } from "../lib/ipc.js";

const GettingStarted = lazy(async () => ({ default: (await import("../features/onboarding/GettingStarted.js")).GettingStarted }));
const RuntimeSettings = lazy(async () => ({ default: (await import("../features/settings/RuntimeSettings.js")).RuntimeSettings }));
const ServerOverview = lazy(async () => ({ default: (await import("../features/overview/ServerOverview.js")).ServerOverview }));
const ActivityFeed = lazy(async () => ({ default: (await import("../features/activity/ActivityFeed.js")).ActivityFeed }));
const ServicesPane = lazy(async () => ({ default: (await import("../features/services/ServicesPane.js")).ServicesPane }));
const LogsPane = lazy(async () => ({ default: (await import("../features/logs/LogsPane.js")).LogsPane }));
const AgentNistPane = lazy(async () => ({ default: (await import("../features/agent-nist/AgentNistPane.js")).AgentNistPane }));
const VISIBLE_PRIMARY_NAV = PRIMARY_NAV.filter((item) => item !== "tasks");
const PRIMARY_NAV_META: Record<(typeof VISIBLE_PRIMARY_NAV)[number], { label: string; icon: IconName }> = {
  board: { label: "看板", icon: "board" },
  agents: { label: "团队", icon: "agent" },
  activity: { label: "动态", icon: "activity" }, settings: { label: "设置", icon: "settings" },
};
const SERVER_PAGE_LABEL: Record<ServerPage, string> = {
  overview: "概览", terminal: "终端", files: "文件",
  logs: "日志", services: "服务", activity: "活动",
};
function PageLoadingFallback() { return <div className="compact-empty" role="status">正在加载页面…</div>; }
const INSPECTOR_PAGES = SERVER_PAGES.filter((page) => page !== "terminal" && page !== "files");

export function AppShell() {
  const primary = useWorkspaceStore((state) => state.primary);
  const serverPage = useWorkspaceStore((state) => state.serverPage);
  const serverDetailOpen = useWorkspaceStore((state) => state.serverDetailOpen);
  const selectedServerId = useWorkspaceStore((state) => state.selectedServerId);
  const informationOpen = useWorkpagesStore((state) => !state.collapsed && (state.pages.length > 0 || state.operationsOpen));
  const informationExpanded = useWorkpagesStore((state) => Boolean(state.expandedId));
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
  const primaryActive = (item: (typeof VISIBLE_PRIMARY_NAV)[number]) => primary === item || (item === "activity" && primary === "servers" && !serverDetailOpen);
  // Existing navigation requests also open tools on the information surface.
  useEffect(() => {
    if (!detail || guideOpen || (serverPage !== "terminal" && serverPage !== "files")) return;
    useWorkpagesStore.getState().openTool(serverPage, { serverId: selectedServerId });
    setServerPage("overview");
    setContextOpen(false);
  }, [detail, guideOpen, serverPage, selectedServerId, setServerPage]);
  useEffect(() => {
    // Retired task navigation can still arrive from an older in-memory view.
    if (primary === "tasks") { setPrimary("activity"); return; }
    if (primary === "board") { useWorkpagesStore.getState().openTeamBoard(null); setContextOpen(false); }
    if (primary !== "board" && previousPrimary.current !== primary && window.matchMedia("(max-width: 1180px)").matches) setContextOpen(true);
    previousPrimary.current = primary;
  }, [primary, setPrimary]);

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
  const openContext = () => { useWorkpagesStore.getState().setCollapsed(true); setContextOpen(true); requestAnimationFrame(() => inspectorRef.current?.focus({ preventScroll: true })); };
  const returnToAI = () => {
    setContextOpen(false); setSidebarOpen(false);
    useWorkpagesStore.getState().setCollapsed(true);
    requestAnimationFrame(() => {
      if (window.matchMedia("(max-width: 1180px)").matches) contextToggleRef.current?.focus({ preventScroll: true });
      else aiWorkspaceRef.current?.focus({ preventScroll: true });
    });
  };
  const navigate = (item: PrimaryNav) => {
    if (item === "board") { useWorkpagesStore.getState().openTeamBoard(null); setContextOpen(false); }
    else useWorkpagesStore.getState().setCollapsed(true);
    setPrimary(item); closeGuide(); setSidebarOpen(false);
    if (item !== "board" && window.matchMedia("(max-width: 1180px)").matches) openContext();
  };
  const navigateTab = (event: KeyboardEvent<HTMLButtonElement>, index: number) => {
    const next = event.key === "ArrowRight" ? (index + 1) % INSPECTOR_PAGES.length : event.key === "ArrowLeft" ? (index + INSPECTOR_PAGES.length - 1) % INSPECTOR_PAGES.length : event.key === "Home" ? 0 : event.key === "End" ? INSPECTOR_PAGES.length - 1 : null;
    if (next === null) return;
    event.preventDefault(); const page = INSPECTOR_PAGES[next];
    if (page) { setServerPage(page); document.getElementById(`server-tab-${page}`)?.focus(); }
  };
  return (
    <div onClickCapture={recordTeamAttention} onFocusCapture={recordTeamAttention} className={`app-shell ai-first ${sidebarOpen ? "navigation-open" : ""} ${contextOpen ? "context-open" : ""} ${informationOpen && !contextOpen ? "information-open" : ""} ${informationExpanded ? "information-expanded" : ""}`} data-primary={primary}>
      <aside className="workspace-sidebar" id="workspace-sidebar" ref={sidebarRef} aria-label="工作区导航">
        <div className="workspace-brand"><span>Yukinal</span><button type="button" className="icon-button navigation-close" aria-label="关闭导航" onClick={() => setSidebarOpen(false)}><Icon name="close" /></button></div>
        <nav className="workspace-navigation" aria-label="主导航">{VISIBLE_PRIMARY_NAV.map((item) => <button type="button" key={item} data-attention={`nav:${item}`} className={`workspace-nav-item team-operation-target ${primaryActive(item) ? "is-active" : ""}`} aria-current={primaryActive(item) ? "page" : undefined} onClick={() => navigate(item)}><Icon name={PRIMARY_NAV_META[item].icon} size="md" /><span>{PRIMARY_NAV_META[item].label}</span>{item === "board" ? <TeamBoardNavFeedback /> : null}</button>)}</nav>
        <ServerList embedded onClose={() => setSidebarOpen(false)} onSelect={() => { useWorkpagesStore.getState().setCollapsed(true); setSidebarOpen(false); closeGuide(); if (window.matchMedia("(max-width: 1180px)").matches) openContext(); }} />
      </aside>
      {sidebarOpen ? <button type="button" className="navigation-scrim" aria-label="关闭导航" onClick={() => setSidebarOpen(false)} /> : null}
      <div className="ai-workspace-column" ref={aiWorkspaceRef} tabIndex={-1}>
        <AgentPanel workspace onAttentionRequested={returnToAI} onOpenTeam={() => navigate("agents")}
          workspaceWorkpages={<WorkpagesDock onPopOut={async (page) => {
            if (!page.serverId || page.kind === "evidence" || page.kind === "artifact" || page.kind === "board") throw new Error("该内容可在工作页中展开");
            await openNativeWorkpage({ kind: page.kind, serverId: page.serverId });
          }} />} workspaceControls={<>
          {import.meta.env.DEV ? <LivePreviewControl /> : null}
          <button type="button" ref={sidebarToggleRef} className="icon-button navigation-toggle" aria-label="打开导航" aria-controls="workspace-sidebar" aria-expanded={sidebarOpen} onClick={() => setSidebarOpen(true)}><Icon name="projects" /></button>
          <button type="button" className="text-button workspace-guide-toggle" aria-label="连接设置" title="连接设置" onClick={() => { setGuideOpen((open) => !open); openContext(); }}><Icon name="settings" size="sm" /><span>连接设置</span></button>
          <button type="button" ref={contextToggleRef} className="icon-button context-toggle" aria-label="打开参考面板" aria-controls="workspace-inspector" aria-expanded={contextOpen} onClick={openContext}><Icon name="activity" /></button>
        </>} />
      </div>
      <main className="workspace-main workspace-inspector" id="workspace-inspector" ref={inspectorRef} tabIndex={-1} aria-label="动态与参考资料">
        {detail && !guideOpen ? <ServerInspectorHeader onBack={() => setPrimary("activity")} onClose={returnToAI} /> : <header className="workspace-header is-titleless">
          <div className="workspace-header-meta">
            <button type="button" className="icon-button context-close" aria-label="回到对话" onClick={returnToAI}><Icon name="close" /></button>
          </div>
        </header>}
        {detail && !guideOpen ? <div className="server-tabs" role="tablist" aria-label="服务器视图">{INSPECTOR_PAGES.map((page, index) => <button key={page} type="button" role="tab" data-attention={`view:${page}`} data-server-id={selectedServerId ?? undefined} id={`server-tab-${page}`} aria-controls="server-view" aria-selected={serverPage === page} tabIndex={serverPage === page ? 0 : -1} onKeyDown={(event) => navigateTab(event, index)} onClick={() => setServerPage(page)} className={`server-tab ${serverPage === page ? "server-tab-active" : ""}`}><span>{SERVER_PAGE_LABEL[page]}</span></button>)}</div> : null}
        <div className="workspace-content" id={detail ? "server-view" : undefined} role={detail && !guideOpen ? "tabpanel" : undefined} aria-labelledby={detail && !guideOpen ? `server-tab-${serverPage}` : undefined} tabIndex={0}>
          {guideOpen ? <Suspense fallback={<PageLoadingFallback />}><GettingStarted onClose={closeGuide} /></Suspense> : null}
          <div hidden={guideOpen} className="workspace-pages">
            {primary === "settings" && !guideOpen ? <Suspense fallback={<PageLoadingFallback />}><RuntimeSettings /></Suspense> : null}
            {primary === "agents" && !guideOpen ? <Suspense fallback={<PageLoadingFallback />}><AgentNistPane onUseAgent={returnToAI} /></Suspense> : null}
            {(primary === "activity" || (primary === "servers" && !detail)) && !guideOpen ? <Suspense fallback={<PageLoadingFallback />}><ActivityFeed /></Suspense> : null}
            {detail && !guideOpen ? <>
              {serverPage === "overview" ? <HostKeyGate serverId={selectedServerId}><Suspense fallback={<PageLoadingFallback />}><ServerOverview key={selectedServerId} /></Suspense></HostKeyGate> : null}
              {serverPage === "logs" ? <HostKeyGate serverId={selectedServerId}><Suspense fallback={<PageLoadingFallback />}><LogsPane /></Suspense></HostKeyGate> : null}
              {serverPage === "services" ? <HostKeyGate serverId={selectedServerId}><Suspense fallback={<PageLoadingFallback />}><ServicesPane /></Suspense></HostKeyGate> : null}
              {serverPage === "activity" ? <Suspense fallback={<PageLoadingFallback />}><ActivityFeed serverId={selectedServerId} /></Suspense> : null}
            </> : null}
          </div>
        </div>
      </main>
      <TeamHeartbeat />
      <ServerAuthChallengeModal />
    </div>
  );
}
