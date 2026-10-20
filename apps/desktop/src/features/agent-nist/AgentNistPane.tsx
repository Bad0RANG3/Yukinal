import { useEffect, useId, useState } from "react";

import { Icon } from "../../components/Icon.js";
import { errorMessage } from "../../lib/format.js";
import { useProviders } from "../../lib/providers.js";
import { isPreviewFixture } from "../../lib/preview-fixtures.js";
import { useServers } from "../../lib/servers.js";
import { useWorkspaceStore } from "../../stores/workspace-store.js";
import { MemoryLibrary } from "../agent/MemoryLibrary.js";
import { TeamWatchPane } from "./TeamWatchPane.js";
import { TeamFeedbackPreview } from "./TeamActivity.js";
import { TEAM_WORK } from "./team-operations.js";
import { useTeamRuntimeStore, TEAM_STAGE_LABEL, teamRunActive } from "./team-runtime-store.js";
import {
  AGENT_NIST_STORAGE_KEY, currentAgentRoleId, defaultAgentRoles, draftProtection, handoffAgentRole,
  loadAgentRoles, roleModelChoices, type AgentRole,
} from "./agent-nist.js";
import {
  TEAM_CAPABILITIES, capabilityLabel, commonModelBinding, saveTeamModelBinding,
  teamModelBinding, type TeamModelBinding,
} from "./team-model-config.js";

function readLocalRoles(key: string) {
  try {
    return loadAgentRoles(window.localStorage, key);
  } catch {
    return { roles: defaultAgentRoles(), notice: "本机存储暂不可用，暂时无法保存模型配置。" };
  }
}

