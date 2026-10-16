import type { AiProviderConfig } from "@yukinal/shared";

import { resolveAgentModels } from "../agent/useAgentModels.js";
import { useWorkspaceStore, type WorkspaceState } from "../../stores/workspace-store.js";

export const AGENT_NIST_STORAGE_KEY = "yukinal.agent-nist.v1";
export const AGENT_TEAMS = [
  { id: "blue", label: "蓝队", description: "守护运行，分析异常", purpose: "围绕稳定性和可观测性建立可复核的运维判断。" },
  { id: "red", label: "红队", description: "审查风险，验证变更", purpose: "从风险和反例出发，检查方案与变更的边界。" },
  { id: "orange", label: "橙队", description: "协调故障，沉淀复盘", purpose: "整理处置进展与证据，让团队更容易协作和交接。" },
] as const;

export type AgentTeam = (typeof AGENT_TEAMS)[number]["id"];
export interface AgentRole {
  id: string;
  team: AgentTeam;
  name: string;
  responsibility: string;
  providerId: string | null;
  model: string | null;
}

const DEFAULT_ROLES: readonly AgentRole[] = [
  { id: "blue-guardian", team: "blue", name: "运维守护", responsibility: "检查服务器资源、服务和连接状态，优先识别影响稳定性的异常。先收集只读证据，区分已确认事实与待验证假设，再给出分级处置建议。", providerId: null, model: null },
  { id: "blue-logs", team: "blue", name: "日志分析", responsibility: "围绕故障时间段关联日志、错误与资源变化，定位异常链路。引用可复核的证据，列出可能原因及下一步验证方式，不把相关性直接当作根因。", providerId: null, model: null },
  { id: "red-review", team: "red", name: "风险审查", responsibility: "审查当前运维方案中的权限、配置、暴露面与恢复风险。只在用户授权的目标范围内进行检查，说明影响、证据与建议，不擅自扩大检查或执行改动。", providerId: null, model: null },
  { id: "red-validation", team: "red", name: "变更验证", responsibility: "检查拟议变更的前置条件、影响范围、验收标准和回滚路径。主动寻找反例与遗漏，区分变更前后证据；任何实际改动仍遵循当前权限与批准流程。", providerId: null, model: null },
  { id: "orange-incident", team: "orange", name: "故障协调", responsibility: "整理故障影响、时间线、已完成操作、待确认事项与下一步优先级。围绕已有证据协调排查和交接，明确需要用户决策的节点，不捏造执行结果。", providerId: null, model: null },
  { id: "orange-retrospective", team: "orange", name: "复盘整理", responsibility: "汇总故障和处置记录，整理事实时间线、已确认原因、恢复过程与遗留风险。将改进项写成可验证的行动建议，对证据不足的部分明确标注待确认。", providerId: null, model: null },
];

export function defaultAgentRoles(): AgentRole[] {
  return DEFAULT_ROLES.map((role) => ({ ...role }));
}

function record(value: unknown): Record<string, unknown> | null {
  return typeof value === "object" && value !== null && !Array.isArray(value) ? value as Record<string, unknown> : null;
}

function text(value: unknown, max: number): string | null {
  return typeof value === "string" && value.trim().length > 0 && value.length <= max ? value.trim() : null;
}

/** Persist only known local role fields; provider credentials remain in their existing store. */
export function parseAgentRoles(value: unknown): AgentRole[] {
  const saved = record(value);
  if (saved?.version !== 1 || !Array.isArray(saved.roles)) throw new Error("本机角色配置版本或格式无法识别。");
  const roles = saved.roles.map(record);
  return DEFAULT_ROLES.map((base) => {
    const role = roles.find((candidate) => candidate?.id === base.id);
    if (!role) return { ...base };
    const providerId = text(role.providerId, 512);
    return {
      ...base,
      name: text(role.name, 60) ?? base.name,
      responsibility: text(role.responsibility, 4000) ?? base.responsibility,
      providerId,
      model: providerId ? text(role.model, 512) : null,
    };
  });
}

export interface AgentRoleStorage {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
}

