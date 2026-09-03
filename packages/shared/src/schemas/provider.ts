/**
 * AI provider config schemas: the API key is a *transient* input that Rust drops
 * into the keychain; only the reference is stored.
 */

import { z } from "zod";

export const ProviderConfigSchema = z.object({
  id: z.string().min(1),
  kind: z.literal("openai-compatible"),
  label: z.string().min(1),
  baseUrl: z.string().min(1),
  model: z.string().min(1),
  apiKeyCredentialRef: z.string().optional(),
  enabled: z.boolean(),
  customHeaders: z.record(z.string(), z.string()).optional(),
  maxInputTokens: z.number().int().positive().optional(),
  createdAt: z.string(),
  updatedAt: z.string(),
});

export const ProviderSaveInputSchema = z.object({
  label: z.string().optional(),
  baseUrl: z.string().min(1),
  model: z.string().min(1),
  apiKey: z.string().min(1).optional(),
});