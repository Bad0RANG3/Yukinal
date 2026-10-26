import { useId, useState } from "react";

import { Icon } from "../../components/Icon.js";
import { errorMessage } from "../../lib/format.js";
import { useServers } from "../../lib/servers.js";
import { useWorkspaceStore } from "../../stores/workspace-store.js";
import { useAgentModels } from "../agent/useAgentModels.js";
import { MemoryLibrary } from "../agent/MemoryLibrary.js";
import { TeamWatchPane } from "./TeamWatchPane.js";
import { useTeamRuntimeStore, TEAM_STAGE_LABEL, teamRunActive } from "./team-runtime-store.js";

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
  const activeView = view === "teams" ? "agent" : view === "config" ? "model" : view;
  const activeRuns = [...runs].reverse().filter((run) => teamRunActive(run) && (!selectedServerId || run.serverId === selectedServerId));
  const modelError = models.providers.isError ? `无法读取 AI 连接：${errorMessage(models.providers.error)}` : null;

  return <section className="agent-nist-page" aria-labelledby={`${instanceId}-title`}>
    <header className="agent-nist-heading">
      <div><h2 id={`${instanceId}-title`}>运维 Agent</h2></div>
      <button type="button" className="agent-nist-text-action" onClick={() => setMemoryOpen(true)}><Icon name="archive" size="sm" />记忆库</button>
    </header>
    <div className="team-workbench-tabs" role="group" aria-label="运维 Agent 页面">
      {([{ id: "agent", label: "Agent" }, { id: "watch", label: "值守" }, { id: "model", label: "模型" }] as const).map((tab) => <button type="button" key={tab.id} aria-pressed={activeView === tab.id} onClick={() => setView(tab.id)}>
        {tab.label}{tab.id === "watch" && watchUpdates.some((update) => !selectedServerId || update.serverId === selectedServerId) ? <span className="team-watch-unread" aria-label="有最新状态" title="值守有最新状态">新</span> : null}
      </button>)}
    </div>
    <div className="team-current-scope"><Icon name={selectedServerId ? "servers" : "projects"} size="xs" /><span title={selectedServerId ? servers.data?.find((server) => server.id === selectedServerId)?.name : undefined}>{selectedServerId ? servers.data?.find((server) => server.id === selectedServerId)?.name ?? selectedServerId : "全局工作区"}</span></div>
    {models.providers.isLoading ? <p className="agent-nist-notice" role="status">正在读取 AI 连接…</p> : null}
    {modelError ? <div className="agent-nist-notice is-error" role="alert"><p>{modelError}</p><button type="button" className="agent-nist-text-action" disabled={models.providers.isFetching} onClick={() => void models.providers.refetch()}>重新读取</button></div> : null}

    {activeView === "agent" ? <>
      <div className="team-current-mission"><span>统一处理</span>
        {activeRuns.length ? activeRuns.map((run) => <div key={run.id}><strong>运维 Agent · {TEAM_STAGE_LABEL[run.stage]}</strong><p>{run.goal}</p></div>) : <><strong>一个 Agent 负责完整运维流程</strong><p>同一个运维 AI 负责检查、排障、部署、修复与验证，必要时并行启动通用 subagent。</p></>}
      </div>
      <div className="team-model-summary"><span>当前模型</span><strong>{models.selectedModelLabel ?? "尚未配置可用模型"}</strong></div>
      <div className="team-config-actions">
        <button type="button" className="button-primary" disabled={!models.providerReady} onClick={() => onUseAgent?.()}>打开运维对话</button>
        <button type="button" className="agent-nist-text-action" onClick={() => setView("model")}>选择模型<Icon name="chevronRight" size="xs" /></button>
      </div>
    </> : null}

    {activeView === "watch" ? <TeamWatchPane /> : null}

    {activeView === "model" ? <section className="team-config-section" aria-labelledby={`${instanceId}-model-title`}>
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
