/**
 * AI provider config schemas: the API key is a *transient* input that Rust drops
 * into the keychain; only the reference is stored.
 */

import { z } from "zod";

import { AI_PROVIDER_KINDS, type AiProviderKind } from "../types/provider.js";

/**
 * 一个可用的 http(s) base URL，**不带**内嵌凭据。
 *
 * 「不带凭据」这条不是洁癖：`https://user:pass@host` 会被 `new URL()` 正常解析，
 * 于是密钥会以明文形式留在配置里、进日志、进数据库 —— 而它本该走
 * keychain（见本文件顶部：API key 是 transient 输入，只存引用）。
 *
 * 这份定义原先是两份逐字节相同的副本（本文件一份、`permission.ts` 一份，
 * 只有变量名和 `min/max` 的书写方式不同）。同时 `RuntimeSettings.tsx` 又在
 * 前端做了第三次检查，**而且漏掉了凭据这一条** —— 于是编辑器认为合法、
 * IPC 层随后拒绝，用户看到的是一个没有来源的错误。
 */
export const HttpBaseUrlSchema = z.string().trim().min(1).max(2_048).refine((value) => {
  try {
    const url = new URL(value);
    return (url.protocol === "http:" || url.protocol === "https:") && !url.username && !url.password;
  } catch {
    return false;
  }
}, "baseUrl must be an http(s) URL without embedded credentials");

/** Provider 配置里的 base URL 与运行时用的是同一条规则，别名只为读起来贴合语境。 */
const ProviderBaseUrlSchema = HttpBaseUrlSchema;

export const ProviderModelOptionSchema = z.strictObject({
  id: z.string().trim().min(1).max(256),
  label: z.string().trim().min(1).max(256),
  contextWindow: z.number().int().positive().max(10_000_000).optional(),
  supportsToolCalling: z.boolean(),
  supportsStreaming: z.boolean(),
});

/** 三个 kind 的唯一取值表在 `types/provider.ts`；这里只是把同一份列表交给 Zod。 */
export const AiProviderKindSchema = z.enum(AI_PROVIDER_KINDS);

export const WireApiSchema = z.enum(["chat", "responses"]);

/**
 * `kind` 与 `wireApi` 是正交的两轴（ADR 0011 第 2 点），因此它们的组合里有一半是非法的：
 * `wireApi` 只在 `openai-compatible` 里选方言，另外两种 kind 的协议本身就是方言。
 *
 * 这里选的是**拒绝**，不是忽略：一个带 `wireApi: "responses"` 的 `gemini` 配置如果被静默
 * 接受，它的含义就取决于读它的那段代码有没有记得跳过这个字段 —— 数据库里存一份、读回来是
 * 另一回事，正是「配了一个不知道自己是什么的 Provider」。所以三个 schema（保存输入、
 * 随运行下发的 `RuntimeProviderConfig`、从数据库读回的 `ProviderConfig`）用的是同一条规则：
 * kind 不是 `openai-compatible` 时 `wireApi` 必须**缺省**，带上它就是非法输入。
 *
 * Rust 侧对应地只对 `openai-compatible` 序列化这个字段（`commands/provider.rs` 的
 * `runtime_provider_config()` 与 `repositories/providers.rs` 的读路径）。
 */
export function wireApiAppliesTo(value: {
  kind: AiProviderKind;
  wireApi?: "chat" | "responses";
}): boolean {
  return value.wireApi === undefined || value.kind === "openai-compatible";
}

const WIRE_API_IS_OPENAI_ONLY = {
  message: 'wireApi only applies to kind "openai-compatible"; native kinds have no dialect axis',
  path: ["wireApi"],
};

/**
 * Provider credentials must use apiKey + the OS credential store. Custom headers
 * are limited to non-secret gateway metadata so a custom provider cannot persist
 * another credential channel in SQLite.
 */
const SafeCustomHeaderNameSchema = z.string().trim().min(1).max(128).refine(
  (name) => [
    "http-referer",
    "referer",
    "origin",
    "user-agent",
    "x-app-name",
    "x-app-version",
    "x-client-name",
    "x-client-version",
    "x-title",
  ].includes(name.toLowerCase()),
  "custom header is not approved for non-secret metadata",
);

const SafeCustomHeaderValueSchema = z.string().trim().min(1).max(4_096).refine(
  (value) => !/[\r\n]/.test(value) && !/^(?:bearer|basic)\s/i.test(value),
  "custom header value must not contain credentials",
);

export const SafeCustomHeadersSchema = z
  .record(SafeCustomHeaderNameSchema, SafeCustomHeaderValueSchema)
  .refine((headers) => Object.keys(headers).length <= 32, "too many custom headers");

export const ProviderConfigSchema = z
  .strictObject({
    id: z.string().trim().min(1).max(256),
    kind: AiProviderKindSchema,
    label: z.string().trim().min(1).max(256),
    baseUrl: ProviderBaseUrlSchema,
    model: z.string().trim().min(1).max(256),
    apiKeyCredentialRef: z.string().trim().min(1).max(512).optional(),
    enabled: z.boolean(),
    customHeaders: SafeCustomHeadersSchema.optional(),
    maxInputTokens: z.number().int().positive().max(10_000_000).optional(),
    wireApi: WireApiSchema.optional(),
    models: z.array(ProviderModelOptionSchema).max(1_000).optional(),
    createdAt: z.string().min(1).max(80),
    updatedAt: z.string().min(1).max(80),
  })
  .refine(wireApiAppliesTo, WIRE_API_IS_OPENAI_ONLY);

export const ProviderSaveInputSchema = z
  .strictObject({
    providerId: z.string().trim().min(1).max(256).regex(/^[a-z0-9][a-z0-9-_]*$/, "providerId must use lowercase letters, numbers, hyphens or underscores").optional(),
    /**
     * Required rather than defaulted: `provider_save` writes the kind into the row, and a
     * save that leaves it out would have to pick one of the three on the user's behalf.
     */
    kind: AiProviderKindSchema,
    label: z.string().trim().max(256).optional(),
    baseUrl: ProviderBaseUrlSchema,
    model: z.string().trim().min(1).max(256),
    apiKey: z.string().min(1).max(4_096).optional(),
    wireApi: WireApiSchema.optional(),
    models: z.array(ProviderModelOptionSchema).max(1_000).optional(),
  })
  .refine(wireApiAppliesTo, WIRE_API_IS_OPENAI_ONLY);
