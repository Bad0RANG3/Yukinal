/**
 * Schemas for the `filesystem.*` host-backed tools.
 *
 * The shared contract (`@yukinal/shared`) describes the fields both sides already agreed on —
 * the remote-path rule, the read result, the write cap. Two things live here instead, next to the
 * tools that use them:
 *
 *  - the **content revision**, which only the Agent's `filesystem.read` / `filesystem.edit` pair
 *    needs (the UI's remote file browser has no edit path, so its payload does not carry one);
 *  - the **`filesystem.edit`** request and result, which are new.
 *
 * The fields that already exist are reused through `.shape` rather than re-declared, so the path
 * rule and the byte cap have exactly one definition per side of the IPC boundary.
 */

import { FilesystemReadOutputSchema, FilesystemWriteInputSchema } from "@yukinal/shared";
import { z } from "zod";

/**
 * A content revision: the SHA-256 of the bytes, 64 hex characters (see
 * `crates/filesystem/src/revision.rs` for the algorithm, the encoding and what it does *not*
 * cover). Case-insensitive because the host compares it that way: an Agent that re-types the
 * revision in upper case has not changed the file.
 */
export const ContentRevisionSchema = z
  .string()
  .regex(
    /^[0-9a-f]{64}$/i,
    "content revision must be the 64-character hex hash returned by filesystem.read",
  );

/**
 * `filesystem.read`'s result: the shared read output plus the revision the edit guard checks.
 *
 * `revision` describes the bytes that were returned. When `truncated` is `true` those bytes are a
 * prefix of the file, so the revision describes a prefix — the host never lets it authorize an
 * edit, because writing a prefix back is how a file gets truncated.
 */
export const FilesystemReadResultSchema = z.strictObject({
  ...FilesystemReadOutputSchema.shape,
  revision: ContentRevisionSchema,
});

/** One of the two strings an edit carries; both are bounded by the same cap as `write`'s content. */
const EditStringSchema = FilesystemWriteInputSchema.shape.content;

export const FilesystemEditInputSchema = z.strictObject({
  path: FilesystemReadOutputSchema.shape.path,
  expectedRevision: ContentRevisionSchema,
  oldString: EditStringSchema.min(1, "oldString must not be empty"),
  newString: EditStringSchema,
});

export const FilesystemEditResultSchema = z.strictObject({
  path: FilesystemReadOutputSchema.shape.path,
  revision: ContentRevisionSchema,
  bytesBefore: z.number().int().nonnegative(),
  bytesAfter: z.number().int().nonnegative(),
  lineDelta: z.number().int(),
});
