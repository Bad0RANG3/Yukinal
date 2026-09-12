export const REMOTE_FILE_TYPES = ["file", "directory", "symlink", "other"] as const;
export type RemoteFileType = (typeof REMOTE_FILE_TYPES)[number];

export interface RemoteFileEntry {
  name: string;
  path: string;
  type: RemoteFileType;
  size: number;
}

export interface RemoteFileListResponse { path: string; entries: RemoteFileEntry[]; }
export interface RemoteFileReadResponse { path: string; content: string; truncated: boolean; }

/** Agent-facing remote file tools. The host enforces the same bounds again. */
export interface FilesystemReadInput {
  path: string;
  maxBytes?: number;
}

export interface FilesystemReadOutput {
  path: string;
  content: string;
  truncated: boolean;
  /**
   * 这次读到的**字节**的内容摘要：SHA-256 的 64 位小写十六进制。
   *
   * 它是 `filesystem.edit` 的 `expectedRevision`，也就是「我看到的还是不是我看到的那份」
   * 的凭据。两条必须一起读的性质：
   *
   * - `truncated` 为 `true` 时它描述的是**前缀**，而不是整个文件。宿主不会让这样的摘要
   *   授权一次编辑，所以截断过的读不能直接拿去改。
   * - 它是**内容**摘要，不是版本号：两个不同的写入如果落成同样的字节，摘要相同。宿主
   *   事后可以再校验一次「文件现在是不是还是这个摘要」，但那只是把竞争窗口收窄
   *   （检查与写入之间仍有间隙），不是原子操作。
   */
  revision: string;
}

export interface FilesystemWriteInput {
  path: string;
  content: string;
}

export interface FilesystemWriteOutput {
  path: string;
  bytesWritten: number;
}

/**
 * Agent 侧的读-改-写：拿刚读到的摘要去换一次精确替换。
 *
 * `expectedRevision` 是**唯一**防止覆盖别人改动的机制，所以它不是可选项。宿主在写入前
 * 会比较它与当前文件的摘要；不一致就拒绝，并把两个摘要一起回给模型（`expectedRevision`
 * 与 `actualRevision`），让它重读后再试，而不是替它猜一个折中。
 */
export interface FilesystemEditInput {
  path: string;
  expectedRevision: string;
  /** 必须**恰好一次**出现在文件里；出现零次或多次都拒绝，绝不「取第一个」。 */
  oldString: string;
  newString: string;
}

export interface FilesystemEditOutput {
  path: string;
  /** 替换之后的摘要。 */
  revision: string;
  bytesBefore: number;
  bytesAfter: number;
  /** 行数变化，可正可负。 */
  lineDelta: number;
}
