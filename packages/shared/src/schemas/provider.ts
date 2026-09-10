/**
 * AI provider config schemas: the API key is a *transient* input that Rust drops
 * into the keychain; only the reference is stored.
 */

import { z } from "zod";

const ProviderBaseUrlSchema = z.string().trim().min(1).max(2_048).refine((value) => {
  try {
    const url = new URL(value);
    return (url.protocol === "http:" || url.protocol === "https:") && !url.username && !url.password;
  } catch {
    return false;
  }
}, "baseUrl must be an http(s) URL without embedded credentials");

export const ProviderModelOptionSchema = z.strictObject({
  id: z.string().trim().min(1).max(256),
  label: z.string().trim().min(1).max(256),
  contextWindow: z.number().int().positive().max(10_000_000).optional(),
  supportsToolCalling: z.boolean(),
  supportsStreaming: z.boolean(),
});

/**
 * Provider credentials must use apiKey + the OS credential store. Custom headers
 * are limited to non-secret gateway metadata so imports cannot persist another
 * credential channel in SQLite.
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
  providerId: z.string().trim().min(1).max(256).optional(),
  label: z.string().trim().max(256).optional(),
  baseUrl: ProviderBaseUrlSchema,
  model: z.string().trim().min(1).max(256),
  apiKey: z.string().min(1).max(4_096).optional(),
  wireApi: z.enum(["chat", "responses"]).optional(),
  models: z.array(ProviderModelOptionSchema).max(1_000).optional(),
});
export const CcSwitchProviderCandidateSchema = z.strictObject({
  id: z.string().trim().min(1).max(512),
  name: z.string().trim().min(1).max(256),
  baseUrl: ProviderBaseUrlSchema,
  model: z.string().trim().min(1).max(256),
  wireApi: z.enum(["chat", "responses"]),
  hasApiKey: z.boolean(),
  models: z.array(ProviderModelOptionSchema).max(1_000).optional(),
});
