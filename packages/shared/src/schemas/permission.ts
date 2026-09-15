/**
 * Schemas for permission, tools and agent runs. See `types/risk.ts` for the model.
 */

import { z } from "zod";

import {
  AGENT_AUDIO_MEDIA_TYPES,
  AGENT_DOCUMENT_MEDIA_TYPES,
  AGENT_IMAGE_MEDIA_TYPES,
  AGENT_PROMPT_LIMITS,
  APPROVAL_DECISIONS,
  type AgentPromptPart,
} from "../types/chat.js";
import { AGENT_PERMISSION_MODES, AGENT_RUN_MODES, PERMISSION_APPROVAL_SOURCES, PERMISSION_MODES, PERMISSION_TIERS } from "../types/risk.js";
import { TOOL_EXECUTION_STATUSES } from "../types/enums.js";
import { EnvironmentSchema, RiskLevelSchema, SERVER_ID_SCHEMA, ToolTargetSchema } from "./server.js";
import {
  ApiVersionSchema,
  HttpBaseUrlSchema,
  SafeCustomHeadersSchema,
  AiProviderKindSchema,
  WireApiSchema,
  apiVersionAppliesTo,
  wireApiAppliesTo,
} from "./provider.js";

/**
 * Per-run provider material (mirrors `RuntimeProviderConfig`).
 *
 * 「哪些 kind 能带 `wireApi`」这条规则与保存路径共用同一份实现，而不是各自再写一遍：
 * 这是同一件事实（`wireApi` 只属于 openai-compatible），分成两份就会分叉。
 */
export const RuntimeProviderConfigSchema = z
  .strictObject({
    kind: AiProviderKindSchema,
    baseUrl: HttpBaseUrlSchema,
    model: z.string().trim().min(1).max(256),
    apiKey: z.string().max(4096).optional(),
    customHeaders: SafeCustomHeadersSchema.optional(),
    apiVersion: ApiVersionSchema.optional(),
    timeoutMs: z.number().int().min(100).max(10 * 60_000).optional(),
    wireApi: WireApiSchema.optional(),
  })
  .refine(wireApiAppliesTo, {
    message: 'wireApi only applies to kind "openai-compatible"; native kinds have no dialect axis',
    path: ["wireApi"],
  })
  .refine(apiVersionAppliesTo, {
    message: 'apiVersion only applies to kind "anthropic"; other protocols do not use the Anthropic version header',
    path: ["apiVersion"],
  });

export const PermissionTierSchema = z.enum(PERMISSION_TIERS);
export const PermissionModeSchema = z.enum(PERMISSION_MODES);
export const AgentPermissionModeSchema = z.enum(AGENT_PERMISSION_MODES);
export const AgentRunModeSchema = z.enum(AGENT_RUN_MODES);
export const PermissionApprovalSourceSchema = z.enum(PERMISSION_APPROVAL_SOURCES);

export const PermissionPolicySchema = z.object({
  id: z.string().min(1),
  name: z.string().min(1),
  environment: EnvironmentSchema,
  tiers: z.object({
    read: PermissionModeSchema,
    write: PermissionModeSchema,
    dangerous: PermissionModeSchema,
  }),
  builtin: z.boolean(),
});

/** Facts are auditable input to the decision; they can come from code or from rules. */
export const RiskFactSchema = z.discriminatedUnion("source", [
  z.object({
    source: z.literal("tool"),
    level: RiskLevelSchema,
    toolName: z.string().min(1),
    note: z.string().optional(),
  }),
  z.object({
    source: z.literal("command"),
    level: RiskLevelSchema,
    command: z.string(),
    matched: z.array(z.string()),
    note: z.string().optional(),
  }),
  z.object({
    source: z.literal("environment"),
    level: RiskLevelSchema,
    environment: EnvironmentSchema,
    note: z.string().optional(),
  }),
]);