export function loadAgentRoles(storage: Pick<AgentRoleStorage, "getItem">, key = AGENT_NIST_STORAGE_KEY): { roles: AgentRole[]; notice: string | null } {
  try {
    const saved = storage.getItem(key);
    return { roles: saved === null ? defaultAgentRoles() : parseAgentRoles(JSON.parse(saved)), notice: null };
  } catch {
    return { roles: defaultAgentRoles(), notice: "无法读取本机角色配置，当前显示默认角色；原存储内容未被覆盖。" };
  }
}

export function saveAgentRoles(storage: Pick<AgentRoleStorage, "setItem">, roles: readonly AgentRole[], key = AGENT_NIST_STORAGE_KEY): AgentRole[] {
  const clean = parseAgentRoles({ version: 1, roles });
  storage.setItem(key, JSON.stringify({ version: 1, roles: clean }));
  return clean;
}

export function roleModelChoices(provider: AiProviderConfig | undefined) {
  // Editing a role may list any saved catalog, including a currently inactive
  // connection. This copy only resolves labels; roleBinding still checks the
  // real enabled flag before any draft handoff and never activates a provider.
  return provider ? resolveAgentModels([{ ...provider, enabled: true }], null, null).modelChoices : [];
}

export function roleBinding(role: AgentRole, providers: readonly AiProviderConfig[]) {
  const provider = providers.find((candidate) => candidate.id === role.providerId);
  const choice = roleModelChoices(provider).find((candidate) => candidate.model === role.model);
  const reason = !role.providerId ? "尚未绑定 AI 连接与模型。"
    : !provider ? "原来的 AI 连接已不可用，请重新绑定。"
      : !provider.enabled ? "绑定的 AI 连接当前未启用，请在设置中切换启用后再使用。"
        : !choice ? "绑定的模型已不在此连接的模型列表中，请重新选择。"
          : null;
  return { provider, choice, reason };
}

export function draftProtection(state: Pick<WorkspaceState, "agentDraft" | "agentAttachments" | "agentBusy" | "agentSessionArchived">): string | null {
  if (state.agentBusy) return "对话正在运行，结束后再使用角色。";
  if (state.agentSessionArchived) return "当前对话已归档，请先新建或恢复对话，再使用角色。";
  if (state.agentDraft.length > 0 && state.agentAttachments.length > 0) return "对话中已有草稿和附件，请先处理，角色不会覆盖它们。";
  if (state.agentDraft.length > 0) return "对话中已有草稿，请先发送或清空，角色不会覆盖它。";
  if (state.agentAttachments.length > 0) return "对话中已有附件，请先处理，角色不会覆盖它们。";
  return null;
}

export function rolePrompt(role: AgentRole): string {
  const team = AGENT_TEAMS.find((candidate) => candidate.id === role.team)?.label ?? "运维队伍";
  return `请以「${team} · ${role.name.trim()}」的角色协助本次运维。\n\n角色职责：\n${role.responsibility.trim()}\n\n先确认目标服务器与任务，基于证据开展工作。角色职责不改变当前运行模式、工具权限或批准要求。\n\n本次任务：\n`;
}

export type AgentRoleHandoff = { ok: true; draft: string; providerId: string; model: string } | { ok: false; reason: string };

/** One synchronous guard and store update prevent a role from replacing live composition. */
export function handoffAgentRole(role: AgentRole, providers: readonly AiProviderConfig[]): AgentRoleHandoff {
  const state = useWorkspaceStore.getState();
  const protection = draftProtection(state);
  if (protection) return { ok: false, reason: protection };
  const binding = roleBinding(role, providers);
  if (binding.reason || !binding.provider || !binding.choice) return { ok: false, reason: binding.reason ?? "请先绑定 AI 连接与模型。" };
  if (!role.name.trim() || !role.responsibility.trim()) return { ok: false, reason: "角色名称与职责不能为空。" };
  const draft = rolePrompt(role);
  useWorkspaceStore.setState({ agentDraft: draft, selectedProviderId: binding.provider.id, selectedModel: binding.choice.model, agentOpen: true, agentHistoryOpen: false });
  return { ok: true, draft, providerId: binding.provider.id, model: binding.choice.model };
}
