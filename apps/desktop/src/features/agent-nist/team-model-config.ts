import type { AiProviderConfig } from "@yukinal/shared";

import { useWorkspaceStore } from "../../stores/workspace-store.js";
import {
  AGENT_NIST_STORAGE_KEY,
  defaultAgentRoles,
  parseAgentRoles,
  roleBinding,
  saveAgentRoles,
  type AgentRole,
  type AgentRoleStorage,
} from "./agent-nist.js";

/** Public capability names are independent of saved user names and internal prompts. */
export const ROLE_CAPABILITY_LABELS: Readonly<Record<string, string>> = {
  "blue-guardian": "运行安全与响应", "blue-logs": "日志分析",
  "red-review": "授权攻击测试", "red-validation": "攻击路径验证",
  "purple-detection": "检测覆盖验证", "purple-retest": "防护复测",
  "white-scope": "范围与规则", "white-control": "风险与验收",
  "yellow-architecture": "系统设计", "yellow-engineering": "部署、开发与修复",
  "orange-incident": "漏洞协同修复", "orange-retrospective": "安全开发改进",
  "green-observability": "监测与告警建设", "green-response": "响应与取证设计",
};

export const TEAM_CAPABILITIES = [
  { id: "build", label: "建设与修复", description: "环境部署、系统设计、开发和漏洞修复。", preferredRoleId: "yellow-engineering", roleIds: ["yellow-architecture", "yellow-engineering", "orange-incident", "orange-retrospective"] },
  { id: "operate", label: "运行与响应", description: "运行检查、日志分析、监测告警与事件响应。", preferredRoleId: "blue-guardian", roleIds: ["blue-guardian", "blue-logs", "green-observability", "green-response"] },
  { id: "validate", label: "验证与复测", description: "在授权范围内验证攻击路径、防护效果和检测覆盖。", preferredRoleId: "purple-retest", roleIds: ["red-review", "red-validation", "purple-detection", "purple-retest"] },
  { id: "control", label: "范围与风险", description: "明确操作边界、停止条件、验收依据和待确认事项。", preferredRoleId: "white-scope", roleIds: ["white-scope", "white-control"] },
] as const;

export type TeamModelBinding = Pick<AgentRole, "providerId" | "model">;
export interface ConversationModel {
  providerId: string | null;
  model: string | null;
}

export function capabilityLabel(roleId: string): string {
  return ROLE_CAPABILITY_LABELS[roleId] ?? "原能力不可用";
}

/** null means mixed bindings; it never silently becomes an inheritance choice. */
export function commonModelBinding(roles: readonly AgentRole[]): TeamModelBinding | null {
  const first = roles[0];
  return first && roles.every((role) => role.providerId === first.providerId && role.model === first.model)
    ? { providerId: first.providerId, model: first.model } : null;
}

export function teamModelBinding(binding: TeamModelBinding, providers: readonly AiProviderConfig[], conversation: ConversationModel) {
  const inherited = binding.providerId === null;
  const providerId = inherited ? conversation.providerId : binding.providerId;
  const provider = providers.find((candidate) => candidate.id === providerId);
  const model = inherited ? conversation.model ?? provider?.model ?? null : binding.model;
  const resolved = roleBinding({ id: "binding", team: "blue", name: "", responsibility: "", providerId, model }, providers);
  // AgentPanel sends selectedModel ?? selectedProvider.model verbatim. A saved
  // provider catalog does not restrict that foreground conversation selection;
  // apply its stricter membership check only to independent role bindings.
  const inheritedReason = !providerId ? "当前对话尚未选择 AI 连接。"
    : !provider ? "当前对话的 AI 连接已不可用，请重新选择。"
      : !provider.enabled ? "当前对话的 AI 连接未启用，请在设置中切换启用。"
        : !model?.trim() ? "当前对话尚未选择模型。" : null;
  const reason = inherited ? inheritedReason : resolved.reason;
  const modelLabel = resolved.choice?.label ?? model;
  const label = reason ? `${inherited ? "继承对话" : "独立绑定"} · 不可用`
    : `${inherited ? "继承对话" : "独立绑定"} · ${provider!.label} · ${modelLabel}`;
  return { ...resolved, inherited, reason, label };
}

function assertModelSaveIdle() {
  if (useWorkspaceStore.getState().agentBusy) throw new Error("对话正在运行，结束后再保存模型配置。");
}

/** Update only model fields, merging the latest storage without rewriting legacy prompts/names. */
export function saveTeamModelBinding(
  storage: AgentRoleStorage,
  target: "all" | string,
  binding: TeamModelBinding,
  providers: readonly AiProviderConfig[],
  key = AGENT_NIST_STORAGE_KEY,
): AgentRole[] {
  assertModelSaveIdle();
  if (binding.providerId !== null) {
    const reason = teamModelBinding(binding, providers, { providerId: null, model: null }).reason;
    if (reason) throw new Error(reason);
  } else if (binding.model !== null) {
    throw new Error("继承对话时不能单独指定模型。");
  }
  const saved = storage.getItem(key);
  const envelope: unknown = saved === null ? null : JSON.parse(saved);
  const roles = saved === null ? defaultAgentRoles() : parseAgentRoles(envelope);
  if (target !== "all" && !roles.some((role) => role.id === target)) throw new Error("原能力不可用，请重新选择。");
  const selected = new Set(roles.filter((role) => target === "all" || role.id === target).map((role) => role.id));
  const changes = { providerId: binding.providerId, model: binding.providerId ? binding.model : null };
  if (saved === null) {
    assertModelSaveIdle();
    return saveAgentRoles(storage, roles.map((role) => selected.has(role.id) ? { ...role, ...changes } : role), key);
  }
  // parseAgentRoles validated this envelope. Retain its catalog version and all
  // existing role data verbatim; only the selected providerId/model fields change.
  const original = envelope as { roles: Array<Record<string, unknown>> };
  const present = new Set<string>();
  const entries = original.roles.map((entry) => {
    if (entry && typeof entry.id === "string" && selected.has(entry.id)) {
      present.add(entry.id);
      return { ...entry, ...changes };
    }
    return entry;
  });
  for (const role of roles) {
    if (selected.has(role.id) && !present.has(role.id)) entries.push({ ...role, ...changes });
  }
  const updated = { ...original, roles: entries };
  const result = parseAgentRoles(updated);
  assertModelSaveIdle();
  storage.setItem(key, JSON.stringify(updated));
  if (typeof window !== "undefined") window.dispatchEvent(new Event("agent-role-config-changed"));
  return result;
}
