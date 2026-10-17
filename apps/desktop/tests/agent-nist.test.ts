import assert from "node:assert/strict";
import { afterEach, test } from "node:test";
import type { AgentPromptPart, AiProviderConfig } from "@yukinal/shared";

import {
  AGENT_NIST_STORAGE_KEY,
  AGENT_TEAMS,
  defaultAgentRoles,
  handoffAgentRole,
  loadAgentRoles,
  roleBinding,
  roleModelChoices,
  resolveAgentRole,
  rolePersona,
  saveAgentRoles,
} from "../src/features/agent-nist/agent-nist.js";
import { useWorkspaceStore } from "../src/stores/workspace-store.js";

const initialWorkspace = useWorkspaceStore.getState();
afterEach(() => useWorkspaceStore.setState(initialWorkspace, true));

function provider(id = "provider-a", enabled = true): AiProviderConfig {
  return {
    id, enabled, kind: "openai-compatible", label: `Connection ${id}`, baseUrl: "http://127.0.0.1:1234/v1",
    model: "default-model", createdAt: "", updatedAt: "",
    models: [
      { id: "default-model", label: "Default model", supportsStreaming: true, supportsToolCalling: true },
      { id: "reasoning:model-v2", label: "Reasoning model", supportsStreaming: true, supportsToolCalling: true },
    ],
  };
}

function configuredRole(providerId = "provider-a") {
  return { ...defaultAgentRoles()[0]!, name: "自定义守护", responsibility: "先检查证据，再提出可验证的建议。", providerId, model: "reasoning:model-v2" };
}

function memoryStorage() {
  const values = new Map<string, string>();
  return { values, getItem: (key: string) => values.get(key) ?? null, setItem: (key: string, value: string) => { values.set(key, value); } };
}

test("the seven-team catalog keeps one stable assignment per role across reloads", () => {
  const storage = memoryStorage();
  const defaults = defaultAgentRoles();
  assert.equal(new Set(defaults.map((role) => role.id)).size, defaults.length);
  assert.deepEqual(new Set(defaults.map((role) => role.team)), new Set(["blue", "red", "purple", "white", "yellow", "orange", "green"]));
  for (const team of AGENT_TEAMS) {
    const roles = defaults.filter((role) => role.team === team.id);
    assert.equal(roles.length, 2);
    roles[0]!.providerId = `connection-${team.id}`;
    roles[0]!.model = `model-${team.id}`;
  }
  saveAgentRoles(storage, defaults);
  assert.deepEqual(loadAgentRoles(storage).roles, defaults);
});

test("upgrading the original catalog updates untouched fields and preserves custom duties and model assignments", () => {
  const storage = memoryStorage();
  const legacy = JSON.stringify({ version: 1, roles: [
    { id: "red-review", name: "风险审查", responsibility: "自定义测试范围：仅预发布。", providerId: "red-connection", model: "red-model" },
    { id: "orange-incident", name: "自定义协作角色", responsibility: "整理故障影响、时间线、已完成操作、待确认事项与下一步优先级。围绕已有证据协调排查和交接，明确需要用户决策的节点，不捏造执行结果。", providerId: "orange-connection", model: "orange-model" },
  ] });
  storage.values.set(AGENT_NIST_STORAGE_KEY, legacy);
  const loaded = loadAgentRoles(storage);
  const defaults = defaultAgentRoles();
  assert.equal(loaded.notice, null);
  assert.equal(loaded.roles.length, 14);
  assert.deepEqual(loaded.roles.find((role) => role.id === "red-review"), {
    ...defaults.find((role) => role.id === "red-review"), responsibility: "自定义测试范围：仅预发布。", providerId: "red-connection", model: "red-model",
  });
  assert.deepEqual(loaded.roles.find((role) => role.id === "orange-incident"), {
    ...defaults.find((role) => role.id === "orange-incident"), name: "自定义协作角色", providerId: "orange-connection", model: "orange-model",
  });
  assert.equal(storage.values.get(AGENT_NIST_STORAGE_KEY), legacy, "loading must not write storage");
  saveAgentRoles(storage, loaded.roles);
  assert.deepEqual(loadAgentRoles(storage).roles, loaded.roles);
});

