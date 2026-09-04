import { z } from "zod";

const RemoteAbsolutePathSchema = z
  .string()
  .min(1)
  .max(4096)
  .regex(/^\//, "remote path must be absolute");

export const FilesystemReadInputSchema = z.strictObject({
  path: RemoteAbsolutePathSchema,
  maxBytes: z.number().int().min(1).max(1024 * 1024).optional(),
});

export const FilesystemReadOutputSchema = z.strictObject({
  path: RemoteAbsolutePathSchema,
  content: z.string(),
  truncated: z.boolean(),
});

export const FilesystemWriteInputSchema = z.strictObject({
  path: RemoteAbsolutePathSchema,
  content: z.string().max(512 * 1024),
});

export const FilesystemWriteOutputSchema = z.strictObject({
  path: RemoteAbsolutePathSchema,
  bytesWritten: z.number().int().nonnegative(),
});
