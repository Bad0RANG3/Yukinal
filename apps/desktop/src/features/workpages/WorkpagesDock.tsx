import { lazy, Suspense, useEffect, useId, useRef, useState, type KeyboardEvent } from "react";
import { useQuery } from "@tanstack/react-query";
import { IPC_COMMANDS } from "@yukinal/shared";
import { Icon, type IconName } from "../../components/Icon.js";
import { errorMessage, formatTimestamp } from "../../lib/format.js";
import { callDesktop, isDesktopShell } from "../../lib/ipc.js";
import { useServers } from "../../lib/servers.js";
import { useWorkspaceStore } from "../../stores/workspace-store.js";
import { HostKeyGate } from "../servers/HostKeyGate.js";
import { WorkpageArtifactView } from "./WorkpageArtifactView.js";
import { useWorkpageOperations } from "./useWorkpageOperations.js";
import { artifactFromContent, printableContent, WORKPAGE_LABEL, WORKPAGE_TOOLS, type Workpage, type WorkpageTool } from "./workpage-model.js";
import { useWorkpagesStore } from "./workpages-store.js";
import "./workpages.css";

const TerminalPane = lazy(async () => ({ default: (await import("../terminal/TerminalPane.js")).TerminalPane }));
const RemoteFilesPane = lazy(async () => ({ default: (await import("../files/RemoteFilesPane.js")).RemoteFilesPane }));
const LogsPane = lazy(async () => ({ default: (await import("../logs/LogsPane.js")).LogsPane }));
const ServicesPane = lazy(async () => ({ default: (await import("../services/ServicesPane.js")).ServicesPane }));
const PAGE_ICON: Record<Workpage["kind"], IconName> = { files: "folder", terminal: "terminal", logs: "logs", services: "services", evidence: "activity", artifact: "file" };
export type WorkpagePopOut = (page: Workpage) => Promise<void> | void;

export function WorkpagesLauncher() {
  const serverId = useWorkspaceStore((state) => state.selectedServerId);
  const open = useWorkpagesStore((state) => state.openTool);
  return <div className="workpages-launcher" aria-label="打开工作页">{WORKPAGE_TOOLS.map((kind) => <button key={kind} type="button" className="workpages-launcher-button" aria-label={`打开${WORKPAGE_LABEL[kind]}`} onClick={() => open(kind, { serverId })} title={`打开${WORKPAGE_LABEL[kind]}`}><Icon name={PAGE_ICON[kind]} size="sm" /><span>{WORKPAGE_LABEL[kind]}</span></button>)}</div>;
}

