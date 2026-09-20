/** Host-owned, two-step cleanup of terminal investigation history. */

export const RETENTION_ITEM_KINDS = ["evidence", "artifact"] as const;
export type RetentionItemKind = (typeof RETENTION_ITEM_KINDS)[number];

export const RETENTION_ITEM_REASONS = ["unreferenced_evidence", "superseded_artifact"] as const;
export type RetentionItemReason = (typeof RETENTION_ITEM_REASONS)[number];

export const RETENTION_SKIP_REASONS = [
  "not_found",
  "newer_than_cutoff",
  "referenced_by_task_history",
  "artifact_not_superseded",
  "changed_during_prune",
] as const;
export type RetentionSkipReason = (typeof RETENTION_SKIP_REASONS)[number];

export interface InvestigationRetentionItem {
  id: string;
  taskId: string;
  kind: RetentionItemKind;
  createdAt: string;
  bytes: number;
  reason: RetentionItemReason;
}

export interface InvestigationRetentionSkip {
  id: string;
  kind: RetentionItemKind;
  reason: RetentionSkipReason;
}

export interface InvestigationRetentionPreviewInput {
  taskId: string;
  /** Omit to use the host's conservative default retention cutoff. */
  cutoffAt?: string;
  limit?: number;
}

export interface InvestigationRetentionPreview {
  taskId: string;
  cutoffAt: string;
  candidates: InvestigationRetentionItem[];
  protectedCount: number;
  candidateBytes: number;
  truncated: boolean;
}

export interface InvestigationRetentionPruneItem {
  id: string;
  kind: RetentionItemKind;
}

export interface InvestigationRetentionPruneInput {
  taskId: string;
  cutoffAt: string;
  items: InvestigationRetentionPruneItem[];
  /** A separate literal makes the destructive intent visible at the IPC boundary. */
  confirmation: "delete_unreferenced";
}

export interface InvestigationRetentionPruneResult {
  taskId: string;
  cutoffAt: string;
  deleted: InvestigationRetentionItem[];
  skipped: InvestigationRetentionSkip[];
}