test("newly saved custom values matching an old default are not migrated on later reloads", () => {
  const storage = memoryStorage();
  const roles = defaultAgentRoles();
  roles.find((role) => role.id === "orange-incident")!.name = "故障协调";
  saveAgentRoles(storage, roles);
  assert.equal(loadAgentRoles(storage).roles.find((role) => role.id === "orange-incident")?.name, "故障协调");
});

test("local role configuration survives reload and stores only role fields", () => {
  const storage = memoryStorage();
  const roles = defaultAgentRoles();
  roles[0] = { ...configuredRole(), name: "  自定义守护  " };
  const extraFields = { ...roles[0], team: "red" as const, apiKey: "must-not-persist", customHeaders: { authorization: "secret" } };
  saveAgentRoles(storage, [extraFields, ...roles.slice(1)]);
  const loaded = loadAgentRoles(storage);
  assert.equal(loaded.notice, null);
  assert.deepEqual(loaded.roles[0], { ...configuredRole(), team: "blue" });
  assert.deepEqual(loaded.roles.slice(1), roles.slice(1));
  assert.equal(storage.values.get(AGENT_NIST_STORAGE_KEY)?.includes("must-not-persist"), false);
  assert.equal(storage.values.get(AGENT_NIST_STORAGE_KEY)?.includes("authorization"), false);
  loaded.roles[1]!.name = "changed in memory";
  assert.notEqual(loadAgentRoles(storage).roles[1]?.name, "changed in memory");
});

test("preview role bindings use separate storage and never replace the real local configuration", () => {
  const storage = memoryStorage();
  const real = defaultAgentRoles();
  real[0] = configuredRole("real-provider");
  saveAgentRoles(storage, real);
  const preview = defaultAgentRoles();
  preview[0] = configuredRole("fixture-provider");
  const previewKey = `${AGENT_NIST_STORAGE_KEY}.preview`;
  saveAgentRoles(storage, preview, previewKey);
  assert.equal(loadAgentRoles(storage).roles[0]?.providerId, "real-provider");
  assert.equal(loadAgentRoles(storage, previewKey).roles[0]?.providerId, "fixture-provider");
});

test("unreadable role storage falls back without overwriting stored data or caller edits", () => {
  const storage = memoryStorage();
  for (const saved of ["invalid JSON", JSON.stringify({ version: 2, roles: [] }), JSON.stringify({ version: 1, catalogVersion: 99, roles: [] })]) {
    storage.values.set(AGENT_NIST_STORAGE_KEY, saved);
    const loaded = loadAgentRoles(storage);
    assert.ok(loaded.notice);
    assert.deepEqual(loaded.roles, defaultAgentRoles());
    assert.equal(storage.values.get(AGENT_NIST_STORAGE_KEY), saved);
  }
  const edited = [configuredRole(), ...defaultAgentRoles().slice(1)];
  const before = structuredClone(edited);
  assert.throws(() => saveAgentRoles({ setItem: () => { throw new Error("storage unavailable"); } }, edited));
  assert.deepEqual(edited, before);
  assert.ok(loadAgentRoles({ getItem: () => { throw new Error("blocked"); } }).notice);
});

