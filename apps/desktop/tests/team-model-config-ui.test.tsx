import assert from "node:assert/strict";
import { after, afterEach, test } from "node:test";
import { registerHooks } from "node:module";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { renderToStaticMarkup } from "react-dom/server";
import type { AiProviderConfig } from "@yukinal/shared";

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
const second: AiProviderConfig = { ...deepseek, id: "deepseek-two", label: "DeepSeek 二线", model: "deepseek-flash" };

/**
 * The role catalog is gone, but persisted storage from before the change still exists
 * on real machines. `localStorage` is therefore seeded with a legacy payload whose
 * text is a sentinel: the page must neither read it nor echo it.
 */
function render(options: { view?: "agent" | "watch" | "model"; busy?: boolean; model?: string; providers?: AiProviderConfig[] } = {}) {
  const saved = JSON.stringify({ version: 1, catalogVersion: 2, roles: [
    { id: "blue-guardian", name: "PRIVATE_CUSTOM_NAME_SENTINEL", responsibility: "PRIVATE_SYSTEM_PROMPT_SENTINEL" },
  ] });
  Object.defineProperty(globalThis, "window", { configurable: true, value: {
    location: { search: "" }, localStorage: { getItem: () => saved },
    addEventListener: () => {}, removeEventListener: () => {}, dispatchEvent: () => true,
  } });
  useWorkspaceStore.setState({ selectedProviderId: "deepseek", selectedModel: options.model ?? "deepseek-chat", agentBusy: options.busy ?? false, agentSessionArchived: false });
  useTeamRuntimeStore.setState({ panelView: options.view ?? "model", runs: [] });
  // Zustand's SSR hook reads its original initial object, while the browser
  // reads getState(). Seed that same SSR snapshot with this isolated fixture.
  Object.assign(workspaceInitial, useWorkspaceStore.getState());
  Object.assign(runtimeInitial, useTeamRuntimeStore.getState());
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity, gcTime: Infinity } } });
  client.setQueryData(["providers"], options.providers ?? [deepseek]);
  client.setQueryData(["servers"], []);
  try { return renderToStaticMarkup(<QueryClientProvider client={client}><AgentNistPane /></QueryClientProvider>); }
  finally { client.clear(); }
}

test("the model page shows the one conversation model and never discloses stored role text", () => {
  const html = render();
  assert.match(html, /运维 Agent 模型/);
  // The single-agent runtime shares one model with its subagents; per-role routing
  // is gone, so the page must not offer it again.
  assert.match(html, /Agent 与它启动的 subagent 共用当前对话模型/);
  assert.match(html, /DeepSeek · deepseek-chat/);
  assert.doesNotMatch(html, /MoE 路由配置|路由方式|团队 AI 连接|应用到全部能力|独立绑定/);
  assert.doesNotMatch(html, /蓝队|红队|紫队|白队|黄队|橙队|绿队|team-roster|team-color-/);
  assert.doesNotMatch(html, /PRIVATE_CUSTOM_NAME_SENTINEL|PRIVATE_SYSTEM_PROMPT_SENTINEL|角色名称|角色职责|保存角色|<textarea/);
});

test("the selected model always comes from the live conversation setting, not the stored role catalog", () => {
  const html = render({ model: "deepseek-flash", providers: [deepseek, second] });
  // The live selection is the selected option, and the hint repeats that exact
  // provider/model pair; the stored role catalog cannot redirect it.
  assert.match(html, /<option value="deepseek:deepseek-flash" selected="">DeepSeek · deepseek-flash<\/option>/);
  assert.match(html, /当前使用：DeepSeek · deepseek-flash/);
  assert.equal(useWorkspaceStore.getState().selectedModel, "deepseek-flash");
});

test("a running conversation still renders the model page with the live selection", () => {
  const html = render({ busy: true });
  assert.match(html, /<select[^>]*aria-label="运维 Agent 模型"/);
  // Busy is a workspace fact; a stale catalog is never used to pretend the
  // selection differs from what the current run will use.
  assert.match(html, /当前使用：DeepSeek · deepseek-chat/);
});

test("the agent page names a single owner and no colour-coded roster", () => {
  const html = render({ view: "agent" });
  assert.match(html, /一个 Agent 负责完整运维流程/);
  assert.match(html, /运维 Agent/);
  assert.doesNotMatch(html, /蓝队|红队|紫队|白队|黄队|橙队|绿队|team-roster|team-color-|PRIVATE_SYSTEM_PROMPT_SENTINEL/);
});

test("without a usable provider the page says so instead of inventing a model", () => {
  const html = render({ providers: [{ ...deepseek, enabled: false }] });
  assert.match(html, /尚未配置可用模型|请先配置 AI 连接/);
});
