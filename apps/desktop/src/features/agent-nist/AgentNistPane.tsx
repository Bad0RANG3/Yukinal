import { useId, useState } from "react";

import { Icon } from "../../components/Icon.js";
import { errorMessage } from "../../lib/format.js";
import { useProviders } from "../../lib/providers.js";
import { isPreviewFixture } from "../../lib/preview-fixtures.js";
import { useWorkspaceStore } from "../../stores/workspace-store.js";
import {
  AGENT_NIST_STORAGE_KEY,
  AGENT_TEAMS,
  defaultAgentRoles,
  draftProtection,
  handoffAgentRole,
  loadAgentRoles,
  roleBinding,
  roleModelChoices,
  saveAgentRoles,
  type AgentRole,
  type AgentTeam,
} from "./agent-nist.js";

function readLocalRoles(key: string) {
  try {
    return loadAgentRoles(window.localStorage, key);
  } catch {
    return { roles: defaultAgentRoles(), notice: "本机存储暂不可用，修改角色时将提示保存结果。" };
  }
}

export function AgentNistPane({ onUseAgent }: { onUseAgent?: () => void }) {
  const providers = useProviders();
  const agentDraft = useWorkspaceStore((state) => state.agentDraft);
  const agentAttachments = useWorkspaceStore((state) => state.agentAttachments);
  const agentBusy = useWorkspaceStore((state) => state.agentBusy);
  const agentSessionArchived = useWorkspaceStore((state) => state.agentSessionArchived);
  const setPrimary = useWorkspaceStore((state) => state.setPrimary);
  const [storageKey] = useState(() => `${AGENT_NIST_STORAGE_KEY}${isPreviewFixture() ? ".preview" : ""}`);
  const [config, setConfig] = useState(() => readLocalRoles(storageKey));
  const [teamId, setTeamId] = useState<AgentTeam>("blue");
  const [editors, setEditors] = useState<ReadonlySet<string>>(() => new Set());
  const [drafts, setDrafts] = useState<Record<string, AgentRole>>({});
  const [notice, setNotice] = useState<{ text: string; error: boolean } | null>(null);
  const instanceId = useId();
  const protectionId = `${instanceId}-composition-state`;
  const queryStateId = `${instanceId}-connection-state`;
  const team = AGENT_TEAMS.find((candidate) => candidate.id === teamId) ?? AGENT_TEAMS[0];
  const availableProviders = providers.data ?? [];
  const protection = draftProtection({ agentDraft, agentAttachments, agentBusy, agentSessionArchived });
  const queryBlock = providers.isLoading ? "正在读取 AI 连接。" : providers.isError ? "请先重试读取 AI 连接。" : null;

  function toggleEditor(role: AgentRole) {
    setDrafts((current) => ({ ...current, [role.id]: current[role.id] ?? { ...role } }));
    setEditors((current) => {
      const next = new Set(current);
      if (next.has(role.id)) next.delete(role.id);
      else next.add(role.id);
      return next;
    });
  }

  function closeEditor(roleId: string) {
    setEditors((current) => new Set([...current].filter((id) => id !== roleId)));
    setDrafts((current) => {
      const next = { ...current };
      delete next[roleId];
      return next;
    });
  }

  function saveRole(role: AgentRole) {
    if (!role.name.trim() || !role.responsibility.trim()) {
      setNotice({ text: "请填写角色名称和职责。", error: true });
      return;
    }
    try {
      const roles = saveAgentRoles(window.localStorage, config.roles.map((current) => current.id === role.id ? role : current), storageKey);
      setConfig({ roles, notice: null });
      closeEditor(role.id);
      setNotice({ text: `「${role.name.trim()}」已保存为本机配置。`, error: false });
    } catch {
      setNotice({ text: "无法保存本机配置，编辑内容仍保留。请检查本机存储后重试。", error: true });
    }
  }

  function useRole(role: AgentRole) {
    if (queryBlock) {
      setNotice({ text: queryBlock, error: true });
      return;
    }
    const result = handoffAgentRole(role, availableProviders);
    if (!result.ok) {
      setNotice({ text: result.reason, error: true });
      return;
    }
    setNotice({ text: `已选择「${role.name}」。`, error: false });
    onUseAgent?.();
  }

  return <section className="agent-nist-page" aria-labelledby={`${instanceId}-title`}>
    <header className="agent-nist-heading">
      <div><h2 id={`${instanceId}-title`}>Agent NIST</h2><span>本机配置</span></div>
    </header>

    <div className="agent-nist-teams" role="group" aria-label="运维 Agent 队伍">
      {AGENT_TEAMS.map((candidate) => <button key={candidate.id} type="button" aria-pressed={candidate.id === teamId} className={candidate.id === teamId ? "is-selected" : ""} onClick={() => setTeamId(candidate.id)}>{candidate.label}</button>)}
    </div>
    <div className="agent-nist-team-purpose"><strong>{team.englishName} · {team.description}</strong><p>{team.purpose}</p></div>

    {config.notice ? <p className="agent-nist-notice" role="status">{config.notice}</p> : null}
    {providers.isLoading ? <p className="agent-nist-notice" id={queryStateId} role="status">正在读取 AI 连接…</p> : null}
    {providers.isError ? <div className="agent-nist-notice is-error" id={queryStateId} role="alert"><p>无法读取 AI 连接：{errorMessage(providers.error)}</p><button type="button" className="agent-nist-text-action" disabled={providers.isFetching} onClick={() => void providers.refetch()}>重新读取</button></div> : null}
    {protection ? <p className="agent-nist-notice" id={protectionId} role="status">{protection}</p> : null}
    {notice ? <p className={`agent-nist-notice${notice.error ? " is-error" : ""}`} role={notice.error ? "alert" : "status"}>{notice.text}</p> : null}

    <div className="agent-nist-roles">
      {config.roles.filter((role) => role.team === teamId).map((role) => {
        const binding = roleBinding(role, availableProviders);
        const editing = editors.has(role.id);
        const draft = drafts[role.id] ?? role;
        const editorId = `${instanceId}-${role.id}-editor`;
        const reasonId = `${instanceId}-${role.id}-reason`;
        const bindingReason = role.providerId ? binding.reason : null;
        const reason = protection ?? queryBlock ?? bindingReason;
        const localReason = protection || queryBlock ? null : bindingReason;
        const describedBy = protection ? protectionId : queryBlock ? queryStateId : localReason ? reasonId : undefined;
        const draftProvider = availableProviders.find((candidate) => candidate.id === draft.providerId);
        const choices = roleModelChoices(draftProvider);
        const validDraftModel = choices.some((choice) => choice.model === draft.model);
        return <article className="agent-nist-role" key={role.id}>
          <div className="agent-nist-role-heading"><h3>{role.name}</h3><button type="button" className="agent-nist-text-action" aria-expanded={editing} aria-controls={editorId} onClick={() => toggleEditor(role)}><Icon name={editing ? "chevronUp" : "edit"} size="xs" />{editing ? "收起" : "编辑"}</button></div>
          <p className="agent-nist-responsibility">{role.responsibility}</p>
          <dl className="agent-nist-binding">
            <div><dt>连接</dt><dd>{binding.provider?.label ?? (role.providerId ? "原连接不可用" : "未绑定")}</dd></div>
            <div><dt>模型</dt><dd title={role.model ?? undefined}>{binding.choice?.label ?? role.model ?? "跟随对话"}</dd></div>
          </dl>
          {localReason ? <p className="agent-nist-role-reason" id={reasonId}>{localReason}</p> : null}
          <div className="agent-nist-role-actions">
            <button type="button" className="button-secondary" disabled={Boolean(reason) || editing} aria-describedby={describedBy} onClick={() => useRole(role)}><Icon name="agent" size="sm" />在对话中使用</button>
            {!providers.isLoading && !providers.isError && binding.reason ? <button type="button" className="agent-nist-text-action" onClick={() => setPrimary("settings")}>AI 连接设置<Icon name="chevronRight" size="xs" /></button> : null}
          </div>

          {editing ? <form id={editorId} className="agent-nist-editor" onSubmit={(event) => { event.preventDefault(); saveRole(draft); }}>
            <label className="form-field"><span>角色名称</span><input className="form-input" aria-label="角色名称" required maxLength={60} value={draft.name} onChange={(event) => setDrafts((current) => ({ ...current, [role.id]: { ...draft, name: event.target.value } }))} /></label>
            <label className="form-field"><span>角色职责</span><textarea className="form-input" aria-label="角色职责" required maxLength={4000} rows={5} value={draft.responsibility} onChange={(event) => setDrafts((current) => ({ ...current, [role.id]: { ...draft, responsibility: event.target.value } }))} /></label>
            <label className="form-field"><span>AI 连接</span><select className="form-select" aria-label="AI 连接" value={draft.providerId ?? ""} disabled={Boolean(queryBlock)} onChange={(event) => {
              const provider = availableProviders.find((candidate) => candidate.id === event.target.value);
              const models = roleModelChoices(provider);
              const model = models.find((candidate) => candidate.model === provider?.model)?.model ?? models[0]?.model ?? null;
              setDrafts((current) => ({ ...current, [role.id]: { ...draft, providerId: provider?.id ?? null, model } }));
            }}>
              <option value="">选择已有连接</option>
              {draft.providerId && !draftProvider ? <option value={draft.providerId} disabled>原连接已不可用</option> : null}
              {availableProviders.map((provider) => <option key={provider.id} value={provider.id}>{provider.label}{provider.enabled ? "" : "（当前未启用）"}</option>)}
            </select></label>
            <label className="form-field"><span>模型</span><select className="form-select" aria-label="模型" value={draft.model ?? ""} disabled={!choices.length || Boolean(queryBlock)} onChange={(event) => setDrafts((current) => ({ ...current, [role.id]: { ...draft, model: event.target.value || null } }))}>
              <option value="">选择模型</option>
              {draft.model && !validDraftModel ? <option value={draft.model} disabled>{draft.model}（不可用）</option> : null}
              {choices.map((choice) => <option key={choice.key} value={choice.model}>{choice.label}</option>)}
            </select></label>
            {draftProvider && !draftProvider.enabled ? <p className="agent-nist-role-reason">可保存此连接的角色分配；使用前需在 AI 连接设置中切换启用。</p> : null}
            {availableProviders.length === 0 && !queryBlock ? <p className="agent-nist-role-reason">请先在 AI 连接设置中添加连接。</p> : null}
            <div className="agent-nist-editor-actions"><button type="submit" className="button-secondary">保存角色</button><button type="button" className="agent-nist-text-action" onClick={() => closeEditor(role.id)}>取消</button></div>
          </form> : null}
        </article>;
      })}
    </div>
  </section>;
}