test("preferred team selection preserves the coordinator model and visible draft", () => {
  const providers = [provider(), provider("provider-b")];
  const originals = structuredClone(providers);
  const role = configuredRole("provider-b");
  useWorkspaceStore.setState({ selectedServerId: "server-current", selectedProviderId: "provider-a", selectedModel: "default-model", agentDraft: "", agentAttachments: [], agentBusy: false, agentOpen: false, agentHistoryOpen: true, agentSessionArchived: false });
  const observations: Array<{ draft: string; providerId: string | null; model: string | null; historyOpen: boolean }> = [];
  const stop = useWorkspaceStore.subscribe((state) => observations.push({ draft: state.agentDraft, providerId: state.selectedProviderId, model: state.selectedModel, historyOpen: state.agentHistoryOpen }));
  const result = handoffAgentRole(role, providers);
  stop();
  assert.equal(result.ok, true);
  const state = useWorkspaceStore.getState();
  assert.equal(observations.length, 1);
  assert.equal(state.selectedProviderId, "provider-a");
  assert.equal(state.selectedModel, "default-model");
  assert.equal(state.selectedServerId, "server-current");
  assert.equal(state.agentOpen, true);
  assert.equal(state.agentHistoryOpen, false);
  assert.equal(observations[0]?.historyOpen, false);
  assert.equal(state.agentBusy, false);
  assert.equal(state.agentDraft, "");
  assert.equal(state.agentRoleId, role.id);
  assert.ok(rolePersona(role).systemPrompt.includes(role.responsibility));
  assert.deepEqual(providers, originals);
});

test("archived chats reject role handoff until the user opens a new or restored editable chat", () => {
  useWorkspaceStore.setState({ agentDraft: "", agentAttachments: [], agentBusy: false, agentHistoryOpen: true, selectedProviderId: "keep-provider", selectedModel: "keep-model" });
  useWorkspaceStore.getState().setAgentSessionArchived(true);
  const archived = useWorkspaceStore.getState();
  const blocked = handoffAgentRole(configuredRole(), [provider()]);
  assert.equal(blocked.ok, false);
  if (!blocked.ok) assert.match(blocked.reason, /归档.*新建或恢复对话/);
  assert.strictEqual(useWorkspaceStore.getState(), archived);
  assert.equal(useWorkspaceStore.getState().agentDraft, "");
  assert.equal(useWorkspaceStore.getState().agentHistoryOpen, true);

  // Only the chat lifecycle opens an editable chat; a role never changes its
  // archived state or creates a replacement session behind the user's back.
  useWorkspaceStore.getState().setAgentSessionArchived(false);
  assert.equal(handoffAgentRole(configuredRole(), [provider()]).ok, true);
  assert.equal(useWorkspaceStore.getState().agentSessionArchived, false);
  assert.equal(useWorkspaceStore.getState().agentHistoryOpen, false);
  assert.equal(useWorkspaceStore.getState().agentDraft, "");
  assert.equal(useWorkspaceStore.getState().agentRoleId, configuredRole().id);
});

test("role changes preserve existing text, whitespace and attachments exactly", () => {
  const attachments: AgentPromptPart[] = [{ type: "text", text: "attached context" }];
  for (const composition of [
    { agentDraft: "unfinished task", agentAttachments: [], agentBusy: false },
    { agentDraft: " \n", agentAttachments: [], agentBusy: false },
    { agentDraft: "", agentAttachments: attachments, agentBusy: false },
    { agentDraft: "unfinished task", agentAttachments: attachments, agentBusy: false },
  ]) {
    useWorkspaceStore.setState({ ...composition, selectedProviderId: "keep-provider", selectedModel: "keep-model" });
    const before = useWorkspaceStore.getState();
    const result = handoffAgentRole(configuredRole(), [provider()]);
    assert.equal(result.ok, true);
    assert.equal(useWorkspaceStore.getState().agentDraft, before.agentDraft);
    assert.strictEqual(useWorkspaceStore.getState().agentAttachments, before.agentAttachments);
  }
});

test("running work rejects role changes and an unbound role follows the current model", () => {
  useWorkspaceStore.setState({ agentDraft: "draft", agentAttachments: [], agentBusy: true, agentSessionArchived: false });
  const before = useWorkspaceStore.getState();
  assert.equal(handoffAgentRole(configuredRole(), [provider()]).ok, false);
  assert.strictEqual(useWorkspaceStore.getState(), before);
  useWorkspaceStore.setState({ agentBusy: false, selectedProviderId: "keep-provider", selectedModel: "keep-model" });
  assert.equal(handoffAgentRole(defaultAgentRoles()[0]!, []).ok, true);
  assert.equal(useWorkspaceStore.getState().selectedModel, "keep-model");
  assert.equal(useWorkspaceStore.getState().selectedProviderId, "keep-provider");
  assert.equal(useWorkspaceStore.getState().agentDraft, "draft");
});

