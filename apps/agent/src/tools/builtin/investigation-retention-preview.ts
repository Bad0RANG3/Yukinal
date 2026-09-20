/** Inspect terminal-task retention candidates without mutating local history. */

import {
  InvestigationRetentionPreviewInputSchema,
  InvestigationRetentionPreviewSchema,
  type HostRetentionPreviewRequest,
  type HostRetentionPreviewResponse,
  type InvestigationRetentionPreview,
} from "@yukinal/shared";
import { z } from "zod";

import { ToolFailure, type Tool } from "../tool.js";

const input = InvestigationRetentionPreviewInputSchema.omit({ taskId: true });

interface RetentionPreviewClient {
  previewRetention(request: HostRetentionPreviewRequest, signal?: AbortSignal): Promise<HostRetentionPreviewResponse>;
}

export function investigationRetentionPreviewTool(
  host: RetentionPreviewClient,
): Tool<z.infer<typeof input>, InvestigationRetentionPreview> {
  return {
    name: "investigation.retention.preview",
    description:
      "Preview local history that could be cleaned from the current terminal investigation. " +
      "The host only reports old unreferenced evidence and superseded artifacts; this is read-only metadata, " +
      "does not delete anything, and remote filesystem backups are outside its scope.",
    risk: "read",
    timeoutMs: 10_000,
    cancellable: true,
    retry: { maxAttempts: 1, backoffMs: 0 },
    input,
    async execute(request, context) {
      if (!context.taskId) {
        throw new ToolFailure("investigation.retention.preview requires a durable task", "invalid_input", false);
      }
      const response = await host.previewRetention({ taskId: context.taskId, ...request }, context.signal);
      if (response.status === "success") return InvestigationRetentionPreviewSchema.parse(response.preview);
      throw new ToolFailure(response.error.message, response.error.code, response.error.retryable, response.error.detail);
    },
  };
}
