import assert from "node:assert/strict";
import { after, afterEach, test } from "node:test";
import { registerHooks } from "node:module";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { renderToStaticMarkup } from "react-dom/server";
import type { AiProviderConfig } from "@yukinal/shared";

import { AGENT_NIST_STORAGE_KEY, defaultAgentRoles, type AgentRole } from "../src/features/agent-nist/agent-nist.js";
import { useTeamRuntimeStore } from "../src/features/agent-nist/team-runtime-store.js";
import { useWorkspaceStore } from "../src/stores/workspace-store.js";

const css = registerHooks({ load: (url, context, next) => url.endsWith(".css") ? { format: "module", source: "", shortCircuit: true } : next(url, context) });
const { AgentNistPane } = await import("../src/features/agent-nist/AgentNistPane.js");
css.deregister();

const originalWindow = Object.getOwnPropertyDescriptor(globalThis, "window");
const workspaceInitial = useWorkspaceStore.getInitialState();
const runtimeInitial = useTeamRuntimeStore.getInitialState();
const workspaceDefaults = { ...workspaceInitial };
const runtimeDefaults = { ...runtimeInitial };
afterEach(() => {
  Object.assign(workspaceInitial, workspaceDefaults);
  Object.assign(runtimeInitial, runtimeDefaults);
  useWorkspaceStore.setState({ ...workspaceDefaults }, true);
  useTeamRuntimeStore.setState({ ...runtimeDefaults }, true);
});
after(() => {
  if (originalWindow) Object.defineProperty(globalThis, "window", originalWindow);
  else Reflect.deleteProperty(globalThis, "window");
});

const deepseek: AiProviderConfig = { id: "deepseek", kind: "openai-compatible", label: "DeepSeek", enabled: true, baseUrl: "https://api.example.test/v1", model: "deepseek-chat", createdAt: "", updatedAt: "" };

function render(options: { roles?: AgentRole[]; view?: "teams" | "config"; busy?: boolean; selectedRoleId?: string | null; model?: string } = {}) {
  const roles = options.roles ?? defaultAgentRoles();
  const saved = JSON.stringify({ version: 1, catalogVersion: 2, roles });
  Object.defineProperty(globalThis, "window", { configurable: true, value: {
    location: { search: "" }, localStorage: { getItem: (key: string) => key === AGENT_NIST_STORAGE_KEY ? saved : null },
    addEventListener: () => {}, removeEventListener: () => {}, dispatchEvent: () => true,
  } });
  useWorkspaceStore.setState({ selectedProviderId: "deepseek", selectedModel: options.model ?? "deepseek-chat", agentRoleId: options.selectedRoleId ?? null, agentBusy: options.busy ?? false, agentSessionArchived: false });
  useTeamRuntimeStore.setState({ panelView: options.view ?? "config", runs: [] });
  // Zustand's SSR hook reads its original initial object, while the browser
  // reads getState(). Seed that same SSR snapshot with this isolated fixture.
  Object.assign(workspaceInitial, useWorkspaceStore.getState());
  Object.assign(runtimeInitial, useTeamRuntimeStore.getState());
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity, gcTime: Infinity } } });
  client.setQueryData(["providers"], [deepseek]);
  client.setQueryData(["servers"], []);
  try { return renderToStaticMarkup(<QueryClientProvider client={client}><AgentNistPane /></QueryClientProvider>); }
  finally { client.clear(); }
}

test("real configuration renders routing and live inherited model status without team selectors or stored prompt/name disclosure", () => {
  const roles = defaultAgentRoles();
  roles[0]!.name = "PRIVATE_CUSTOM_NAME_SENTINEL";
  roles[0]!.responsibility = "PRIVATE_SYSTEM_PROMPT_SENTINEL";
  const html = render({ roles });
  assert.match(html, /MoE 路由配置/);
  assert.match(html, /团队模型配置/);
  assert.match(html, /自动模式由协调助手按目标组合能力/);
  assert.match(html, /应用层任务路由/);
  assert.match(html, /继承对话 · DeepSeek · deepseek-chat/);
  assert.doesNotMatch(html, /蓝队|红队|紫队|白队|黄队|橙队|绿队|agent-nist-teams/);
  assert.doesNotMatch(html, /PRIVATE_CUSTOM_NAME_SENTINEL|PRIVATE_SYSTEM_PROMPT_SENTINEL|角色名称|角色职责|保存角色|<textarea/);
});

test("broken explicit assignments remain visibly unavailable and mixed bindings have no automatic replacement", () => {
  const roles = defaultAgentRoles();
  roles[0]!.providerId = "deleted-provider";
  roles[0]!.model = "old-model";
  const before = structuredClone(roles);
  const html = render({ roles });
  assert.match(html, /独立绑定 · 不可用/);
  assert.match(html, /原来的 AI 连接已不可用/);
  assert.match(html, /继承对话 · DeepSeek/);
  assert.match(html, /当前为不同绑定，请明确选择/);
  assert.match(html, /已有绑定保持不变/);
  assert.match(html, /<button[^>]*type="submit"[^>]*disabled=""[^>]*>应用到全部能力/);
  assert.deepEqual(roles, before);
});

test("inheritance displays the current foreground model even when the stored provider catalog is older", () => {
  const html = render({ model: "deepseek-flash" });
  assert.match(html, /继承对话 · DeepSeek · deepseek-flash/);
  assert.doesNotMatch(html, /绑定的模型已不在|继承对话 · 不可用/);
  assert.equal(useWorkspaceStore.getState().selectedModel, "deepseek-flash");
});

test("active runs disable model persistence and routing while retaining the selected preference", () => {
  const html = render({ busy: true, selectedRoleId: "yellow-engineering" });
  assert.match(html, /对话正在运行，结束后再保存模型配置/);
  assert.match(html, /<select[^>]*aria-label="路由方式"[^>]*disabled=""/);
  assert.match(html, /<select[^>]*aria-label="团队 AI 连接"[^>]*disabled=""/);
  assert.match(html, /<button[^>]*type="submit"[^>]*disabled=""/);
  assert.match(html, /当前优先：部署、开发与修复/);
  assert.match(html, /恢复自动分工/);
});

test("the home page presents four capability groups and uses no seven-color roster or private role text", () => {
  const roles = defaultAgentRoles();
  roles[0]!.responsibility = "PRIVATE_HOME_PROMPT";
  const html = render({ view: "teams", roles });
  for (const label of ["建设与修复", "运行与响应", "验证与复测", "范围与风险"]) assert.ok(html.includes(label));
  assert.equal((html.match(/class="team-capability-toggle"/g) ?? []).length, 4);
  assert.doesNotMatch(html, /蓝队|红队|紫队|白队|黄队|橙队|绿队|team-roster|team-color-|PRIVATE_HOME_PROMPT/);
  assert.match(html, /协调助手按任务需要分工/);
});
