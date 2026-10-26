import { lazy, Suspense, useId, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { IPC_COMMANDS } from "@yukinal/shared";
import { Icon, type IconName } from "../../components/Icon.js";
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
type ChooseTarget = (kind: WorkpageTool) => void;
const isTool = (page: Workpage | undefined): page is Workpage & { kind: WorkpageTool } => Boolean(page && WORKPAGE_TOOLS.includes(page.kind as WorkpageTool));

/** The four tools switch one main surface. Inactive views remain mounted. */
export function WorkpagesLauncher({ onChooseTarget }: { onChooseTarget?: ChooseTarget } = {}) {
  const serverId = useWorkspaceStore((state) => state.selectedServerId);
  const openTool = useWorkpagesStore((state) => state.openTool);
  const active = useWorkpagesStore((state) => state.collapsed ? undefined : state.pages.find((page) => page.id === state.activeId));
  return <nav className="workpages-launcher" aria-label="服务器工具">{WORKPAGE_TOOLS.map((kind) => <button key={kind} type="button" className="workpages-launcher-button team-operation-target" data-workspace-view={kind} data-attention={`tool:${kind}`} data-server-id={serverId ?? undefined} aria-label={WORKPAGE_LABEL[kind]} aria-pressed={active?.kind === kind} aria-controls={active?.kind === kind ? `workpage-panel-${active.id}` : undefined} onClick={() => {
    openTool(kind, { serverId });
    if (!serverId) onChooseTarget?.(kind);
  }}><Icon name={PAGE_ICON[kind]} size="sm" /><span>{WORKPAGE_LABEL[kind]}</span><TeamActionCue serverId={serverId} surface={kind} /><TeamOperationOutline serverId={serverId} surface={kind} /></button>)}</nav>;
}

export function WorkpagesDock({ onChooseTarget }: { onChooseTarget?: ChooseTarget }) {
  const state = useWorkpagesStore();
  const serverId = useWorkspaceStore((state) => state.selectedServerId);
  const servers = useServers();
  const names = new Map((servers.data ?? []).map((server) => [server.id, server.name]));
  const retained = state.retainedTerminals.filter((page) => !state.pages.some((item) => item.id === page.id));
  const renderPages = [...state.pages, ...retained];
  const active = state.pages.find((page) => page.id === state.activeId);
  const targetId = !state.collapsed && isTool(active) ? active.serverId : serverId;
  const targetName = targetId ? names.get(targetId) ?? targetId : "选择服务器";
  return <section className={`workpages-dock is-expanded ${state.pages.length ? "has-workpages" : ""} ${state.collapsed ? "is-collapsed" : ""}`} aria-label="工作区">
    <div className="workpages-toolbar">
      <WorkpagesLauncher onChooseTarget={onChooseTarget} />
      <button type="button" className="workpages-target-switch" aria-label={`切换工作区服务器：${targetName}`} title={targetName} onClick={() => onChooseTarget?.(isTool(active) ? active.kind : "files")}><Icon name="servers" size="sm" /><span>{targetName}</span><Icon name="chevronDown" size="xs" /></button>
    </div>
    {null}
    <div className={`workpages-stage ${state.pages.length ? "" : "is-empty"}`}>
      {renderPages.map((page) => <WorkpageFrame key={page.id} page={page} active={!state.collapsed && page.id === state.activeId && state.pages.some((item) => item.id === page.id)} serverName={page.serverId ? names.get(page.serverId) ?? page.serverId : "未选择服务器"} onChooseTarget={onChooseTarget} />)}
    </div>
    <WorkpageOperations />
  </section>;
}

function WorkpageFrame({ page, active, serverName, onChooseTarget }: { page: Workpage; active: boolean; serverName: string; onChooseTarget?: ChooseTarget }) {
  const titleId = useId();
  const result = page.kind === "artifact" || page.kind === "evidence";
  return <section hidden={!active} id={`workpage-panel-${page.id}`} role="region" aria-label={`${WORKPAGE_LABEL[page.kind]} · ${serverName}`} aria-labelledby={result ? titleId : undefined} className={`workpage-frame team-operation-target workpage-kind-${page.kind} ${active ? "is-active" : ""}`} data-active={active}>
    {result ? <header className="workpage-heading"><Icon name={PAGE_ICON[page.kind]} size="sm" /><strong id={titleId} title={page.title}>{page.title}</strong><span className="workpage-target" title={serverName}>{serverName}</span><button type="button" className="icon-button" aria-label={`关闭${WORKPAGE_LABEL[page.kind]}`} onClick={() => useWorkpagesStore.getState().close(page.id)}><Icon name="close" size="sm" /></button></header> : null}
    <div className="workpage-body"><WorkpageContent page={page} active={active} onChooseTarget={onChooseTarget ? () => onChooseTarget(page.kind as WorkpageTool) : undefined} /></div>
    {page.kind === "terminal" && page.serverId ? <TerminalStatus serverId={page.serverId} /> : null}
    {isTool(page) ? <TeamOperationOutline serverId={page.serverId} surface={page.kind} /> : null}
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
export function WorkpageContent({ page, active = true, onChooseTarget }: { page: Workpage; active?: boolean; onChooseTarget?: () => void }) {
  if (page.kind === "board") return <TeamBoardPane serverId={page.serverId} expanded />;
  if (page.kind === "artifact") return <WorkpageArtifactView artifact={page.artifact} />;
  if (page.kind === "evidence") return <WorkpageEvidenceView page={page} />;
  if (!page.serverId) return <div className="workpage-unbound" role="status"><Icon name={PAGE_ICON[page.kind]} size="md" /><strong>选择服务器以使用{WORKPAGE_LABEL[page.kind]}</strong><p>从左侧服务器列表选择目标，所选工具会直接打开。</p>{onChooseTarget ? <button type="button" className="button-secondary" onClick={onChooseTarget}>选择服务器</button> : null}</div>;
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