/** A bounded row in the central layout; it never overlays chat or its input. */
export function WorkpagesDock({ onPopOut }: { onPopOut?: WorkpagePopOut }) {
  const state = useWorkpagesStore();
  const dockRef = useRef<HTMLElement>(null);
  const [compact, setCompact] = useState(true);
  useEffect(() => {
    const element = dockRef.current;
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
  const navigateTabs = (event: KeyboardEvent<HTMLButtonElement>, index: number) => {
    const next = event.key === "ArrowRight" ? (index + 1) % state.pages.length : event.key === "ArrowLeft" ? (index + state.pages.length - 1) % state.pages.length : event.key === "Home" ? 0 : event.key === "End" ? state.pages.length - 1 : null;
    if (next === null) return;
    const page = state.pages[next];
    if (page) { event.preventDefault(); state.activate(page.id); document.getElementById(`workpage-tab-${page.id}`)?.focus(); }
  };
  return <section ref={dockRef} className={`workpages-dock ${state.pages.length ? "has-workpages" : ""} ${state.expandedId ? "is-expanded" : ""}`} aria-label="工作页">
    {state.pages.length ? <div className="workpage-tabs" role="tablist" aria-label="已打开的工作页">{state.pages.map((page, index) => <button type="button" key={page.id} role="tab" id={`workpage-tab-${page.id}`} aria-controls={`workpage-panel-${page.id}`} aria-selected={page.id === state.activeId} tabIndex={page.id === state.activeId ? 0 : -1} className={page.id === state.activeId ? "is-active" : ""} onKeyDown={(event) => navigateTabs(event, index)} onClick={() => state.activate(page.id)}><Icon name={PAGE_ICON[page.kind]} size="sm" /><span>{WORKPAGE_LABEL[page.kind]}</span>{page.serverId ? <small>{names.get(page.serverId) ?? page.serverId}</small> : null}</button>)}</div> : null}
    <div className={`workpages-stage ${state.pages.length ? "" : "is-empty"}`}>
      {renderPages.map((page) => {
        const open = state.pages.some((item) => item.id === page.id);
        const visible = open && (state.expandedId ? page.id === state.expandedId : compact ? page.id === state.activeId : page.id === visibleLeft || page.id === visibleRight);
        return <WorkpageFrame key={page.id} page={page} active={page.id === state.activeId} visible={visible} serverName={page.serverId ? names.get(page.serverId) ?? page.serverId : "本机"} expanded={state.expandedId === page.id} onPopOut={onPopOut} />;
      })}
    </div>
    <WorkpageOperations />
  </section>;
}

function WorkpageFrame({ page, active, visible, serverName, expanded, onPopOut }: { page: Workpage; active: boolean; visible: boolean; serverName: string; expanded: boolean; onPopOut?: WorkpagePopOut }) {
  const titleId = useId();
  const { close, setDock, toggleExpanded, activate } = useWorkpagesStore();
  const [popOutError, setPopOutError] = useState<string | null>(null);
  const [opening, setOpening] = useState(false);
  const closeView = () => {
    close(page.id);
    requestAnimationFrame(() => {
      const nextId = useWorkpagesStore.getState().activeId;
      const target = nextId ? document.getElementById(`workpage-tab-${nextId}`) : document.querySelector<HTMLButtonElement>(".workpages-launcher button");
      target?.focus({ preventScroll: true });
    });
  };
  const popOut = async () => {
    if (!onPopOut || opening) return;
    setOpening(true); setPopOutError(null);
    try { await onPopOut(page); }
    catch (error) { setPopOutError(errorMessage(error)); }
    finally { setOpening(false); }
  };
  return <section hidden={!visible} id={`workpage-panel-${page.id}`} role="tabpanel" aria-labelledby={titleId} className={`workpage-frame workpage-dock-${page.dock} ${active ? "is-active" : ""}`} data-active={active} onFocusCapture={() => { if (!active) activate(page.id); }}>
    <header className="workpage-heading"><Icon name={PAGE_ICON[page.kind]} size="sm" /><strong id={titleId} title={page.title}>{page.kind === "artifact" || page.kind === "evidence" ? page.title : WORKPAGE_LABEL[page.kind]}</strong><span className="workpage-target" title={serverName}>{serverName}</span><div className="workpage-controls">
      <button type="button" className="icon-button" aria-label={page.dock === "left" ? "移到右上方" : "移到左上方"} title={page.dock === "left" ? "移到右上方" : "移到左上方"} onClick={() => setDock(page.id, page.dock === "left" ? "right" : "left")}><DockIcon right={page.dock === "left"} /></button>
      <button type="button" className="icon-button" aria-label={expanded ? "还原工作页" : "展开工作页"} title={expanded ? "还原" : "展开"} onClick={() => toggleExpanded(page.id)}><ExpandIcon expanded={expanded} /></button>
      <button type="button" className="icon-button" aria-label={page.kind === "terminal" ? "在独立窗口新建终端会话" : "在独立窗口打开"} title={onPopOut ? page.kind === "terminal" ? "独立窗口 · 新终端会话" : "独立窗口" : "独立窗口需要桌面应用"} disabled={!onPopOut || opening} onClick={() => void popOut()}><Icon name="externalLink" size="sm" /></button>
      <button type="button" className="icon-button" aria-label={`关闭${WORKPAGE_LABEL[page.kind]}工作页`} title={page.kind === "terminal" ? "关闭视图 · 终端会话继续" : "关闭视图"} onClick={closeView}><Icon name="close" size="sm" /></button>
    </div></header>
    {popOutError ? <div className="workpage-inline-error" role="alert">{popOutError}</div> : null}
    <div className="workpage-body"><WorkpageContent page={page} active={visible} /></div>
  </section>;
}

/** Reused by a native standalone window; the target is never the global selection. */
export function WorkpageContent({ page, active = true }: { page: Workpage; active?: boolean }) {
  if (page.kind === "artifact") return <WorkpageArtifactView artifact={page.artifact} />;
  if (page.kind === "evidence") return <WorkpageEvidenceView page={page} />;
  return <HostKeyGate serverId={page.serverId}><Suspense fallback={<div className="workpage-empty" role="status">加载中…</div>}>
    {page.kind === "terminal" ? <TerminalPane serverId={page.serverId} active={active} /> : page.kind === "files" ? <RemoteFilesPane serverId={page.serverId} /> : page.kind === "logs" ? <LogsPane serverId={page.serverId} /> : <ServicesPane serverId={page.serverId} />}
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
    <details open><summary>输入</summary><pre><code>{printableContent(execution.input)}</code></pre></details>
    <details open><summary>结果 · {execution.status}{execution.durationMs === undefined ? "" : ` · ${execution.durationMs}ms`}</summary>{execution.error ? <div className="workpage-inline-error">{execution.error}</div> : null}<pre><code>{printableContent(output)}</code></pre></details>
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
