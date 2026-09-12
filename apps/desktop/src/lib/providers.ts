import { useQuery } from "@tanstack/react-query";
import {
  AI_PROVIDER_KINDS,
  IPC_COMMANDS,
  type AiProviderConfig,
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

/**
 * 选择框里的一行：`名称 · 模型`，只有这一对分不开时才补一个**最短的区别**。
 *
 * 名称是用户自己起的，重名是现实（同一个网关开几个账号、几把密钥），而两个长得一模一样
 * 的选项等于让人猜。区别按这个顺序找：
 *
 * 1. `名称 · 模型` 已经不重复 —— 什么都不补，一眼能分清的选项不该背着机器名；
 * 2. 重复，但 `baseUrl` 的 host 不同 —— 补 host（「哪个网关」正是这些同名条目真正不一样
 *    的地方，也是唯一解释「请求发去哪儿」的字段）；
 * 3. 连网关也一样 —— 只剩 id。真实数据里有 90 字符的 id（`prv_ccswitch_codex_…_b30acd2f`），
 *    整条放进下拉等于把选项变成一团乱码，所以取**尾部 6 位**当记号：完整 id 就在下面
 *    那张表单的「Provider ID」里，这个记号只负责「哪一行是哪一行」。尾部万一在同一个
 *    名字下撞上，就退回完整 id —— 宁可长，不可歧义。
 *
 * 只取 host 还有一层保险：路径与内嵌凭据都不会上界面。
 *
 * `all` 是整个列表：判断「是否重复」需要它。条目就是十几个，这里不做缓存。
 */
export function providerOptionLabel(
  provider: AiProviderConfig,
  all: readonly AiProviderConfig[],
): string {
  const base = baseLabel(provider);
  const sameBase = all.filter((other) => baseLabel(other) === base);
  if (sameBase.length < 2) return base;

  const host = providerHost(provider.baseUrl);
  if (host !== null && sameBase.filter((other) => providerHost(other.baseUrl) === host).length === 1) {
    return `${base} · ${host}`;
  }
  const fragment = idFragment(provider.id);
  const ambiguous = sameBase.filter((other) => idFragment(other.id) === fragment).length > 1;
  return `${base} · ${ambiguous ? provider.id : fragment}`;
}

/** `名称 · 模型`。空的一段不留下一个悬挂的分隔符。 */
function baseLabel(provider: AiProviderConfig): string {
  return [provider.label, provider.model]
    .map((part) => part.trim())
    .filter((part) => part !== "")
    .join(" · ");
}

/** id 的尾部记号。短 id（手写的 `prv_a` 之类）原样留下。 */
function idFragment(id: string): string {
  return id.length <= 12 ? id : `…${id.slice(-6)}`;
}

/** `baseUrl` 的 host（含端口）。解析不出来时返回 `null` —— 界面不会因此显示半个地址。 */
export function providerHost(baseUrl: string): string | null {
  try {
    return new URL(baseUrl).host || null;
  } catch {
    return null;
  }
}

/**
 * 「删掉之后密钥怎么了」这句话 —— 三种情况不能共用一句。
 *
 * `credentialReclaimed` 只回答「有没有回收条目」，而**没有引用**的行（本地端点、免鉴权的
 * 网关）也答 `false`：照那个字段直接拼一句话，会对着一个从来没配过密钥的 Provider 说
 * 「密钥仍被别的 Provider 使用」。那不是它想说的话，而这句话本来就不该靠猜 —— 界面手上
 * 有 `apiKeyCredentialRef`，`true/false` 加上「有没有引用」才是完整的答案。
 */
export function providerDeleteNotice(
  provider: AiProviderConfig,
  credentialReclaimed: boolean,
): string {
  const head = `已删除「${provider.label}」`;
  if (!provider.apiKeyCredentialRef) return `${head}。这份配置本来就没有密钥。`;
  return credentialReclaimed
    ? `${head}，那份密钥也已从系统密钥链移除。`
    : `${head}；那份密钥仍被别的 Provider 使用，没有动它。`;
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
