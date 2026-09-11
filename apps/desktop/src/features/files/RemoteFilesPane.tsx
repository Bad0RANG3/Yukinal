import { useQuery } from "@tanstack/react-query";
import { IPC_COMMANDS, type RemoteFileEntry } from "@yukinal/shared";
import { useState } from "react";

import { errorMessage } from "../../lib/format.js";
import { Icon } from "../../components/Icon.js";
import { callDesktop, isDesktopShell } from "../../lib/ipc.js";
import { useWorkspaceStore } from "../../stores/workspace-store.js";

export function RemoteFilesPane() {
  const serverId = useWorkspaceStore((state) => state.selectedServerId);
  const shell = isDesktopShell();
  const [path, setPath] = useState("/");
  const [draftPath, setDraftPath] = useState("/");
  const [selectedFile, setSelectedFile] = useState<string | null>(null);
  const files = useQuery({
    queryKey: ["remote-files", serverId, path],
    enabled: shell && Boolean(serverId),
    queryFn: async () => callDesktop(IPC_COMMANDS.remoteFileList, { serverId: serverId as string, path }),
  });
  const read = useQuery({
    queryKey: ["remote-file", serverId, selectedFile],
    enabled: shell && Boolean(serverId && selectedFile),
    queryFn: () => callDesktop(IPC_COMMANDS.remoteFileRead, { serverId: serverId as string, path: selectedFile as string }),
  });

  if (!serverId) return <div className="empty-state page-empty"><Icon name="folder" size="xl" /><h2>选择一台服务器</h2><p>连接服务器后浏览远程文件。</p></div>;
  if (!shell) return <div className="empty-state page-empty"><Icon name="folder" size="xl" /><h2>浏览器预览模式</h2><p>远程文件需要 Tauri 原生连接。</p></div>;

  const entries = files.data?.entries ?? [];
  const parent = path === "/" ? "/" : path.replace(/\/+$/, "").split("/").slice(0, -1).join("/") || "/";
  const navigate = (nextPath: string) => {
    const normalized = nextPath.trim().replace(/\/{2,}/g, "/").replace(/\/$/, "") || "/";
    setDraftPath(normalized);
    setPath(normalized);
    setSelectedFile(null);
    if (normalized === path) void files.refetch();
  };
  const open = (entry: RemoteFileEntry) => {
    if (entry.type === "directory") navigate(entry.path);
    else setSelectedFile(entry.path);
  };

  return (
    <section className="remote-files-page">
      <form className="files-toolbar" onSubmit={(event) => { event.preventDefault(); navigate(draftPath); }}>
        <button type="button" className="button-secondary" onClick={() => navigate(parent)} disabled={path === "/"} title="返回上级目录" aria-label="返回上级目录"><Icon name="arrowUp" size="sm" /></button>
        <input className="form-input files-path-input" value={draftPath} onChange={(event) => setDraftPath(event.target.value)} aria-label="远程路径" title="输入绝对路径，按 Enter 打开" pattern="/.*" required spellCheck={false} />
        <button type="submit" className="button-secondary">前往</button>
        <button type="button" className="button-secondary" onClick={() => void files.refetch()} disabled={files.isFetching} title="刷新远程文件" aria-label="刷新远程文件"><Icon name="refresh" size="sm" />刷新</button>
      </form>
      {files.isError ? <div className="error-panel"><div><strong>无法读取目录</strong><p>{errorMessage(files.error)}</p></div><button type="button" className="button-secondary" onClick={() => void files.refetch()}>重试</button></div> : null}
      <div className="files-layout">
        <div className="file-list-panel">
          <div className="section-heading"><div><p className="eyebrow">远程文件</p><h2>{path}</h2></div><span className="section-note">{entries.length} 项</span></div>
          {files.isLoading ? <p className="muted-copy" role="status">正在加载…</p> : entries.length ? <ul className="remote-file-list">{entries.map((entry) => <li key={entry.path}><button type="button" className={`remote-file-row ${selectedFile === entry.path ? "remote-file-row-selected" : ""}`} aria-current={selectedFile === entry.path ? "true" : undefined} onClick={() => open(entry)}><span className="remote-file-icon"><Icon name={entry.type === "directory" ? "folder" : "file"} size="md" /></span><span className="remote-file-name">{entry.name}</span><span className="remote-file-size">{entry.type === "directory" ? "目录" : formatSize(entry.size)}</span></button></li>)}</ul> : files.isSuccess ? <p className="muted-copy">目录为空。</p> : null}
        </div>
        <div className="file-preview-panel">
          <div className="section-heading"><div><p className="eyebrow">预览</p><h2>{selectedFile ?? "选择文件"}</h2></div>{read.data?.truncated ? <span className="section-note">内容已截断</span> : null}</div>
          {!selectedFile ? <div className="file-preview-empty"><Icon name="file" size="xxl" /><p>选择文件查看内容</p><small>目录与文件预览会分别显示在这里。</small></div> : read.isLoading ? <p className="muted-copy" role="status">正在读取…</p> : read.isError ? <div role="alert"><p className="error-copy">{read.error.message}</p><button type="button" className="button-secondary" onClick={() => void read.refetch()}>重新读取</button></div> : read.data ? <pre className="file-preview-content">{read.data.content || "（空文件）"}</pre> : null}
        </div>
      </div>
    </section>
  );
}

function formatSize(size: number): string {
  if (size < 1024) return `${size} B`;
  if (size < 1024 * 1024) return `${Math.round(size / 1024)} KB`;
  return `${(size / (1024 * 1024)).toFixed(1)} MB`;
}
