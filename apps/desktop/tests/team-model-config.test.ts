import assert from "node:assert/strict";
import { afterEach, test } from "node:test";
import type { AiProviderConfig } from "@yukinal/shared";

import { AGENT_NIST_STORAGE_KEY, defaultAgentRoles, loadAgentRoles, saveAgentRoles } from "../src/features/agent-nist/agent-nist.js";
import { commonModelBinding, saveTeamModelBinding, teamModelBinding } from "../src/features/agent-nist/team-model-config.js";
import { coordinatorConfig } from "../src/features/agent/coordinator-state.js";
import { useWorkspaceStore } from "../src/stores/workspace-store.js";

const initial = useWorkspaceStore.getState();
afterEach(() => useWorkspaceStore.setState(initial, true));

function provider(enabled = true): AiProviderConfig {
  return { id: "deepseek", kind: "openai-compatible", label: "DeepSeek", enabled,
    baseUrl: "https://api.example.test/v1", model: "deepseek-chat", createdAt: "", updatedAt: "",
    models: [{ id: "deepseek-chat", label: "DeepSeek Chat", supportsStreaming: true, supportsToolCalling: true },
      { id: "deepseek-reasoner", label: "DeepSeek Reasoner", supportsStreaming: true, supportsToolCalling: true }] };
}

function storage() {
  const values = new Map<string, string>();
  return { values, getItem: (key: string) => values.get(key) ?? null, setItem: (key: string, value: string) => { values.set(key, value); } };
}

test("model edits retain exact legacy custom names, prompts and unrelated records while reaching the real coordinator payload", () => {
  const local = storage();
  const legacy = { version: 1, marker: "keep-envelope", roles: [
    { id: "blue-guardian", name: "  旧自定义名称  ", responsibility: "  私有职责\n原样保留  ", providerId: "removed", model: "old-model", custom: "keep-field" },
    { id: "blue-logs", name: "自定义日志", responsibility: "私有日志职责", providerId: "other", model: "other-model" },
    { id: "future-role", name: "未来角色", responsibility: "未来字段保留", custom: true },
  ] };
  local.values.set(AGENT_NIST_STORAGE_KEY, JSON.stringify(legacy));
  const roles = saveTeamModelBinding(local, "blue-guardian", { providerId: "deepseek", model: "deepseek-reasoner" }, [provider()]);
  const saved = JSON.parse(local.values.get(AGENT_NIST_STORAGE_KEY)!);
  assert.deepEqual(saved, { ...legacy, roles: [{ ...legacy.roles[0], providerId: "deepseek", model: "deepseek-reasoner" }, ...legacy.roles.slice(1)] });
  const dispatched = coordinatorConfig(roles, [provider()], [], "blue-guardian");
  assert.equal(dispatched.preferredRoleId, "blue-guardian");
  assert.equal(dispatched.roles.find((role) => role.id === "blue-guardian")?.providerId, "deepseek");
  assert.equal(dispatched.roles.find((role) => role.id === "blue-guardian")?.model, "deepseek-reasoner");
  assert.equal(dispatched.roles.some((role) => role.id === "blue-logs"), false, "broken independent bindings stay unavailable instead of inheriting silently");
});

test("explicitly applying inheritance updates all model bindings and leaves an existing run snapshot and foreground model unchanged", () => {
  const local = storage();
  const roles = defaultAgentRoles().map((role) => ({ ...role, providerId: "deepseek", model: "deepseek-reasoner" }));
  saveAgentRoles(local, roles);
  const snapshot = coordinatorConfig(roles, [provider()], [], null);
  const original = structuredClone(snapshot);
  useWorkspaceStore.setState({ agentBusy: false, selectedProviderId: "deepseek", selectedModel: "deepseek-chat", agentDraft: "未发送草稿" });
  const before = useWorkspaceStore.getState();
  saveTeamModelBinding(local, "all", { providerId: null, model: null }, [provider()]);
  const reloaded = loadAgentRoles(local).roles;
  assert.ok(reloaded.every((role) => role.providerId === null && role.model === null));
  assert.deepEqual(reloaded.map((role) => [role.id, role.name, role.responsibility]), roles.map((role) => [role.id, role.name, role.responsibility]));
  assert.deepEqual(snapshot, original);
  assert.strictEqual(useWorkspaceStore.getState(), before);
  assert.ok(coordinatorConfig(reloaded, [provider()], [], null).roles.every((role) => role.providerId === undefined && role.model === undefined));
});

test("saving one capability merges the latest persisted edits rather than an old pane snapshot", () => {
  const local = storage();
  saveAgentRoles(local, defaultAgentRoles());
  const stale = loadAgentRoles(local).roles;
  const fresh = loadAgentRoles(local).roles;
  fresh[0]!.name = "另一窗口新名称";
  fresh[0]!.responsibility = "另一窗口新职责";
  fresh[1]!.providerId = "other";
  fresh[1]!.model = "other-model";
  saveAgentRoles(local, fresh);
  saveTeamModelBinding(local, stale[0]!.id, { providerId: "deepseek", model: "deepseek-chat" }, [provider()]);
  const updated = loadAgentRoles(local).roles;
  assert.equal(updated[0]!.name, fresh[0]!.name);
  assert.equal(updated[0]!.responsibility, fresh[0]!.responsibility);
  assert.deepEqual(updated[1], fresh[1]);
});

