/**
 * AI provider config schemas: the API key is a *transient* input that Rust drops
 * into the keychain; only the reference is stored.
 */

import { z } from "zod";

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

export const ProviderConfigSchema = z.strictObject({
  id: z.string().trim().min(1).max(256),
  kind: z.literal("openai-compatible"),
  label: z.string().trim().min(1).max(256),
  baseUrl: ProviderBaseUrlSchema,
  model: z.string().trim().min(1).max(256),
  apiKeyCredentialRef: z.string().trim().min(1).max(512).optional(),
  enabled: z.boolean(),
  customHeaders: SafeCustomHeadersSchema.optional(),
  maxInputTokens: z.number().int().positive().max(10_000_000).optional(),
  wireApi: z.enum(["chat", "responses"]).optional(),
  models: z.array(ProviderModelOptionSchema).max(1_000).optional(),
  createdAt: z.string().min(1).max(80),
  updatedAt: z.string().min(1).max(80),
});

export const ProviderSaveInputSchema = z.strictObject({
  providerId: z.string().trim().min(1).max(256).regex(/^[a-z0-9][a-z0-9-_]*$/, "providerId must use lowercase letters, numbers, hyphens or underscores").optional(),
  label: z.string().trim().max(256).optional(),
  baseUrl: ProviderBaseUrlSchema,
  model: z.string().trim().min(1).max(256),
  apiKey: z.string().min(1).max(4_096).optional(),
  wireApi: z.enum(["chat", "responses"]).optional(),
  models: z.array(ProviderModelOptionSchema).max(1_000).optional(),
});