export const PermissionDecisionSchema = z.object({
  outcome: PermissionModeSchema,
  intrinsicRisk: RiskLevelSchema,
  finalRisk: RiskLevelSchema,
  tier: PermissionTierSchema,
  facts: z.array(RiskFactSchema),
  policyId: z.string(),
  toolName: z.string().min(1),
  reason: z.string(),
  approvedBy: PermissionApprovalSourceSchema.optional(),
  target: ToolTargetSchema,
  approvalId: z.string().optional(),
  requestedAt: z.string(),
});

export const RetryPolicySchema = z.object({
  maxAttempts: z.number().int().min(1).max(10),
  backoffMs: z.number().int().min(0),
});

export const ToolOriginSchema = z.discriminatedUnion("kind", [
  z.object({ kind: z.literal("builtin") }),
  z.object({ kind: z.literal("mcp"), serverId: z.string().min(1) }),
  z.object({ kind: z.literal("provider"), providerId: z.string().min(1) }),
]);

/** A tool that cannot describe its risk and timeout must not be registrable. */
export const ToolDeclarationSchema = z.object({
  name: z.string().min(3),
  description: z.string().min(1),
  risk: RiskLevelSchema,
  timeoutMs: z.number().int().positive(),
  cancellable: z.boolean(),
  retry: RetryPolicySchema,
  inputSchema: z.record(z.string(), z.unknown()),
  origin: ToolOriginSchema,
});

export const ToolExecutionStatusSchema = z.enum(TOOL_EXECUTION_STATUSES);

export const ApprovalResponseSchema = z.strictObject({
  approvalId: z.string().min(1),
  runId: z.string().min(1),
  decision: z.enum(APPROVAL_DECISIONS),
  respondedAt: z.string(),
});

const MAX_IMAGE_BASE64_CHARS =
  Math.ceil(AGENT_PROMPT_LIMITS.maxImageBytes / 3) * 4;
const MAX_DOCUMENT_BASE64_CHARS =
  Math.ceil(AGENT_PROMPT_LIMITS.maxDocumentBytes / 3) * 4;

const AgentImageDataSchema = z
  .string()
  .min(4)
  .max(MAX_IMAGE_BASE64_CHARS)
  .regex(/^[A-Za-z0-9+/]*={0,2}$/, "image data must be base64 without a data-URL prefix")
  .refine((value) => value.length % 4 === 0, "image base64 length must be a multiple of four");

const AgentTextPromptPartSchema = z.strictObject({
  type: z.literal("text"),
  text: z.string().min(1),
});

const AgentImagePromptPartSchema = z.strictObject({
  type: z.literal("image"),
  mediaType: z.enum(AGENT_IMAGE_MEDIA_TYPES),
  data: AgentImageDataSchema,
  name: z.string().trim().min(1).max(AGENT_PROMPT_LIMITS.maxImageNameChars).optional(),
});

const AgentDocumentPromptPartSchema = z.strictObject({
  type: z.literal("document"),
  mediaType: z.enum(AGENT_DOCUMENT_MEDIA_TYPES),
  data: z
    .string()
    .min(4)
    .max(MAX_DOCUMENT_BASE64_CHARS)
    .regex(/^[A-Za-z0-9+/]*={0,2}$/, "document data must be base64 without a data-URL prefix")
    .refine(
      (value) => value.length % 4 === 0,
      "document base64 length must be a multiple of four",
    ),
  name: z
    .string()
    .trim()
    .min(1)
    .max(AGENT_PROMPT_LIMITS.maxDocumentNameChars)
    .refine(
      (value) => !/[\\/\u0000-\u001F\u007F]/.test(value),
      "document name must not contain path separators or control characters",
    ),
});

const AgentTextFilePromptPartSchema = z.strictObject({
  type: z.literal("file"),
  mediaType: z.literal("text/plain"),
  data: z
    .string()
    .min(1)
    .refine(
      (value) => new TextEncoder().encode(value).byteLength <= AGENT_PROMPT_LIMITS.maxFileBytes,
      `text file must be at most ${AGENT_PROMPT_LIMITS.maxFileBytes} UTF-8 bytes`,
    )
    .refine(
      (value) => !/[\u0000-\u0008\u000B\u000C\u000E-\u001F\u007F]/.test(value),
      "text file must not contain binary control characters",
    ),
  name: z
    .string()
    .trim()
    .min(1)
    .max(AGENT_PROMPT_LIMITS.maxFileNameChars)
    .refine(
      (value) => !/[\\/\u0000-\u001F\u007F]/.test(value),
      "text file name must not contain path separators or control characters",
    ),
});

