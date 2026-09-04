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
}

export interface FilesystemWriteInput {
  path: string;
  content: string;
}

export interface FilesystemWriteOutput {
  path: string;
  bytesWritten: number;
}