test("missing, inactive and missing-model bindings refuse handoff without selecting a fallback", () => {
  const active = provider();
  const inactive = provider("inactive", false);
  for (const role of [
    configuredRole("deleted"),
    configuredRole("inactive"),
    { ...configuredRole(), model: "removed-model" },
  ]) {
    useWorkspaceStore.setState({ agentDraft: "", agentAttachments: [], agentBusy: false, selectedProviderId: "keep-provider", selectedModel: "keep-model" });
    const before = useWorkspaceStore.getState();
    assert.equal(handoffAgentRole(role, [active, inactive]).ok, false);
    assert.strictEqual(useWorkspaceStore.getState(), before);
  }
});

test("explicit team mentions override a selected persona and automatic routing preserves the user's wording", () => {
  const roles = defaultAgentRoles();
  const prompt = "@红队 检查授权目标的攻击路径";
  assert.equal(resolveAgentRole(prompt, roles, "blue-guardian").role.team, "red");
  assert.equal(resolveAgentRole(prompt, roles, "blue-guardian").source, "mention");
  assert.equal(resolveAgentRole("@yellow 实现接口", roles).role.team, "yellow");
  assert.equal(resolveAgentRole("@PurpleTeam 检查检测覆盖", roles).role.team, "purple");
  assert.equal(resolveAgentRole("@red-team 检查授权目标", roles).source, "mention");
  assert.equal(resolveAgentRole("查看服务日志", roles).role.id, "blue-logs");
  assert.equal(resolveAgentRole("开发代码实现接口", roles).role.id, "yellow-engineering");
  assert.equal(resolveAgentRole("制定授权范围与停止条件", roles).role.team, "white");
  assert.equal(resolveAgentRole("完善日志设计和可观测性", roles).role.team, "green");
  assert.equal(resolveAgentRole("复测红蓝检测覆盖", roles).role.team, "purple");
  assert.equal(resolveAgentRole("制定安全编码与漏洞修复方案", roles).role.team, "orange");
  assert.equal(resolveAgentRole("任意任务", roles, "green-response").source, "selected");
  assert.equal(prompt, "@红队 检查授权目标的攻击路径");
});

test("inactive providers can be assigned from their catalog but stay inactive and cannot hand off", () => {
  const inactive = provider("inactive-api", false);
  const before = structuredClone(inactive);
  const role = configuredRole(inactive.id);
  assert.equal(roleModelChoices(inactive).find((choice) => choice.model === role.model)?.label, "Reasoning model");
  const storage = memoryStorage();
  saveAgentRoles(storage, [role, ...defaultAgentRoles().slice(1)]);
  const saved = loadAgentRoles(storage).roles[0]!;
  assert.match(roleBinding(saved, [inactive]).reason ?? "", /未启用/);
  useWorkspaceStore.setState({ agentDraft: "", agentAttachments: [], agentBusy: false });
  const workspace = useWorkspaceStore.getState();
  assert.equal(handoffAgentRole(saved, [inactive]).ok, false);
  assert.strictEqual(useWorkspaceStore.getState(), workspace);
  assert.deepEqual(inactive, before);
});

test("deployment and persistent watching route to builders and defenders without treating an attack alert as a red-team request", () => {
  const roles = defaultAgentRoles();
  assert.equal(resolveAgentRole("帮我部署这台服务器的环境", roles).role.team, "yellow");
  assert.equal(resolveAgentRole("帮我部署这台服务器的环境", roles).role.id, "yellow-engineering");
  assert.equal(resolveAgentRole("帮我持续检查服务和日志，每5分钟盯着", roles).role.team, "blue");
  assert.equal(resolveAgentRole("这台服务器似乎正在被攻击，帮我检查", roles).role.team, "blue");
  assert.equal(resolveAgentRole("@紫队 持续检查修复后的防护效果", roles).role.team, "purple");
});