test("missing, disabled and removed-model assignments cannot overwrite saved bindings", () => {
  const local = storage();
  saveAgentRoles(local, defaultAgentRoles());
  const before = local.values.get(AGENT_NIST_STORAGE_KEY);
  for (const [binding, providers] of [
    [{ providerId: "missing", model: "deepseek-chat" }, [provider()]],
    [{ providerId: "deepseek", model: "deepseek-chat" }, [provider(false)]],
    [{ providerId: "deepseek", model: "removed" }, [provider()]],
  ] as const) {
    assert.throws(() => saveTeamModelBinding(local, "all", binding, providers));
    assert.equal(local.values.get(AGENT_NIST_STORAGE_KEY), before);
  }
  assert.throws(() => saveTeamModelBinding(local, "unknown-role", { providerId: null, model: null }, [provider()]));
  assert.equal(local.values.get(AGENT_NIST_STORAGE_KEY), before);
});

test("active runs and a run starting during the save are both rejected before persistence", () => {
  const local = storage();
  saveAgentRoles(local, defaultAgentRoles());
  const before = local.values.get(AGENT_NIST_STORAGE_KEY);
  useWorkspaceStore.setState({ agentBusy: true });
  assert.throws(() => saveTeamModelBinding(local, "all", { providerId: null, model: null }, []), /对话正在运行/);
  useWorkspaceStore.setState({ agentBusy: false });
  assert.throws(() => saveTeamModelBinding({ ...local, getItem: (key) => { useWorkspaceStore.setState({ agentBusy: true }); return local.getItem(key); } }, "all", { providerId: null, model: null }, []), /对话正在运行/);
  assert.equal(local.values.get(AGENT_NIST_STORAGE_KEY), before);
});

test("unreadable storage and failed writes preserve the old data and caller draft", () => {
  const local = storage();
  const binding = { providerId: "deepseek", model: "deepseek-chat" };
  for (const raw of ["broken-json", JSON.stringify({ version: 99, roles: [] }), JSON.stringify({ version: 1, catalogVersion: 99, roles: [] })]) {
    local.values.set(AGENT_NIST_STORAGE_KEY, raw);
    assert.throws(() => saveTeamModelBinding(local, "all", binding, [provider()]));
    assert.equal(local.values.get(AGENT_NIST_STORAGE_KEY), raw);
  }
  local.values.delete(AGENT_NIST_STORAGE_KEY);
  assert.throws(() => saveTeamModelBinding({ ...local, setItem: () => { throw new Error("disk unavailable"); } }, "all", binding, [provider()]), /disk unavailable/);
  assert.deepEqual(binding, { providerId: "deepseek", model: "deepseek-chat" });
  assert.equal(local.values.size, 0);
});

test("inheritance resolves the exact conversation model and clearly reports absent or unavailable configuration without fallback", () => {
  const inherit = { providerId: null, model: null };
  const inherited = teamModelBinding(inherit, [provider()], { providerId: "deepseek", model: "deepseek-reasoner" });
  assert.equal(inherited.reason, null);
  assert.match(inherited.label, /继承对话.*DeepSeek.*Reasoner/);
  assert.match(teamModelBinding(inherit, [provider()], { providerId: null, model: null }).reason!, /尚未选择/);
  assert.ok(teamModelBinding(inherit, [provider()], { providerId: "missing", model: "deepseek-chat" }).reason);
  assert.ok(teamModelBinding(inherit, [provider(false)], { providerId: "deepseek", model: "deepseek-chat" }).reason);
  const currentOutsideCatalog = teamModelBinding(inherit, [provider()], { providerId: "deepseek", model: "deepseek-flash" });
  assert.equal(currentOutsideCatalog.reason, null, "inherit the exact model sent by the conversation, even if its saved catalog is stale");
  assert.match(currentOutsideCatalog.label, /继承对话.*deepseek-flash/);
  assert.ok(teamModelBinding({ providerId: "deepseek", model: "deepseek-flash" }, [provider()], { providerId: "deepseek", model: "deepseek-flash" }).reason, "independent bindings retain their real catalog requirement");
  assert.match(teamModelBinding({ providerId: "missing", model: "old" }, [provider()], { providerId: "deepseek", model: "deepseek-chat" }).label, /独立绑定.*不可用/);
});

test("mixed bindings remain mixed and preview saves cannot overwrite the real configuration", () => {
  const local = storage();
  const roles = defaultAgentRoles();
  roles[0]!.providerId = "deepseek";
  roles[0]!.model = "deepseek-chat";
  saveAgentRoles(local, roles);
  const before = local.values.get(AGENT_NIST_STORAGE_KEY);
  assert.equal(commonModelBinding(roles), null);
  const previewKey = `${AGENT_NIST_STORAGE_KEY}.preview`;
  saveTeamModelBinding(local, "all", { providerId: "deepseek", model: "deepseek-reasoner" }, [provider()], previewKey);
  assert.deepEqual(commonModelBinding(loadAgentRoles(local, previewKey).roles), { providerId: "deepseek", model: "deepseek-reasoner" });
  assert.equal(local.values.get(AGENT_NIST_STORAGE_KEY), before);
});
