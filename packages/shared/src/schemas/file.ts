import { z } from "zod";

const RemoteAbsolutePathSchema = z
  .string()
  .min(1)
  .max(4096)
  .regex(/^\//, "remote path must be absolute");

/**
 * 内容摘要：SHA-256 的小写十六进制。
 *
 * 大小写不宽松是有意的：`ContentRevisionSchema`（Agent 侧）说了理由 —— 上层的比较是
 * 字节比较，若这里接受大写，同一份内容就会出现两种合法写法，而「大写是否等于没变」这种
 * 问题不该由每个调用点回答。
 */
const ContentRevisionSchema = z
  .string()
  .regex(/^[0-9a-f]{64}$/, "content revision must be the 64-character lowercase hex SHA-256");

export const FilesystemReadInputSchema = z.strictObject({
  path: RemoteAbsolutePathSchema,
  maxBytes: z.number().int().min(1).max(1024 * 1024).optional(),
});

export const FilesystemReadOutputSchema = z.strictObject({
  path: RemoteAbsolutePathSchema,
  content: z.string(),
  truncated: z.boolean(),
  /** 这次返回字节的内容摘要，供 `filesystem.edit` 的 `expectedRevision` 使用。 */
  revision: ContentRevisionSchema,
});

export const FilesystemEditInputSchema = z.strictObject({
  path: RemoteAbsolutePathSchema,
  /**
   * `filesystem.read` 给出的摘要。宿主拿它和**当前**文件比，不一致就拒绝这次编辑，
   * 而不是拿旧内容去覆盖新内容。
   */
  expectedRevision: ContentRevisionSchema,
  oldString: z.string().min(1).max(512 * 1024),
  newString: z.string().max(512 * 1024),
});

export const FilesystemEditOutputSchema = z.strictObject({
  path: RemoteAbsolutePathSchema,
  /** 替换后的内容摘要，让模型可以连续改同一个文件而不必重新读一遍。 */
  revision: ContentRevisionSchema,
  bytesBefore: z.number().int().nonnegative(),
  bytesAfter: z.number().int().nonnegative(),
  /**
   * 行数变化，可正可负。它存在是因为「改了 3 个字节」和「删了一整段」对下一步判断的
   * 意义完全不同，而工具返回的小结是模型唯一的依据。
   */
  lineDelta: z.number().int(),
});

export const FilesystemWriteInputSchema = z.strictObject({
  path: RemoteAbsolutePathSchema,
  content: z.string().max(512 * 1024),
});

export const FilesystemWriteOutputSchema = z.strictObject({
  path: RemoteAbsolutePathSchema,
  bytesWritten: z.number().int().nonnegative(),
});
