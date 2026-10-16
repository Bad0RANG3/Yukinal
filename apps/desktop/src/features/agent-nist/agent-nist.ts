import type { AiProviderConfig } from "@yukinal/shared";

import { resolveAgentModels } from "../agent/useAgentModels.js";
import { useWorkspaceStore, type WorkspaceState } from "../../stores/workspace-store.js";

export const AGENT_NIST_STORAGE_KEY = "yukinal.agent-nist.v1";
const ROLE_CATALOG_VERSION = 2;
export const AGENT_TEAMS = [
  { id: "blue", label: "蓝队", englishName: "Blue Team", description: "防守与响应", purpose: "系统加固、日志监测、威胁狩猎与入侵事件处置，维护日常运行安全。" },
  { id: "red", label: "红队", englishName: "Red Team", description: "授权攻击测试", purpose: "在授权范围内模拟攻击者，寻找可利用的薄弱点，验证防线与攻击目标。" },
  { id: "purple", label: "紫队", englishName: "Purple Team", description: "红蓝协作", purpose: "把红队方法转化为蓝队的检测与防护能力，再复测效果；可作为协作方式参与任务。" },
  { id: "white", label: "白队", englishName: "White Team", description: "组织、裁判与风险控制", purpose: "确定范围、规则、评分和停止条件，控制影响并监督越界；角色不替代用户授权。" },
  { id: "yellow", label: "黄队", englishName: "Yellow Team", description: "建设者", purpose: "由开发者、软件工程师与架构师承担系统设计、编码实现和问题修复。" },
  { id: "orange", label: "橙队", englishName: "Orange Team", description: "红队与建设者协作", purpose: "攻击人员与开发者协作，把漏洞成因和攻击经验转化为更安全的代码与开发习惯。" },
  { id: "green", label: "绿队", englishName: "Green Team", description: "蓝队与建设者协作", purpose: "防守人员与开发者协作，完善日志和安全设计，让产品更易监控、检测、响应与取证。" },
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

// Match untouched fields from the first catalog without replacing custom duties
// or model bindings. The original role IDs remain stable for stored assignments.
const LEGACY_ROLE_DEFAULTS = [
  { id: "blue-guardian", team: "blue", name: "运维守护", responsibility: "检查服务器资源、服务和连接状态，优先识别影响稳定性的异常。先收集只读证据，区分已确认事实与待验证假设，再给出分级处置建议。", providerId: null, model: null },
  { id: "blue-logs", team: "blue", name: "日志分析", responsibility: "围绕故障时间段关联日志、错误与资源变化，定位异常链路。引用可复核的证据，列出可能原因及下一步验证方式，不把相关性直接当作根因。", providerId: null, model: null },
  { id: "red-review", team: "red", name: "风险审查", responsibility: "审查当前运维方案中的权限、配置、暴露面与恢复风险。只在用户授权的目标范围内进行检查，说明影响、证据与建议，不擅自扩大检查或执行改动。", providerId: null, model: null },
  { id: "red-validation", team: "red", name: "变更验证", responsibility: "检查拟议变更的前置条件、影响范围、验收标准和回滚路径。主动寻找反例与遗漏，区分变更前后证据；任何实际改动仍遵循当前权限与批准流程。", providerId: null, model: null },
  { id: "orange-incident", team: "orange", name: "故障协调", responsibility: "整理故障影响、时间线、已完成操作、待确认事项与下一步优先级。围绕已有证据协调排查和交接，明确需要用户决策的节点，不捏造执行结果。", providerId: null, model: null },
  { id: "orange-retrospective", team: "orange", name: "复盘整理", responsibility: "汇总故障和处置记录，整理事实时间线、已确认原因、恢复过程与遗留风险。将改进项写成可验证的行动建议，对证据不足的部分明确标注待确认。", providerId: null, model: null },
];

const DEFAULT_ROLES: readonly AgentRole[] = [
  { id: "blue-guardian", team: "blue", name: "防守与响应", responsibility: "检查服务器、服务、权限与安全配置，提出系统加固建议。结合日志和运行证据开展威胁狩猎与入侵事件响应，区分事实、假设和待确认影响；实际变更遵循当前批准要求。", providerId: null, model: null },
  { id: "blue-logs", team: "blue", name: "日志分析", responsibility: "围绕故障时间段关联日志、错误与资源变化，定位异常链路。引用可复核的证据，列出可能原因及下一步验证方式，不把相关性直接当作根因。", providerId: null, model: null },
  { id: "red-review", team: "red", name: "授权攻击测试", responsibility: "先确认目标、授权范围、攻击目标与停止条件，再模拟攻击者分析可利用的薄弱点。仅在明确授权范围内验证攻击路径与防线，记录证据和影响；角色身份不扩大工具权限或自动批准任何操作。", providerId: null, model: null },
  { id: "red-validation", team: "red", name: "攻击路径验证", responsibility: "根据已确认的授权目标设计可复核的攻击路径与验证步骤，检查能否突破防线并达到约定目标。标明前置条件、影响与停止条件，区分推测和已验证结果，并提出修复后的复测方式。", providerId: null, model: null },
  { id: "purple-detection", team: "purple", name: "检测共建", responsibility: "协调红蓝双方，把授权测试中的攻击方法映射为日志信号、检测规则与防护措施。保留攻击步骤、证据和检测覆盖的对应关系；协作建议仍遵循各自授权范围。", providerId: null, model: null },
  { id: "purple-retest", team: "purple", name: "防护复测", responsibility: "依据红队测试证据与蓝队改进项，制定复测条件和通过标准，验证检测、防护与响应是否有效。记录遗漏、误报和待验证项，形成可重复的红蓝协作闭环。", providerId: null, model: null },
  { id: "white-scope", team: "white", name: "范围与规则", responsibility: "整理演习目标、授权资产、允许的操作、禁止事项、时间窗口与停止条件。发现范围不明或可能越界时暂停相关建议并请求确认；白队角色不能授予权限或替代用户批准。", providerId: null, model: null },
  { id: "white-control", team: "white", name: "演习裁判", responsibility: "依据事先约定的规则整理评分、证据、影响与停止条件，监督双方是否越界。明确已确认结果与争议项，提示需要用户决策的风险，并保留审计与交接记录。", providerId: null, model: null },
  { id: "yellow-architecture", team: "yellow", name: "系统设计", responsibility: "从运维需求出发设计系统架构、接口、数据与部署方案，明确约束和取舍。把安全性、可维护性与可观测性纳入设计，给出可验证的实现计划。", providerId: null, model: null },
  { id: "yellow-engineering", team: "yellow", name: "开发与修复", responsibility: "依据确认的需求与问题证据提出代码实现或修复，说明修改位置、兼容性、验证方式和回滚路径。复用现有能力，避免无关改动，实际写入与部署遵循当前批准要求。", providerId: null, model: null },
  { id: "orange-incident", team: "orange", name: "漏洞协同修复", responsibility: "协调攻击人员与开发者，将授权测试中的漏洞成因、攻击前置条件与证据转化为代码修复方案。明确修复位置、回归验证与复测标准，不把未复现的推测当作已确认漏洞。", providerId: null, model: null },
  { id: "orange-retrospective", team: "orange", name: "安全开发改进", responsibility: "从已确认的攻击经验和漏洞成因提炼安全编码、代码审查与开发流程改进。给出具体可验证的规则和测试建议，说明适用边界，帮助避免同类问题再次出现。", providerId: null, model: null },
  { id: "green-observability", team: "green", name: "可观测性共建", responsibility: "协调防守人员与开发者，完善日志、指标、告警与安全事件设计。明确检测所需字段、关联标识、敏感信息处理及验证方式，让产品更易监控与检测。", providerId: null, model: null },
  { id: "green-response", team: "green", name: "响应与取证设计", responsibility: "把防守与事件响应需求转化为产品和运维能力，完善审计轨迹、证据留存、隔离恢复与故障交接设计。说明采集范围、权限与保留策略，并验证响应和取证流程。", providerId: null, model: null },
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
  if (saved.catalogVersion !== undefined && saved.catalogVersion !== ROLE_CATALOG_VERSION) throw new Error("本机角色目录版本无法识别。");
  const legacy = saved.catalogVersion === undefined;
  const roles = saved.roles.map(record);
  return DEFAULT_ROLES.map((base) => {
    const role = roles.find((candidate) => candidate?.id === base.id);
    if (!role) return { ...base };
    const providerId = text(role.providerId, 512);
    const previous = legacy ? LEGACY_ROLE_DEFAULTS.find((candidate) => candidate.id === base.id) : undefined;
    const name = text(role.name, 60);
    const responsibility = text(role.responsibility, 4000);
    return {
      ...base,
      name: previous && name === previous.name ? base.name : name ?? base.name,
      responsibility: previous && responsibility === previous.responsibility ? base.responsibility : responsibility ?? base.responsibility,
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
  const clean = parseAgentRoles({ version: 1, catalogVersion: ROLE_CATALOG_VERSION, roles });
  storage.setItem(key, JSON.stringify({ version: 1, catalogVersion: ROLE_CATALOG_VERSION, roles: clean }));
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
