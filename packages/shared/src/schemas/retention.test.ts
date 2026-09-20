import assert from "node:assert/strict";
import test from "node:test";

import {
  InvestigationRetentionPreviewInputSchema,
  InvestigationRetentionPreviewResponseSchema,
  InvestigationRetentionPruneInputSchema,
  InvestigationRetentionPruneResponseSchema,
} from "./retention.js";

const cutoffAt = "2026-02-01T00:00:00Z";

test("retention preview input is bounded and allows the host default cutoff", () => {
  assert.equal(InvestigationRetentionPreviewInputSchema.safeParse({ taskId: "task_retention" }).success, true);
  assert.equal(
    InvestigationRetentionPreviewInputSchema.safeParse({ taskId: "task_retention", cutoffAt, limit: 128 }).success,
    true,
  );
  assert.equal(
    InvestigationRetentionPreviewInputSchema.safeParse({ taskId: "task_retention", limit: 129 }).success,
    false,
  );
});

test("retention preview response carries candidate metadata and protection counts", () => {
  const parsed = InvestigationRetentionPreviewResponseSchema.safeParse({
    preview: {
      taskId: "task_retention",
      cutoffAt,
      candidates: [
        {
          id: "ev_drop",
          taskId: "task_retention",
          kind: "evidence",
          createdAt: "2026-01-02T00:00:00Z",
          bytes: 20,
          reason: "unreferenced_evidence",
        },
      ],
      protectedCount: 1,
      candidateBytes: 20,
      truncated: false,
    },
  });
  assert.equal(parsed.success, true);
});

test("retention prune requires an explicit confirmation literal", () => {
  const input = {
    taskId: "task_retention",
    cutoffAt,
    items: [{ id: "ev_drop", kind: "evidence" }],
    confirmation: "delete_unreferenced",
  } as const;
  assert.equal(InvestigationRetentionPruneInputSchema.safeParse(input).success, true);
  assert.equal(
    InvestigationRetentionPruneInputSchema.safeParse({ ...input, confirmation: "yes" }).success,
    false,
  );
  assert.equal(
    InvestigationRetentionPruneInputSchema.safeParse({ ...input, items: [{ id: "ev_drop", kind: "unknown" }] }).success,
    false,
  );
});

test("retention prune response separates deleted history from protected skips", () => {
  assert.equal(
    InvestigationRetentionPruneResponseSchema.safeParse({
      result: {
        taskId: "task_retention",
        cutoffAt,
        deleted: [
          {
            id: "ev_drop",
            taskId: "task_retention",
            kind: "evidence",
            createdAt: "2026-01-02T00:00:00Z",
            bytes: 20,
            reason: "unreferenced_evidence",
          },
        ],
        skipped: [{ id: "ev_keep", kind: "evidence", reason: "referenced_by_task_history" }],
      },
    }).success,
    true,
  );
});