/**
 * Base64 length that could still decode to `maxAudioBytes` plus its padding.
 *
 * Checked before the decoded-bytes refinement so an oversized payload fails on length alone:
 * decoding a hostile 8 MiB string to prove it is too big is work nobody asked for.
 */
const MAX_AUDIO_BASE64_CHARS =
  Math.ceil(AGENT_PROMPT_LIMITS.maxAudioBytes / 3) * 4 + 4;

const AgentAudioPromptPartSchema = z.strictObject({
  type: z.literal("audio"),
  mediaType: z.enum(AGENT_AUDIO_MEDIA_TYPES),
  data: z
    .string()
    .min(4)
    .max(MAX_AUDIO_BASE64_CHARS)
    .regex(/^[A-Za-z0-9+/]*={0,2}$/, "audio data must be base64 without a data-URL prefix")
    .refine(
      (value) => value.length % 4 === 0,
      "audio base64 length must be a multiple of four",
    )
    .refine(
      (value) => decodedBase64Bytes(value) <= AGENT_PROMPT_LIMITS.maxAudioBytes,
      `each audio clip must be at most ${AGENT_PROMPT_LIMITS.maxAudioBytes} decoded bytes`,
    ),
  name: z
    .string()
    .trim()
    .min(1)
    .max(AGENT_PROMPT_LIMITS.maxAudioNameChars)
    .refine(
      (value) => !/[\\/\u0000-\u001F\u007F]/.test(value),
      "audio name must not contain path separators or control characters",
    )
    .optional(),
});

function decodedBase64Bytes(value: string): number {
  const padding = value.endsWith("==") ? 2 : value.endsWith("=") ? 1 : 0;
  return (value.length / 4) * 3 - padding;
}

export const AgentPromptPartsSchema = z
  .array(
    z.discriminatedUnion("type", [
      AgentTextPromptPartSchema,
      AgentImagePromptPartSchema,
      AgentTextFilePromptPartSchema,
      AgentDocumentPromptPartSchema,
      AgentAudioPromptPartSchema,
    ]),
  )
  .min(1)
  .max(AGENT_PROMPT_LIMITS.maxParts)
  .superRefine((parts, context) => {
    const textChars = parts
      .filter((part) => part.type === "text")
      .reduce((sum, part) => sum + part.text.length, 0);
    if (textChars > AGENT_PROMPT_LIMITS.maxTextChars) {
      context.addIssue({
        code: "custom",
        message: `prompt text parts may total at most ${AGENT_PROMPT_LIMITS.maxTextChars} characters`,
      });
    }
    const images = parts.filter((part) => part.type === "image");
    if (images.length > AGENT_PROMPT_LIMITS.maxImages) {
      context.addIssue({
        code: "custom",
        message: `a message may contain at most ${AGENT_PROMPT_LIMITS.maxImages} images`,
      });
      return;
    }
    const documents = parts.filter((part) => part.type === "document");
    if (documents.length > AGENT_PROMPT_LIMITS.maxDocuments) {
      context.addIssue({
        code: "custom",
        message: `a message may contain at most ${AGENT_PROMPT_LIMITS.maxDocuments} PDF documents`,
      });
      return;
    }
    const audio = parts.filter((part) => part.type === "audio");
    if (audio.length > AGENT_PROMPT_LIMITS.maxAudios) {
      context.addIssue({
        code: "custom",
        message: `a message may contain at most ${AGENT_PROMPT_LIMITS.maxAudios} audio clips`,
      });
      return;
    }
    const totalInlineBytes = [...images, ...documents, ...audio].reduce(
      (sum, part) => sum + decodedBase64Bytes(part.data),
      0,
    );
    if (totalInlineBytes > AGENT_PROMPT_LIMITS.maxTotalInlineBytes) {
      context.addIssue({
        code: "custom",
        message: `images, PDF documents and audio clips may total at most ${AGENT_PROMPT_LIMITS.maxTotalInlineBytes} decoded bytes`,
      });
    }
    const files = parts.filter((part) => part.type === "file");
    if (files.length > AGENT_PROMPT_LIMITS.maxFiles) {
      context.addIssue({
        code: "custom",
        message: `a message may contain at most ${AGENT_PROMPT_LIMITS.maxFiles} text files`,
      });
      return;
    }
    const fileBytes = files.reduce(
      (sum, file) => sum + new TextEncoder().encode(file.data).byteLength,
      0,
    );
    if (fileBytes > AGENT_PROMPT_LIMITS.maxTotalFileBytes) {
      context.addIssue({
        code: "custom",
        message: `text files may total at most ${AGENT_PROMPT_LIMITS.maxTotalFileBytes} UTF-8 bytes`,
      });
    }
  });

