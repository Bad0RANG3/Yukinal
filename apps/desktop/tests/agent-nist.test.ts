import assert from "node:assert/strict";
import { afterEach, test } from "node:test";
import type { AgentPromptPart, AiProviderConfig } from "@yukinal/shared";

import {
  AGENT_NIST_STORAGE_KEY,
  defaultAgentRoles,
  handoffAgentRole,
  loadAgentRoles,
  roleBinding,
  roleModelChoices,
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
  for (const saved of ["invalid JSON", JSON.stringify({ version: 2, roles: [] })]) {
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

test("role handoff atomically prepares the chosen model and editable duties without changing saved providers", () => {
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
  assert.equal(state.selectedProviderId, "provider-b");
  assert.equal(state.selectedModel, "reasoning:model-v2");
  assert.equal(state.selectedServerId, "server-current");
  assert.equal(state.agentOpen, true);
  assert.equal(state.agentHistoryOpen, false);
  assert.equal(observations[0]?.historyOpen, false);
  assert.equal(state.agentBusy, false);
  assert.match(state.agentDraft, /蓝队 · 自定义守护/);
  assert.ok(state.agentDraft.includes(role.responsibility));
  assert.ok(state.agentDraft.endsWith("本次任务：\n"));
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
  assert.ok(useWorkspaceStore.getState().agentDraft.includes("自定义守护"));
});

test("existing text, whitespace drafts, attachments and running work cannot be overwritten by a role", () => {
  const attachments: AgentPromptPart[] = [{ type: "text", text: "attached context" }];
  for (const composition of [
    { agentDraft: "unfinished task", agentAttachments: [], agentBusy: false },
    { agentDraft: " \n", agentAttachments: [], agentBusy: false },
    { agentDraft: "", agentAttachments: attachments, agentBusy: false },
    { agentDraft: "unfinished task", agentAttachments: attachments, agentBusy: false },
    { agentDraft: "", agentAttachments: [], agentBusy: true },
  ]) {
    useWorkspaceStore.setState({ ...composition, selectedProviderId: "keep-provider", selectedModel: "keep-model" });
    const before = useWorkspaceStore.getState();
    const result = handoffAgentRole(configuredRole(), [provider()]);
    assert.equal(result.ok, false);
    assert.strictEqual(useWorkspaceStore.getState(), before);
  }
});

test("missing, inactive and missing-model bindings refuse handoff without selecting a fallback", () => {
  const active = provider();
  const inactive = provider("inactive", false);
  for (const role of [
    { ...configuredRole(), providerId: null, model: null },
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
