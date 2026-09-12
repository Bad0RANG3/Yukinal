import { useQuery } from "@tanstack/react-query";
import {
  AI_PROVIDER_KINDS,
  IPC_COMMANDS,
  type AiProviderKind,
  type ProviderModelOption,
  type ProviderSaveInput,
} from "@yukinal/shared";

import { callDesktop, isDesktopShell } from "./ipc.js";

export const PROVIDERS_QUERY_KEY = ["providers"] as const;

/** Durable provider list shared by Settings and Agent runs. */
export function useProviders() {
  return useQuery({
    queryKey: PROVIDERS_QUERY_KEY,
    enabled: isDesktopShell(),
    queryFn: async () => (await callDesktop(IPC_COMMANDS.providerList, {})).providers,
  });
}

/**
 * 三种 kind 在界面里的顺序与文案。取值表本身只有一份（`AI_PROVIDER_KINDS`，shared），
 * 这里是「怎么显示」，不是「有哪些」。
 */
export const PROVIDER_KIND_ORDER = AI_PROVIDER_KINDS;

export const PROVIDER_KIND_LABEL: Record<AiProviderKind, string> = {
  "openai-compatible": "OpenAI 兼容（chat / responses）",
  anthropic: "Anthropic Messages API",
  gemini: "Google Gemini generateContent",
};

/**
 * 每种 kind 的公开端点，用于「切换到该协议时补一个空字段」。
 *
 * `openai-compatible` 是 `null`：它覆盖的是我们不拥有的端点（OpenRouter、Ollama、vLLM、
 * 内部网关……），替用户编一个默认值等于把密钥送去一个他没选过的服务商。另外两个的地址
 * 没有歧义 —— 协议本身就是那家服务商的。同一份事实在 Rust 侧是
 * `commands/provider.rs` 的 `ANTHROPIC_DEFAULT_BASE_URL` / `GEMINI_DEFAULT_BASE_URL`
 * （那边是权威的运行时兜底；这里只是替用户把字段填好，两者请一起改）。
 */
export const PROVIDER_KIND_DEFAULT_BASE_URL: Record<AiProviderKind, string | null> = {
  "openai-compatible": null,
  anthropic: "https://api.anthropic.com",
  gemini: "https://generativelanguage.googleapis.com",
};

/**
 * `wireApi` 只对 `openai-compatible` 有意义（ADR 0011 第 2 点）。
 *
 * 界面据此**不渲染**这个字段，保存时也不带这个键 —— 共享 schema 把「原生 kind 带
 * wireApi」判为非法输入，而不是默默忽略它，所以「顺手带上」会让保存直接失败。
 */
export function providerKindUsesWireApi(kind: AiProviderKind): boolean {
  return kind === "openai-compatible";
}

/** base URL 该填到哪一层由适配器决定：它会自己接上协议路径。 */
export function providerKindBaseUrlHint(kind: AiProviderKind): string {
  switch (kind) {
    case "anthropic":
      return "填到域名即可：适配器自己接 /v1/messages，不要重复写 /v1。";
    case "gemini":
      return "填到域名即可：适配器自己接 /v1beta/models/{model}:streamGenerateContent。";
    case "openai-compatible":
      return "填完整基地址，例如 https://openrouter.ai/api/v1（尾部斜杠会被去掉）。";
  }
}

export interface ProviderDraft {
  providerId?: string;
  kind: AiProviderKind;
  label?: string;
  baseUrl: string;
  model: string;
  apiKey?: string;
  wireApi?: "chat" | "responses";
  models?: ProviderModelOption[];
}

/**
 * 把编辑器里的草稿变成 `provider_save` 的 payload。
 *
 * 抽成纯函数是为了让这条规则可以被测到，而不是只在渲染时成立：
 * **`kind` 永远出现，`wireApi` 只在 openai-compatible 时出现。** 空字符串在这里变成「不发送」，
 * 因为共享 schema 是 strictObject —— 一个 `label: ""` 或 `wireApi: undefined` 之外的字段形状
 * 都会被它当成 drift 拒掉。
 */
export function providerSavePayload(draft: ProviderDraft): ProviderSaveInput {
  const payload: ProviderSaveInput = {
    providerId: draft.providerId?.trim() || undefined,
    kind: draft.kind,
    label: draft.label?.trim() || undefined,
    baseUrl: draft.baseUrl.trim(),
    model: draft.model.trim(),
    apiKey: draft.apiKey?.trim() || undefined,
    models: draft.models?.length ? draft.models : undefined,
  };
  // 原生 kind 带上 `wireApi` 不是「被忽略的字段」而是非法输入：保存会以 INVALID_PARAMS 失败。
  // 所以这里不发送它，而不是发送一个空值。
  if (providerKindUsesWireApi(draft.kind) && draft.wireApi) payload.wireApi = draft.wireApi;
  return payload;
}
