import { lazy, Suspense, useCallback, useEffect, useId, useLayoutEffect, useRef, useState, type KeyboardEvent, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { useQuery } from "@tanstack/react-query";
import { IPC_COMMANDS } from "@yukinal/shared";
import { Icon, type IconName } from "../../components/Icon.js";
import { useDismissOnOutsidePointer } from "../../hooks/useDismissOnOutsidePointer.js";
import { errorMessage, formatTimestamp } from "../../lib/format.js";
import { callDesktop, isDesktopShell } from "../../lib/ipc.js";
import { useServers } from "../../lib/servers.js";
import { DECISION_LABEL, EXECUTION_STATUS_LABEL, approvalSourceLabel, environmentLabel, riskLabel } from "../../lib/labels.js";
import { useWorkspaceStore } from "../../stores/workspace-store.js";
import { HostKeyGate } from "../servers/HostKeyGate.js";
import { TeamActionCue, TeamOperationOutline } from "../agent-nist/TeamActivity.js";
import { TeamBoardPane } from "../agent-nist/TeamBoardPane.js";
import { WorkpageArtifactView } from "./WorkpageArtifactView.js";
import { useWorkpageOperations } from "./useWorkpageOperations.js";
import { artifactFromContent, printableContent, WORKPAGE_LABEL, WORKPAGE_TOOLS, type Workpage, type WorkpageTool } from "./workpage-model.js";
import { useWorkpagesStore } from "./workpages-store.js";
import "./workpages.css";

const TerminalPane = lazy(async () => ({ default: (await import("../terminal/TerminalPane.js")).TerminalPane }));
const RemoteFilesPane = lazy(async () => ({ default: (await import("../files/RemoteFilesPane.js")).RemoteFilesPane }));
const LogsPane = lazy(async () => ({ default: (await import("../logs/LogsPane.js")).LogsPane }));
const ServicesPane = lazy(async () => ({ default: (await import("../services/ServicesPane.js")).ServicesPane }));
const PAGE_ICON: Record<Workpage["kind"], IconName> = { files: "folder", terminal: "terminal", logs: "logs", services: "services", evidence: "activity", artifact: "file", board: "board" };
export type WorkpagePopOut = (page: Workpage) => Promise<void> | void;

function tabId(page: Workpage) { return `workpage-tab-${page.pinned ? `fixed-${page.kind}` : page.id}`; }
function focusActiveTab() {
  const state = useWorkpagesStore.getState();
  const page = state.pages.find((item) => item.id === state.activeId);
  document.getElementById(page ? tabId(page) : "workpage-tab-fixed-files")?.focus({ preventScroll: true });
}

export function WorkpagesLauncher({ onOpen, onChooseTarget, additional = false }: { onOpen?: () => void; onChooseTarget?: () => void; additional?: boolean } = {}) {
  const serverId = useWorkspaceStore((state) => state.selectedServerId);
  const open = useWorkpagesStore((state) => state.openTool);
  const active = useWorkpagesStore((state) => state.collapsed ? null : state.pages.find((page) => page.id === state.activeId));
  const openFromLauncher = (kind: WorkpageTool) => {
    if (!serverId) {
      onChooseTarget?.();
      onOpen?.();
      return;
    }
    open(kind, { serverId, additional });
    onOpen?.();
    requestAnimationFrame(focusActiveTab);
  };
  return <div className="workpages-launcher" aria-label="打开工作页">{WORKPAGE_TOOLS.map((kind) => <button key={kind} type="button" className="workpages-launcher-button team-operation-target" data-attention={`tool:${kind}`} data-server-id={serverId ?? undefined} aria-label={`${additional ? "新建" : "打开"}${WORKPAGE_LABEL[kind]}工作页`} aria-pressed={!additional && active?.kind === kind && active.serverId === serverId} onClick={() => openFromLauncher(kind)}><Icon name={PAGE_ICON[kind]} size="sm" /><span className="workpages-tool-label">{WORKPAGE_LABEL[kind]}</span><TeamActionCue serverId={serverId} surface={kind} /><TeamOperationOutline serverId={serverId} surface={kind} /></button>)}</div>;
}

function WorkpageAddMenu({ onChooseTarget }: { onChooseTarget?: () => void }) {
  const [open, setOpen] = useState(false);
  const root = useRef<HTMLDivElement>(null);
  const trigger = useRef<HTMLButtonElement>(null);
  const dismiss = useCallback(() => setOpen(false), []);
  useDismissOnOutsidePointer(open, [root], dismiss);
  useEffect(() => { if (open) root.current?.querySelector<HTMLButtonElement>(".workpages-launcher button")?.focus(); }, [open]);
  return <div className="workpages-add" ref={root} onKeyDown={(event) => {
    if (event.key === "Escape" && open) { event.preventDefault(); event.stopPropagation(); setOpen(false); trigger.current?.focus(); }
  }}>
    <button ref={trigger} type="button" className="icon-button workpages-add-trigger" aria-label="新增工作页" title="新增工作页" aria-haspopup="dialog" aria-expanded={open} onClick={() => setOpen((current) => !current)}><Icon name="plus" size="sm" /></button>
    {open ? <div className="workpages-add-menu" role="dialog" aria-label="新增工作页"><WorkpagesLauncher additional onOpen={() => setOpen(false)} onChooseTarget={onChooseTarget} /></div> : null}
  </div>;
}

function WorkpagesToolbar({ tabs, onChooseTarget }: { tabs: ReactNode; onChooseTarget?: () => void }) {
  const state = useWorkpagesStore();
  const hasInformation = state.pages.length > 0 || state.operationsOpen;
  return <div className="workpages-toolbar has-tabs">
    {tabs}<WorkpageAddMenu onChooseTarget={onChooseTarget} />
    <div className="workpages-active-controls" />
    <div className="workpages-toolbar-actions">
      <button type="button" className="workpages-records-button" aria-label="操作记录" aria-pressed={state.operationsOpen && !state.collapsed} onClick={() => state.setOperationsOpen(state.collapsed || !state.operationsOpen)} title="操作记录、证据与最近关闭的工作页"><Icon name="activity" size="sm" /><span>操作记录</span></button>
      <button type="button" className="workpages-layer-toggle" disabled={!hasInformation} aria-label={state.collapsed ? "恢复信息层" : "收起信息层，展开对话"} title={state.collapsed ? "恢复已打开的工作页" : "收起信息层，保留工作页与终端会话"} onClick={() => state.setCollapsed(!state.collapsed)}><Icon name={state.collapsed ? "projects" : "chevronRight"} size="sm" /><span>{state.collapsed ? "工作页" : "回到对话"}</span></button>
    </div>
  </div>;
}

/** Information occupies the middle surface; chat has its own foreground sheet. */
export function WorkpagesDock({ onPopOut, onChooseTarget }: { onPopOut?: WorkpagePopOut; onChooseTarget?: () => void }) {
  const state = useWorkpagesStore();
  const serverId = useWorkspaceStore((state) => state.selectedServerId);
  const dockRef = useRef<HTMLElement>(null);
  const [compact, setCompact] = useState(true);
  useEffect(() => {
    const element = dockRef.current?.querySelector(".workpages-stage");
    if (!element) return;
    const measure = () => setCompact(element.getBoundingClientRect().width <= 720);
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(element);
    return () => observer.disconnect();
  }, []);
  const servers = useServers();
  const names = new Map((servers.data ?? []).map((server) => [server.id, server.name]));
  const retained = state.retainedTerminals.filter((page) => !state.pages.some((item) => item.id === page.id));
  const renderPages = [...state.pages, ...retained];
  const active = state.pages.find((page) => page.id === state.activeId);
  const visibleLeft = active?.dock === "left" ? active.id : state.pages.findLast((page) => page.dock === "left")?.id;
  const visibleRight = active?.dock === "right" ? active.id : state.pages.findLast((page) => page.dock === "right")?.id;
  const isVisible = (page: Workpage) => !state.collapsed && state.pages.some((item) => item.id === page.id) && (state.expandedId ? page.id === state.expandedId : compact ? page.id === state.activeId : page.id === visibleLeft || page.id === visibleRight);
  const visibleCount = state.pages.filter(isVisible).length;
  const tabs = [
    ...WORKPAGE_TOOLS.map((kind) => ({ id: `workpage-tab-fixed-${kind}`, kind, fixed: true, page: state.pages.find((page) => page.pinned && page.kind === kind) })),
    ...state.pages.filter((page) => !page.pinned).map((page) => ({ id: tabId(page), kind: page.kind, fixed: false, page })),
  ];
  const selectTab = (index: number) => {
    const tab = tabs[index];
    if (!tab) return;
    if (tab.fixed && serverId) state.openTool(tab.kind as WorkpageTool, { serverId });
    else if (tab.fixed) onChooseTarget?.();
    else if (tab.page) state.activate(tab.page.id);
  };
  const selectedIndex = state.collapsed ? -1 : tabs.findIndex((tab) => tab.page?.id === state.activeId);
  const navigateTabs = (event: KeyboardEvent<HTMLButtonElement>, index: number) => {
    const next = event.key === "ArrowRight" ? (index + 1) % tabs.length : event.key === "ArrowLeft" ? (index + tabs.length - 1) % tabs.length : event.key === "Home" ? 0 : event.key === "End" ? tabs.length - 1 : null;
    if (next === null) return;
    event.preventDefault(); selectTab(next); document.getElementById(tabs[next]!.id)?.focus({ preventScroll: true });
  };
  return <section ref={dockRef} data-compact={compact} className={`workpages-dock ${state.pages.length ? "has-workpages" : ""} ${state.expandedId ? "is-expanded" : ""} ${state.collapsed ? "is-collapsed" : ""}`} aria-label="工作页">
    <WorkpagesToolbar onChooseTarget={onChooseTarget} tabs={<div className="workpage-tabs" role="tablist" aria-label="工作页标签">{tabs.map((tab, index) => {
      const page = tab.page;
      const label = page && (page.kind === "artifact" || page.kind === "evidence") ? page.title : WORKPAGE_LABEL[tab.kind];
      const target = tab.fixed ? serverId : page?.serverId;
      return <button type="button" key={tab.id} role="tab" id={tab.id} aria-label={`${label}工作页`} aria-controls={page ? `workpage-panel-${page.id}` : undefined} aria-selected={index === selectedIndex} tabIndex={index === (selectedIndex < 0 ? 0 : selectedIndex) ? 0 : -1} className={`team-operation-target ${tab.fixed ? "is-fixed" : ""} ${index === selectedIndex ? "is-active" : ""}`} title={[label, target ? names.get(target) ?? target : "", tab.fixed ? "固定工作页" : ""].filter(Boolean).join(" · ")} data-attention={`tool:${tab.kind}`} data-server-id={target ?? undefined} onKeyDown={(event) => navigateTabs(event, index)} onClick={() => selectTab(index)}><Icon name={PAGE_ICON[tab.kind]} size="sm" /><span>{label}</span>{!tab.fixed && page?.serverId ? <small>{names.get(page.serverId) ?? page.serverId}</small> : null}{WORKPAGE_TOOLS.includes(tab.kind as WorkpageTool) ? <><TeamActionCue serverId={target ?? null} surface={tab.kind as WorkpageTool} /><TeamOperationOutline serverId={target ?? null} surface={tab.kind as WorkpageTool} /></> : null}</button>;
    })}</div>} />
    {/* Keep the stage in its existing child slot so hot updates retain mounted PTYs. */}
    {null}
    <div className={`workpages-stage ${state.pages.length ? "" : "is-empty"}`}>
      {renderPages.map((page) => {
        const open = state.pages.some((item) => item.id === page.id);
        const visible = isVisible(page);
        return <WorkpageFrame key={page.id} page={page} active={page.id === state.activeId} open={open} visible={visible} promoteControls={visible && visibleCount === 1} serverName={page.serverId ? names.get(page.serverId) ?? page.serverId : "本机"} expanded={state.expandedId === page.id} onPopOut={onPopOut} />;
      })}
    </div>
    <WorkpageOperations />
  </section>;
}

function WorkpageHeading({ page, titleId, serverName, promoteControls, children }: { page: Workpage; titleId: string; serverName: string; promoteControls: boolean; children: ReactNode }) {
  const heading = useRef<HTMLElement>(null);
  const [target, setTarget] = useState<Element | null>(null);
  useLayoutEffect(() => {
    setTarget(promoteControls ? heading.current?.closest(".workpages-dock")?.querySelector(".workpages-active-controls") ?? null : null);
  }, [promoteControls]);
  return <><header ref={heading} hidden={Boolean(target)} className="workpage-heading"><Icon name={PAGE_ICON[page.kind]} size="sm" /><strong id={titleId} title={page.kind === "artifact" || page.kind === "evidence" ? page.title : undefined}>{page.kind === "artifact" || page.kind === "evidence" ? page.title : WORKPAGE_LABEL[page.kind]}</strong><span className="workpage-target" title={serverName}>{serverName}</span>{target ? null : children}</header>{target ? createPortal(children, target) : null}</>;
}

function WorkpageFrame({ page, active, open, visible, promoteControls, serverName, expanded, onPopOut }: { page: Workpage; active: boolean; open: boolean; visible: boolean; promoteControls: boolean; serverName: string; expanded: boolean; onPopOut?: WorkpagePopOut }) {
  const titleId = useId();
  const { close, setDock, toggleExpanded, activate } = useWorkpagesStore();
  const [popOutError, setPopOutError] = useState<string | null>(null);
  const [opening, setOpening] = useState(false);
  const closeView = () => {
    close(page.id);
    requestAnimationFrame(() => {
      focusActiveTab();
    });
  };
  const popOut = async () => {
    if (!onPopOut || opening) return;
    setOpening(true); setPopOutError(null);
    try { await onPopOut(page); }
    catch (error) { setPopOutError(errorMessage(error)); }
    finally { setOpening(false); }
  };
  return <section hidden={!visible} id={open ? `workpage-panel-${page.id}` : undefined} role={open ? "tabpanel" : undefined} aria-labelledby={open ? titleId : undefined} className={`workpage-frame team-operation-target workpage-kind-${page.kind} workpage-dock-${page.dock} ${active ? "is-active" : ""}`} data-active={active} onFocusCapture={() => { if (open && !active) activate(page.id); }}>
    <WorkpageHeading page={page} titleId={titleId} serverName={serverName} promoteControls={promoteControls}><div className="workpage-controls">
      <button type="button" className="icon-button workpage-dock-control" aria-label={page.dock === "left" ? "移到右侧" : "移到左侧"} title={page.dock === "left" ? "移到右侧" : "移到左侧"} onClick={() => setDock(page.id, page.dock === "left" ? "right" : "left")}><DockIcon right={page.dock === "left"} /></button>
      <button type="button" className="icon-button" aria-label={expanded ? "还原工作页" : "展开工作页"} title={expanded ? "还原" : "展开"} onClick={() => toggleExpanded(page.id)}><ExpandIcon expanded={expanded} /></button>
      <button type="button" className="icon-button" aria-label={page.kind === "terminal" ? "在独立窗口新建终端会话" : "在独立窗口打开"} title={onPopOut ? page.kind === "terminal" ? "独立窗口 · 新终端会话" : "独立窗口" : "独立窗口需要桌面应用"} disabled={!onPopOut || opening || page.kind === "board"} onClick={() => void popOut()}><Icon name="externalLink" size="sm" /></button>
      {!page.pinned ? <button type="button" className="icon-button" aria-label={`关闭${WORKPAGE_LABEL[page.kind]}工作页`} title={page.kind === "terminal" ? "关闭视图 · 终端会话继续" : "关闭视图"} onClick={closeView}><Icon name="close" size="sm" /></button> : null}
    </div></WorkpageHeading>
    {popOutError ? <div className="workpage-inline-error" role="alert">{popOutError}</div> : null}
    <div className="workpage-body"><WorkpageContent page={page} active={visible} /></div>
    {page.kind === "terminal" && page.serverId ? <TerminalStatus serverId={page.serverId} /> : null}
    {WORKPAGE_TOOLS.includes(page.kind as WorkpageTool) ? <TeamOperationOutline serverId={page.serverId} surface={page.kind as WorkpageTool} /> : null}
  </section>;
}

function TerminalStatus({ serverId }: { serverId: string }) {
  const servers = useServers();
  const connection = servers.data?.find((server) => server.id === serverId)?.connection;
  const host = connection?.host.includes(":") ? `[${connection.host}]` : connection?.host;
  const target = connection ? `${connection.username}@${host}:${connection.port}` : null;
  return <footer className="workpage-terminal-status"><span>SSH</span>{target ? <span className="terminal-status-target" title={target}>{target}</span> : null}<span className="terminal-status-encoding">UTF-8</span></footer>;
}

/** Reused by a native standalone window; the target is never the global selection. */
export function WorkpageContent({ page, active = true }: { page: Workpage; active?: boolean }) {
  if (page.kind === "board") return <TeamBoardPane serverId={page.serverId} expanded />;
  if (page.kind === "artifact") return <WorkpageArtifactView artifact={page.artifact} />;
  if (page.kind === "evidence") return <WorkpageEvidenceView page={page} />;
  if (!page.serverId && (page.kind === "terminal" || page.kind === "logs")) return <div className="workpage-unbound" aria-label={WORKPAGE_LABEL[page.kind]} />;
  return <HostKeyGate serverId={page.serverId}><Suspense fallback={<div className="workpage-loading" role="status" aria-label="加载工作页" />}>
    {page.kind === "terminal" ? <TerminalPane serverId={page.serverId} active={active} /> : page.kind === "files" ? <RemoteFilesPane serverId={page.serverId} /> : page.kind === "logs" ? <LogsPane serverId={page.serverId} active={active} /> : <ServicesPane serverId={page.serverId} active={active} />}
  </Suspense></HostKeyGate>;
}

function WorkpageEvidenceView({ page }: { page: Extract<Workpage, { kind: "evidence" }> }) {
  const { execution, actor, target } = page.evidence;
  const openTool = useWorkpagesStore((state) => state.openTool);
  const openArtifact = useWorkpagesStore((state) => state.openArtifact);
  const kind = toolWorkpage(execution.toolName);
  const output = execution.output;
  const envelope = output && typeof output === "object" && "artifact" in output ? output.artifact : null;
  return <div className="workpage-evidence"><div className="workpage-evidence-meta"><span>{actor}</span><span>{target}</span><time dateTime={execution.startedAt}>{formatTimestamp(execution.startedAt)}</time></div>
    <div className="workpage-evidence-actions">{kind ? <button type="button" className="button-secondary" onClick={() => openTool(kind, { serverId: execution.serverId ?? null })}><Icon name={PAGE_ICON[kind]} size="sm" />查看{WORKPAGE_LABEL[kind]}</button> : null}{envelope ? <button type="button" className="button-secondary" onClick={() => openArtifact(artifactFromContent(`${execution.callId}:artifact`, execution.toolName, envelope, execution.serverId))}>打开产物</button> : null}<code title={execution.traceId}>{execution.stepId}</code></div>
    <dl className="workpage-evidence-facts">
      <div><dt>执行状态</dt><dd>{EXECUTION_STATUS_LABEL[execution.status]}</dd></div>
      <div><dt>环境</dt><dd>{environmentLabel(execution.environment)}</dd></div>
      <div><dt>风险</dt><dd>{riskLabel(execution.riskLevel)}</dd></div>
      <div><dt>策略</dt><dd>{DECISION_LABEL[execution.decision]}</dd></div>
      {execution.approvedBy ? <div><dt>批准来源</dt><dd>{approvalSourceLabel(execution.approvedBy)}</dd></div> : null}
      {execution.durationMs !== undefined ? <div><dt>耗时</dt><dd>{execution.durationMs}ms</dd></div> : null}
    </dl>
    <details open><summary>输入</summary><pre><code>{printableContent(execution.input)}</code></pre></details>
    <details open><summary>结果 · {EXECUTION_STATUS_LABEL[execution.status]}{execution.durationMs === undefined ? "" : ` · ${execution.durationMs}ms`}</summary>{execution.error ? <div className="workpage-inline-error">{execution.error}</div> : null}<pre><code>{printableContent(output)}</code></pre></details>
  </div>;
}

function WorkpageOperations() {
  const operations = useWorkpageOperations();
  const { operationsOpen, setOperationsOpen, openEvidence, openArtifact, recent, reopen } = useWorkpagesStore();
  const selectedTaskId = useWorkspaceStore((state) => state.selectedTaskId);
  const task = useQuery({ queryKey: ["investigationTask", selectedTaskId], enabled: operationsOpen && Boolean(selectedTaskId) && isDesktopShell(), staleTime: 10_000, queryFn: () => callDesktop(IPC_COMMANDS.investigationTaskGet, { taskId: selectedTaskId as string }) });
  const [tab, setTab] = useState<"operations" | "artifacts" | "recent">("operations");
  const latest = operations.rows[0];
  return <div className={`workpage-operations ${operationsOpen ? "is-open" : ""}`}>
    <button type="button" className="workpage-operations-toggle" aria-expanded={operationsOpen} onClick={() => setOperationsOpen(!operationsOpen)}><Icon name="activity" size="sm" /><span>操作记录</span>{latest ? <><span className="workpage-operation-latest">{latest.actor} · {latest.target} · {latest.title}</span><span className={`workpage-operation-status status-${latest.status}`}>{latest.statusLabel}</span></> : <span className="workpage-operation-latest">{operations.isLoading ? "读取中…" : "暂无操作"}</span>}<Icon name={operationsOpen ? "chevronUp" : "chevronDown"} size="sm" /></button>
    {operationsOpen ? <div className="workpage-operations-detail"><div className="workpage-operations-tabs"><button type="button" aria-pressed={tab === "operations"} onClick={() => setTab("operations")}>操作 · {operations.rows.length}</button><button type="button" aria-pressed={tab === "artifacts"} onClick={() => setTab("artifacts")}>产物 · {task.data?.artifacts.length ?? 0}</button><button type="button" aria-pressed={tab === "recent"} onClick={() => setTab("recent")}>最近关闭 · {recent.length}</button><button type="button" className="icon-button" aria-label="刷新操作记录" title="刷新" onClick={operations.refresh}><Icon name="refresh" size="sm" /></button></div>
      {tab === "operations" ? <>{operations.error ? <div role="alert" className="workpage-inline-error">{errorMessage(operations.error)}</div> : null}{operations.rows.length ? <ol className="workpage-operation-list">{operations.rows.map((row) => <li key={row.id}><span className="workpage-operation-actor" title={row.actor}>{row.actor}</span><span className="workpage-operation-target" title={row.target}>{row.target}</span><span className="workpage-operation-action" title={row.title}>{row.title}</span><span className={`workpage-operation-status status-${row.status}`}>{row.statusLabel}</span>{row.evidence ? <button type="button" className="text-button" onClick={() => { if (row.evidence) openEvidence(row.evidence); }}>证据</button> : <time dateTime={row.at}>{formatTimestamp(row.at)}</time>}</li>)}</ol> : <div className="workpage-empty">暂无操作记录</div>}</> : null}
      {tab === "artifacts" ? <>{task.isLoading ? <div className="workpage-empty" role="status">读取中…</div> : task.error ? <div className="workpage-inline-error" role="alert">{errorMessage(task.error)}</div> : task.data?.artifacts.length ? <ul className="workpage-recent-list">{task.data.artifacts.map((artifact) => <li key={artifact.id}><button type="button" onClick={() => openArtifact(artifactFromContent(artifact.id, artifact.title, artifact.content, task.data?.task.scope.serverId ?? null))}><Icon name="file" size="sm" /><span>{artifact.title}</span><small>{artifact.status}</small></button></li>)}</ul> : <div className="workpage-empty">暂无产物</div>}</> : null}
      {tab === "recent" ? recent.length ? <ul className="workpage-recent-list">{recent.map((page) => <li key={page.id}><button type="button" onClick={() => reopen(page)}><Icon name={PAGE_ICON[page.kind]} size="sm" /><span>{page.title}</span><small>重新打开</small></button></li>)}</ul> : <div className="workpage-empty">暂无关闭的工作页</div> : null}
    </div> : null}
  </div>;
}

function toolWorkpage(toolName: string): WorkpageTool | null {
  if (/logs|journal/.test(toolName)) return "logs";
  if (/services|docker\.(ps|inspect)/.test(toolName)) return "services";
  if (/file|directory|sftp/.test(toolName)) return "files";
  if (/exec|terminal|shell/.test(toolName)) return "terminal";
  return null;
}
function DockIcon({ right }: { right: boolean }) { return <svg className="icon" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" aria-hidden="true"><rect x="3" y="4" width="18" height="16" rx="2" /><path d={right ? "M14 4v16M7 12h4m-2-2 2 2-2 2" : "M10 4v16M17 12h-4m2-2-2 2 2 2"} /></svg>; }
function ExpandIcon({ expanded }: { expanded: boolean }) { return <svg className="icon" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" aria-hidden="true"><path d={expanded ? "M4 9h5V4m11 11h-5v5M9 9 3 3m12 12 6 6" : "M9 4H4v5m11 11h5v-5M4 4l6 6m10 10-6-6"} /></svg>; }
