import { useMutation, useQuery } from "@tanstack/react-query";
import { IPC_COMMANDS, type RemoteFileEntry } from "@yukinal/shared";
import { useState } from "react";

import { callDesktop, isDesktopShell } from "../../lib/ipc.js";
import { useWorkspaceStore } from "../../stores/workspace-store.js";

export function RemoteFilesPane() {
  const serverId = useWorkspaceStore((state) => state.selectedServerId);
  const shell = isDesktopShell();
  const [path, setPath] = useState("/");
  const [selectedFile, setSelectedFile] = useState<string | null>(null);
  const files = useQuery({
    queryKey: ["remote-files", serverId, path],
    enabled: shell && Boolean(serverId),
    queryFn: async () => callDesktop(IPC_COMMANDS.remoteFileList, { serverId: serverId as string, path }),
  });
  const read = useMutation({ mutationFn: (filePath: string) => callDesktop(IPC_COMMANDS.remoteFileRead, { serverId: serverId as string, path: filePath }) });
  if (!serverId) return <div className="empty-state page-empty"><h2>选择一台服务器</h2><p>连接服务器后浏览远程文件。</p></div>;
  if (!shell) return <div className="empty-state page-empty"><h2>浏览器预览模式</h2><p>远程文件需要 Tauri 原生连接。</p></div>;

  const entries = files.data?.entries ?? [];
  const parent = path === "/" ? "/" : path.replace(/\/+$/, "").split("/").slice(0, -1).join("/") || "/";
  const open = (entry: RemoteFileEntry) => {
    if (entry.type === "directory") { setPath(entry.path); setSelectedFile(null); read.reset(); }
    else { setSelectedFile(entry.path); read.mutate(entry.path); }
  };
  return <section className="remote-files-page">
    <div className="files-toolbar"><button type="button" className="secondary-button" onClick={() => setPath(parent)} disabled={path === "/"}>↑ Up</button><input value={path} onChange={(event) => setPath(event.target.value)} onKeyDown={(event) => { if (event.key === "Enter") void files.refetch(); }} aria-label="Remote path" /><button type="button" className="secondary-button" onClick={() => void files.refetch()} disabled={files.isFetching}>↻ Refresh</button></div>
    {files.isError ? <div className="error-panel"><strong>Unable to list directory</strong><p>{files.error instanceof Error ? files.error.message : String(files.error)}</p><button type="button" className="secondary-button" onClick={() => void files.refetch()}>Retry</button></div> : null}
    <div className="files-layout"><div className="file-list-panel"><div className="section-heading"><div><p className="eyebrow">Remote files</p><h3>{path}</h3></div><span className="section-note">{entries.length} items</span></div>{files.isLoading ? <p className="muted-copy">Loading...</p> : entries.length ? <ul className="remote-file-list">{entries.map((entry) => <li key={entry.path}><button type="button" className={`remote-file-row ${selectedFile === entry.path ? "remote-file-row-selected" : ""}`} onClick={() => open(entry)}><span className="remote-file-icon">{entry.type === "directory" ? "▰" : "·"}</span><span className="remote-file-name">{entry.name}</span><span className="remote-file-size">{entry.type === "directory" ? "dir" : formatSize(entry.size)}</span></button></li>)}</ul> : <p className="muted-copy">Directory is empty.</p>}</div><div className="file-preview-panel"><div className="section-heading"><div><p className="eyebrow">Preview</p><h3>{selectedFile ?? "Select a file"}</h3></div>{read.data?.truncated ? <span className="section-note">truncated</span> : null}</div>{read.isPending ? <p className="muted-copy">Reading...</p> : read.isError ? <p className="text-sm text-red-400">{read.error instanceof Error ? read.error.message : String(read.error)}</p> : read.data ? <pre className="file-preview-content">{read.data.content}</pre> : <p className="muted-copy">Choose a file to inspect its contents.</p>}</div></div>
  </section>;
}

function formatSize(size: number): string { if (size < 1024) return `${size} B`; if (size < 1024 * 1024) return `${Math.round(size / 1024)} KB`; return `${(size / (1024 * 1024)).toFixed(1)} MB`; }
