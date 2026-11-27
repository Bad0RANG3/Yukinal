import { useId, useState } from "react";

import { Icon } from "../../components/Icon.js";
import { errorMessage } from "../../lib/format.js";
import { useServers } from "../../lib/servers.js";
import { useWorkspaceStore } from "../../stores/workspace-store.js";
import { useAgentModels } from "../agent/useAgentModels.js";
import { MemoryLibrary } from "../agent/MemoryLibrary.js";
import { ActivityFeed } from "../activity/ActivityFeed.js";
import { TeamBoardPane } from "./TeamBoardPane.js";
import { TeamWatchPane } from "./TeamWatchPane.js";
import { useTeamRuntimeStore, teamRunActive } from "./team-runtime-store.js";

/**
 * One workbench for everything that runs without the user watching: the task
 * board, the registered watch plans, the activity stream and the model the agent
 * runs on. Keeping them as tabs of one page is why the sidebar needs a single
 * entry instead of four views of the same background work.
 */
const PANEL_TABS = [
  { id: "tasks", label: "任务" },
  { id: "watch", label: "值守" },
  { id: "activity", label: "动态" },
  { id: "model", label: "模型" },
] as const;

export function AgentNistPane({ onUseAgent }: { onUseAgent?: () => void }) {
  const servers = useServers({ enabled: false });
  const models = useAgentModels();
  const runs = useTeamRuntimeStore((state) => state.runs);
  const watchUpdates = useTeamRuntimeStore((state) => state.watchUpdates);
  const view = useTeamRuntimeStore((state) => state.panelView);
  const setView = useTeamRuntimeStore((state) => state.setPanelView);
  const selectedServerId = useWorkspaceStore((state) => state.selectedServerId);
  const setPrimary = useWorkspaceStore((state) => state.setPrimary);
  const [memoryOpen, setMemoryOpen] = useState(false);
  const instanceId = useId();
  const activeRuns = [...runs].reverse().filter((run) => teamRunActive(run) && (!selectedServerId || run.serverId === selectedServerId));
  const modelError = models.providers.isError ? `无法读取 AI 连接：${errorMessage(models.providers.error)}` : null;

  return <section className="agent-nist-page" aria-labelledby={`${instanceId}-title`}>
    <header className="agent-nist-heading">
      <div><h2 id={`${instanceId}-title`}>运维 Agent</h2></div>
      <div className="agent-nist-heading-actions">
        <button type="button" className="agent-nist-text-action" onClick={() => setMemoryOpen(true)}><Icon name="archive" size="sm" />记忆库</button>
        <button type="button" className="button-primary" disabled={!models.providerReady} onClick={() => onUseAgent?.()}>打开运维对话</button>
      </div>
    </header>
    <div className="team-workbench-tabs" role="group" aria-label="运维 Agent 页面">
      {PANEL_TABS.map((tab) => <button type="button" key={tab.id} aria-pressed={view === tab.id} onClick={() => setView(tab.id)}>
        {tab.label}{tab.id === "watch" && watchUpdates.some((update) => !selectedServerId || update.serverId === selectedServerId) ? <span className="team-watch-unread" aria-label="有最新状态" title="值守有最新状态">新</span> : null}
      </button>)}
    </div>
    <div className="team-current-scope"><Icon name={selectedServerId ? "servers" : "projects"} size="xs" /><span title={selectedServerId ? servers.data?.find((server) => server.id === selectedServerId)?.name : undefined}>{selectedServerId ? servers.data?.find((server) => server.id === selectedServerId)?.name ?? selectedServerId : "全局工作区"}</span>{activeRuns.length ? <span className="team-current-scope-runs">{activeRuns.length} 个运行中</span> : null}</div>
    {models.providers.isLoading ? <p className="agent-nist-notice" role="status">正在读取 AI 连接…</p> : null}
    {modelError ? <div className="agent-nist-notice is-error" role="alert"><p>{modelError}</p><button type="button" className="agent-nist-text-action" disabled={models.providers.isFetching} onClick={() => void models.providers.refetch()}>重新读取</button></div> : null}

    {view === "tasks" ? <TeamBoardPane serverId={selectedServerId} /> : null}

    {view === "watch" ? <TeamWatchPane /> : null}

    {view === "activity" ? <ActivityFeed serverId={selectedServerId} /> : null}

    {view === "model" ? <section className="team-config-section" aria-labelledby={`${instanceId}-model-title`}>
      <header><h3 id={`${instanceId}-model-title`}>运维 Agent 模型</h3></header>
      <p>Agent 与它启动的 subagent 共用当前对话模型。</p>
      <label className="form-field"><span>AI 连接与模型</span>
        <select className="form-select" aria-label="运维 Agent 模型" value={models.selectedModelKey ?? ""} disabled={!models.modelChoices.length} onChange={(event) => { if (event.target.value) models.selectModelKey(event.target.value); }}>
          {!models.modelChoices.length ? <option value="">请先配置 AI 连接</option> : null}
          {models.modelChoices.map((choice) => <option key={choice.key} value={choice.key}>{choice.providerLabel} · {choice.label}</option>)}
        </select>
      </label>
      <p className="team-config-hint">当前使用：{models.selectedModelLabel ?? (models.providers.isLoading ? "正在读取…" : "未配置模型")}</p>
      <div className="team-config-actions"><button type="button" className="button-secondary" onClick={() => setPrimary("settings")}>管理 AI 连接<Icon name="chevronRight" size="xs" /></button></div>
    </section> : null}

    {memoryOpen ? <MemoryLibrary key={selectedServerId ?? "global"} onClose={() => setMemoryOpen(false)} /> : null}
  </section>;
}
