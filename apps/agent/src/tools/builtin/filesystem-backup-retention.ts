/**
 * `filesystem.backup.retention` — plan a cross-task backup rotation from the host ledger.
 *
 * Read-only and local to the host: it never opens an SSH session, never deletes, and never
 * proves that a candidate's remote sibling still exists. Its output is the exact item list a
 * `backup_rotation` playbook can propose; the deletion itself is a separate, always-approved
 * `filesystem.backup.cleanup` step (ADR 0076).
 */

import {
  FilesystemBackupRetentionInputSchema,
  FilesystemBackupRetentionOutputSchema,
} from "@yukinal/shared";

import { hostBackedTool, type HostToolExecutor } from "./host-backed.js";

export function filesystemBackupRetentionTool(host: HostToolExecutor) {
  return hostBackedTool(host, {
    name: "filesystem.backup.retention",
    description:
      "Inspect every available backup this host created for the target server, across tasks, " +
      "and return the exact backups a retention policy would remove. Provide pathPrefix and " +
      "at least one of keepLatest (keep the newest N per original path) or olderThanDays; when " +
      "both are given a record must be beyond keepLatest AND older than the cutoff. The result " +
      "is bounded ledger metadata only: an \"available\" row is not proof that the remote file " +
      "still exists, and this tool never deletes anything.",
    risk: "read",
    timeoutMs: 10_000,
    input: FilesystemBackupRetentionInputSchema,
    output: FilesystemBackupRetentionOutputSchema,
  });
}