export function AgentNistPane({ onUseAgent }: { onUseAgent?: () => void }) {
  const providers = useProviders();
  const servers = useServers({ enabled: false });
  const runs = useTeamRuntimeStore((state) => state.runs);
  const watchUpdates = useTeamRuntimeStore((state) => state.watchUpdates);
  const view = useTeamRuntimeStore((state) => state.panelView);
  const setView = useTeamRuntimeStore((state) => state.setPanelView);
  const selectedRoleId = useWorkspaceStore((state) => state.agentRoleId);
  const agentDraft = useWorkspaceStore((state) => state.agentDraft);
  const agentAttachments = useWorkspaceStore((state) => state.agentAttachments);
  const agentBusy = useWorkspaceStore((state) => state.agentBusy);
  const agentSessionArchived = useWorkspaceStore((state) => state.agentSessionArchived);
  const selectedServerId = useWorkspaceStore((state) => state.selectedServerId);
  const providerId = useWorkspaceStore((state) => state.selectedProviderId);
  const model = useWorkspaceStore((state) => state.selectedModel);
  const setPrimary = useWorkspaceStore((state) => state.setPrimary);
  const [memoryOpen, setMemoryOpen] = useState(false);
  const [storageKey] = useState(() => `${AGENT_NIST_STORAGE_KEY}${isPreviewFixture() ? ".preview" : ""}`);
  const [config, setConfig] = useState(() => readLocalRoles(storageKey));
  const [expandedCapability, setExpandedCapability] = useState<string | null>("build");
  const [modelTarget, setModelTarget] = useState("all");
  const [modelDraft, setModelDraft] = useState<TeamModelBinding | null>(() => commonModelBinding(config.roles));
  const [notice, setNotice] = useState<{ text: string; error: boolean } | null>(null);
  const instanceId = useId();
  const availableProviders = providers.data ?? [];
  const conversation = { providerId, model };
  const protection = draftProtection({ agentDraft, agentAttachments, agentBusy, agentSessionArchived });
  const queryBlock = providers.isLoading ? "正在读取 AI 连接。" : providers.isError ? "请先重试读取 AI 连接。" : null;
  const activeRuns = [...runs].reverse().filter((run) => teamRunActive(run) && (!selectedServerId || run.serverId === selectedServerId));
  const selectedRole = config.roles.find((role) => role.id === currentAgentRoleId(selectedRoleId ?? ""));
  const coordinatorModel = teamModelBinding({ providerId: null, model: null }, availableProviders, conversation);
  const common = commonModelBinding(config.roles);
  const draftBinding = modelDraft && teamModelBinding(modelDraft, availableProviders, conversation);
  const draftProvider = availableProviders.find((provider) => provider.id === modelDraft?.providerId);
  const choices = roleModelChoices(draftProvider);
  const fieldsBlocked = Boolean(agentBusy || queryBlock || config.notice);
  const invalidDraft = Boolean(modelDraft?.providerId && draftBinding?.reason);

  useEffect(() => {
    const refresh = () => setConfig(readLocalRoles(storageKey));
    window.addEventListener("agent-role-config-changed", refresh);
    window.addEventListener("storage", refresh);
    return () => {
      window.removeEventListener("agent-role-config-changed", refresh);
      window.removeEventListener("storage", refresh);
    };
  }, [storageKey]);

  function selectRole(role: AgentRole, openConversation = false) {
    if (queryBlock) { setNotice({ text: queryBlock, error: true }); return; }
    const result = handoffAgentRole(role, availableProviders);
    if (!result.ok) { setNotice({ text: result.reason, error: true }); return; }
    setNotice({ text: `优先能力已设为「${capabilityLabel(role.id)}」，下次对话生效。`, error: false });
    if (openConversation) onUseAgent?.();
  }

  function restoreAutomatic() {
    const state = useWorkspaceStore.getState();
    const blocked = draftProtection(state);
    if (blocked) { setNotice({ text: blocked, error: true }); return; }
    state.setAgentRoleId(null);
    setNotice({ text: "已恢复由协调助手按目标自动分工。", error: false });
  }

  function chooseModelTarget(target: string) {
    setModelTarget(target);
    const role = config.roles.find((item) => item.id === target);
    setModelDraft(target === "all" ? commonModelBinding(config.roles) : role ? { providerId: role.providerId, model: role.model } : null);
  }

  function saveModels() {
    if (!modelDraft || queryBlock || config.notice) return;
    try {
      const roles = saveTeamModelBinding(window.localStorage, modelTarget, modelDraft, availableProviders, storageKey);
      setConfig({ roles, notice: null });
      setNotice({ text: `已保存${modelTarget === "all" ? "全部能力" : `「${capabilityLabel(modelTarget)}」`}的模型配置，下次对话生效。`, error: false });
    } catch (error) {
      setNotice({ text: `未保存模型配置：${errorMessage(error)}`, error: true });
    }
  }

  return <section className="agent-nist-page" aria-labelledby={`${instanceId}-title`}>
    <header className="agent-nist-heading">
      <div><h2 id={`${instanceId}-title`}>团队</h2></div>
      <button type="button" className="agent-nist-text-action" onClick={() => setMemoryOpen(true)}><Icon name="archive" size="sm" />记忆库</button>
    </header>
    <div className="team-workbench-tabs" role="group" aria-label="团队工作台视图">{([{ id: "teams", label: "能力" }, { id: "watch", label: "值守" }, { id: "config", label: "配置" }] as const).map((tab) => <button type="button" key={tab.id} aria-pressed={view === tab.id} onClick={() => setView(tab.id)}>{tab.label}{tab.id === "watch" && watchUpdates.some((update) => !selectedServerId || update.serverId === selectedServerId) ? <span className="team-watch-unread" aria-label="有最新状态" title="值守有最新状态">新</span> : null}</button>)}</div>
    <div className="team-current-scope"><Icon name={selectedServerId ? "servers" : "projects"} size="xs" /><span title={selectedServerId ? servers.data?.find((server) => server.id === selectedServerId)?.name : undefined}>{selectedServerId ? servers.data?.find((server) => server.id === selectedServerId)?.name ?? selectedServerId : "全局工作区"}</span></div>
    {config.notice ? <p className="agent-nist-notice is-error" role="alert">{config.notice}</p> : null}
    {providers.isLoading ? <p className="agent-nist-notice" role="status">正在读取 AI 连接…</p> : null}
    {providers.isError ? <div className="agent-nist-notice is-error" role="alert"><p>无法读取 AI 连接：{errorMessage(providers.error)}</p><button type="button" className="agent-nist-text-action" disabled={providers.isFetching} onClick={() => void providers.refetch()}>重新读取</button></div> : null}
    {protection ? <p className="agent-nist-notice" role="status">{protection}</p> : null}
    {notice ? <p className={`agent-nist-notice${notice.error ? " is-error" : ""}`} role={notice.error ? "alert" : "status"}>{notice.text}</p> : null}

    {view === "teams" ? <>
      <div className="team-current-mission"><span>当前协作</span>{activeRuns.length ? activeRuns.map((run) => <div key={run.id}><strong>{TEAM_WORK[run.team].job} · {TEAM_STAGE_LABEL[run.stage]}</strong><p>{run.goal}</p></div>) : <><strong>提出目标，按需组合能力</strong><p>协调助手按任务需要分工，也可为下次对话指定优先能力。</p></>}</div>
      <ul className="team-capability-list">{TEAM_CAPABILITIES.map((capability) => <li key={capability.id}>
        <button type="button" className="team-capability-toggle" aria-expanded={expandedCapability === capability.id} aria-controls={`${instanceId}-${capability.id}`} onClick={() => setExpandedCapability((current) => current === capability.id ? null : capability.id)}><span><strong>{capability.label}</strong><small>{capability.description}</small></span><Icon name={expandedCapability === capability.id ? "chevronDown" : "chevronRight"} size="xs" /></button>
        {expandedCapability === capability.id ? <div className="team-capability-detail" id={`${instanceId}-${capability.id}`}><p>{capability.roleIds.map(capabilityLabel).join("、")}</p><div><button type="button" className="agent-nist-text-action" disabled={Boolean(protection || queryBlock)} onClick={() => { const role = config.roles.find((item) => item.id === capability.preferredRoleId); if (role) selectRole(role, true); }}>在对话中优先使用<Icon name="chevronRight" size="xs" /></button><button type="button" className="agent-nist-text-action" onClick={() => setView("config")}>路由与模型配置</button></div></div> : null}
      </li>)}</ul>
      {selectedRoleId ? <button type="button" className="agent-nist-text-action" disabled={Boolean(protection)} onClick={restoreAutomatic}>恢复自动分工</button> : null}
      {isPreviewFixture() ? <TeamFeedbackPreview serverId={selectedServerId} /> : null}
    </> : null}
    {view === "watch" ? <TeamWatchPane /> : null}
    {view === "config" ? <>
      <section className="team-config-section" aria-labelledby={`${instanceId}-routing-title`}>
        <header><h3 id={`${instanceId}-routing-title`}>MoE 路由配置</h3><span>应用层任务路由</span></header>
        <p>自动模式由协调助手按目标组合能力。手选设置优先能力；对话中明确 @ 指定的能力优先于手选。当前配置不调整模型内部专家网络。</p>
        <label className="form-field"><span>路由方式</span><select className="form-select" aria-label="路由方式" value={selectedRoleId ?? ""} disabled={Boolean(protection)} onChange={(event) => { const role = config.roles.find((item) => item.id === event.target.value); if (role) selectRole(role); else if (!event.target.value) restoreAutomatic(); }}>
          <option value="">自动分工（协调助手）</option>
          {selectedRoleId && !selectedRole ? <option value={selectedRoleId} disabled>原优先能力已不可用</option> : null}
          {config.roles.map((role) => <option key={role.id} value={role.id}>{capabilityLabel(role.id)}</option>)}
        </select></label>
        <div className="team-config-current"><span>{selectedRoleId ? selectedRole ? `当前优先：${capabilityLabel(selectedRole.id)}` : "原优先能力已失效，请恢复自动分工。" : "当前由协调助手自动分工"}</span>{selectedRoleId ? <button type="button" className="agent-nist-text-action" disabled={Boolean(protection)} onClick={restoreAutomatic}>恢复自动分工</button> : null}</div>
      </section>

      <section className="team-config-section" aria-labelledby={`${instanceId}-models-title`}>
        <header><h3 id={`${instanceId}-models-title`}>团队模型配置</h3></header>
        <p>协调助手使用当前对话模型。能力可继承对话模型，也可绑定已有的 AI 连接与模型；保存只影响后续对话。</p>
        <div className="team-model-summary"><span>当前对话</span><strong>{queryBlock ?? coordinatorModel.label}</strong>{!queryBlock && coordinatorModel.reason ? <small className="is-error">{coordinatorModel.reason}</small> : null}</div>
        <div className="team-model-summary"><span>已保存的分配</span><strong>{queryBlock ?? (common ? teamModelBinding(common, availableProviders, conversation).label : "各项能力分别配置，已有绑定保持不变")}</strong></div>
        <form className="team-model-form" onSubmit={(event) => { event.preventDefault(); saveModels(); }}>
          <label className="form-field"><span>适用能力</span><select className="form-select" aria-label="适用能力" value={modelTarget} disabled={fieldsBlocked} onChange={(event) => chooseModelTarget(event.target.value)}><option value="all">全部能力</option>{config.roles.map((role) => <option key={role.id} value={role.id}>{capabilityLabel(role.id)}</option>)}</select></label>
          <label className="form-field"><span>AI 连接</span><select className="form-select" aria-label="团队 AI 连接" value={modelDraft === null ? "__mixed" : modelDraft.providerId ?? ""} disabled={fieldsBlocked} onChange={(event) => {
            const provider = availableProviders.find((item) => item.id === event.target.value);
            const models = roleModelChoices(provider);
            setModelDraft(provider ? { providerId: provider.id, model: models.find((item) => item.model === provider.model)?.model ?? models[0]?.model ?? null } : { providerId: null, model: null });
          }}><option value="">继承当前对话模型</option>{modelDraft === null ? <option value="__mixed" disabled>当前为不同绑定，请明确选择</option> : null}{modelDraft?.providerId && !draftProvider ? <option value={modelDraft.providerId} disabled>原连接已不可用</option> : null}{availableProviders.map((provider) => <option key={provider.id} value={provider.id} disabled={!provider.enabled}>{provider.label}{provider.enabled ? "" : "（未启用）"}</option>)}</select></label>
          {modelDraft?.providerId ? <label className="form-field"><span>模型</span><select className="form-select" aria-label="团队模型" value={modelDraft.model ?? ""} disabled={fieldsBlocked || !choices.length} onChange={(event) => setModelDraft({ ...modelDraft, model: event.target.value || null })}><option value="">选择模型</option>{modelDraft.model && !choices.some((choice) => choice.model === modelDraft.model) ? <option value={modelDraft.model} disabled>{modelDraft.model}（不可用）</option> : null}{choices.map((choice) => <option key={choice.key} value={choice.model}>{choice.label}</option>)}</select></label> : null}
          {draftBinding ? <p className={`team-config-hint${draftBinding.reason ? " is-error" : ""}`} role={draftBinding.reason ? "status" : undefined}>{draftBinding.reason ? `${draftBinding.label}：${draftBinding.reason}` : draftBinding.label}</p> : <p className="team-config-hint">选择连接后才会更新已有绑定。</p>}
          <div className="team-config-actions"><button type="submit" className="button-secondary" disabled={fieldsBlocked || !modelDraft || invalidDraft}>{modelTarget === "all" ? "应用到全部能力" : "保存模型配置"}</button><button type="button" className="agent-nist-text-action" onClick={() => setPrimary("settings")}>AI 连接设置<Icon name="chevronRight" size="xs" /></button></div>
          {agentBusy ? <p className="team-config-hint">对话正在运行，结束后再保存模型配置。</p> : null}
        </form>
        <details className="team-model-bindings"><summary>查看各项能力的模型（{config.roles.length}）</summary><ul>{config.roles.map((role) => { const binding = teamModelBinding(role, availableProviders, conversation); return <li key={role.id}><div><strong>{capabilityLabel(role.id)}</strong><span className={binding.reason ? "is-error" : undefined}>{queryBlock ?? binding.label}</span>{!queryBlock && binding.reason ? <small>{binding.reason}</small> : null}</div><button type="button" className="agent-nist-text-action" aria-label={`配置${capabilityLabel(role.id)}的模型`} disabled={fieldsBlocked} onClick={() => chooseModelTarget(role.id)}>配置</button></li>; })}</ul></details>
      </section>
    </> : null}
    {memoryOpen ? <MemoryLibrary key={selectedServerId ?? "global"} onClose={() => setMemoryOpen(false)} /> : null}
  </section>;
}