export const AgentPromptPartSchema = z.discriminatedUnion("type", [
  AgentTextPromptPartSchema,
  AgentImagePromptPartSchema,
  AgentTextFilePromptPartSchema,
  AgentDocumentPromptPartSchema,
  AgentAudioPromptPartSchema,
]);

/**
 * The sentence both gates carry when a request has nothing to work with. One constant so the
 * IPC gate and the run schema cannot word the same refusal differently.
 */
export const EMPTY_PROMPT_MESSAGE =
  "prompt must contain text, an image, a PDF document, a text file, or an audio clip";

/**
 * Does this request carry anything a model can act on?
 *
 * Exported because **two** schemas need the same answer — `AgentRunRequestSchema` below and the
 * IPC gate for `agent_run_start` in `schemas/ipc.ts` — and two copies of this list had already
 * drifted: adding the audio kind left one of them refusing an audio-only prompt as "empty".
 * A kind added to the vocabulary now lands in both, because there is only one list.
 */
export function agentPromptCarriesContent(request: {
  prompt: string;
  parts?: readonly AgentPromptPart[];
}): boolean {
  if (request.prompt.trim().length > 0) return true;
  return (
    request.parts?.some(
      (part) =>
        part.type === "image" ||
        part.type === "file" ||
        part.type === "document" ||
        part.type === "audio" ||
        (part.type === "text" && part.text.trim().length > 0),
    ) ?? false
  );
}

export const AgentRunRequestSchema = z
  .strictObject({
    runId: z.string().trim().min(1).max(256),
    sessionId: z.string().trim().min(1).max(256),
    prompt: z.string().max(100_000),
    messageId: z.string().trim().min(1).max(256).optional(),
    parts: AgentPromptPartsSchema.optional(),
    delivery: z.enum(["async", "sync"]).optional(),
    resume: z.boolean().optional(),
    workspaceId: z.string().trim().min(1).max(256).optional(),
    focusServerId: SERVER_ID_SCHEMA.optional(),
    target: ToolTargetSchema.optional(),
    /**
     * Deliberately not `z.enum(BUILTIN_POLICY_IDS)`: the *registry* is the authority on
     * which ids exist (`apps/agent/src/permissions/policy-registry.ts`), and it answers an
     * unknown id with an error that names the id and the known ones. Pinning the built-ins
     * here as well would add a second place to change and turn that error into a generic
     * "invalid params". Bounded, because the value crosses into a decision.
     */
    policyId: z.string().trim().min(1).max(256).optional(),
    permissionMode: AgentPermissionModeSchema.optional(),
    /** Omitted -> `goal`, the unconstrained mode. */
    mode: AgentRunModeSchema.optional(),
    providerConfig: RuntimeProviderConfigSchema.optional(),
  })
  .superRefine((request, context) => {
    if (!agentPromptCarriesContent(request)) {
      context.addIssue({
        code: "custom",
        message: EMPTY_PROMPT_MESSAGE,
        path: ["prompt"],
      });
    }
  });
