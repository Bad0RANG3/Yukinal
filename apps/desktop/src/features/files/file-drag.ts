import type { LocalPathHandle, RemoteFileEntry } from "@yukinal/shared";

/** Internal WebView drag payloads carry opaque handles or listed remote paths only. */
export const LOCAL_PATH_HANDLE_MIME = "application/x-yukinal-local-handle";
export const REMOTE_FILE_PATH_MIME = "application/x-yukinal-remote-path";

export function localHandleForDrop(value: string, handles: readonly LocalPathHandle[]): LocalPathHandle | undefined {
  if (!value || value.length > 128) return undefined;
  return handles.find((handle) => handle.handleId === value);
}

export function remoteFileForDrop(value: string, entries: readonly RemoteFileEntry[]): RemoteFileEntry | undefined {
  if (!value || value.length > 4_096) return undefined;
  return entries.find((entry) => entry.type === "file" && entry.path === value);
}
