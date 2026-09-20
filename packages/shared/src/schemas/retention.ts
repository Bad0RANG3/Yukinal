import { z } from "zod";

import {
  RETENTION_ITEM_KINDS,
  RETENTION_ITEM_REASONS,
  RETENTION_SKIP_REASONS,
} from "../types/retention.js";

const OPAQUE_ID = z.string().trim().min(1).max(256);
const TIMESTAMP = z.string().trim().min(1).max(80);

export const RetentionItemSchema = z.strictObject({
  id: OPAQUE_ID,
  taskId: OPAQUE_ID,
  kind: z.enum(RETENTION_ITEM_KINDS),
  createdAt: TIMESTAMP,
  bytes: z.number().int().nonnegative().max(1_048_576),
  reason: z.enum(RETENTION_ITEM_REASONS),
});

export const RetentionSkipSchema = z.strictObject({
  id: OPAQUE_ID,
  kind: z.enum(RETENTION_ITEM_KINDS),
  reason: z.enum(RETENTION_SKIP_REASONS),
});

export const InvestigationRetentionPreviewInputSchema = z.strictObject({
  taskId: OPAQUE_ID,
  cutoffAt: TIMESTAMP.optional(),
  limit: z.number().int().min(1).max(128).optional(),
});

export const InvestigationRetentionPreviewSchema = z.strictObject({
  taskId: OPAQUE_ID,
  cutoffAt: TIMESTAMP,
  candidates: z.array(RetentionItemSchema).max(128),
  protectedCount: z.number().int().nonnegative().max(100_000),
  candidateBytes: z.number().int().nonnegative().max(1_048_576 * 128),
  truncated: z.boolean(),
});

export const InvestigationRetentionPreviewResponseSchema = z.strictObject({
  preview: InvestigationRetentionPreviewSchema,
});

export const InvestigationRetentionPruneItemSchema = z.strictObject({
  id: OPAQUE_ID,
  kind: z.enum(RETENTION_ITEM_KINDS),
});

export const InvestigationRetentionPruneInputSchema = z.strictObject({
  taskId: OPAQUE_ID,
  cutoffAt: TIMESTAMP,
  items: z.array(InvestigationRetentionPruneItemSchema).min(1).max(128),
  confirmation: z.literal("delete_unreferenced"),
});

export const InvestigationRetentionPruneResultSchema = z.strictObject({
  taskId: OPAQUE_ID,
  cutoffAt: TIMESTAMP,
  deleted: z.array(RetentionItemSchema).max(128),
  skipped: z.array(RetentionSkipSchema).max(128),
});

export const InvestigationRetentionPruneResponseSchema = z.strictObject({
  result: InvestigationRetentionPruneResultSchema,
});
