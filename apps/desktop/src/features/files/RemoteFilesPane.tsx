import { useQuery, useQueryClient } from "@tanstack/react-query";
import {
  IPC_COMMANDS,
  type FilePreviewResponse,
  type LocalPathHandle,
  type RemoteFileEntry,
  type TransferConflictAction,
  type TransferSnapshot,
} from "@yukinal/shared";
import { useEffect, useId, useMemo, useState, type DragEvent, type ReactNode } from "react";

import { EmptyPanel, ErrorPanel } from "../../components/PanelStates.js";
import { Icon } from "../../components/Icon.js";
import { errorMessage, formatBytes } from "../../lib/format.js";
import { callDesktop, isDesktopShell, subscribeDesktop } from "../../lib/ipc.js";
import { useWorkspaceStore } from "../../stores/workspace-store.js";
import { LOCAL_PATH_HANDLE_MIME, localHandleForDrop, REMOTE_FILE_PATH_MIME, remoteFileForDrop } from "./file-drag.js";
import { transferProgress } from "./transfer-progress.js";

const ACTIVE_STATUSES = new Set(["queued", "running", "waitingConflict"]);
const TRANSFER_STATUS_LABEL: Record<TransferSnapshot["status"], string> = { queued: "排队中", running: "传输中", waitingConflict: "等待处理", completed: "已完成", partial: "部分完成", failed: "失败", cancelled: "已取消", interrupted: "已中断" };

export function RemoteFilesPane({ serverId: explicitServerId }: { serverId?: string | null } = {}) {
  const workspaceServerId = useWorkspaceStore((state) => state.selectedServerId);
  const serverId = explicitServerId === undefined ? workspaceServerId : explicitServerId;
  const shell = isDesktopShell();
  const queryClient = useQueryClient();
  const [path, setPath] = useState("/");
  const [draftPath, setDraftPath] = useState("/");
  const [selectedRemote, setSelectedRemote] = useState<RemoteFileEntry | null>(null);
  const [localHandles, setLocalHandles] = useState<LocalPathHandle[]>([]);
  const [selectedLocal, setSelectedLocal] = useState<LocalPathHandle | null>(null);
  const [downloadTarget, setDownloadTarget] = useState<LocalPathHandle | null>(null);
  const [preparedDrag, setPreparedDrag] = useState<{ path: string; dragId: string; name: string } | null>(null);
  const [preparingDragPath, setPreparingDragPath] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [conflictName, setConflictName] = useState("");
  const [dropTarget, setDropTarget] = useState<"remote" | "local" | null>(null);

  const files = useQuery({
    queryKey: ["remote-files", serverId, path],
    enabled: shell && Boolean(serverId),
    retry: false,
    refetchOnWindowFocus: false,
    refetchOnReconnect: false,
    queryFn: () => callDesktop(IPC_COMMANDS.remoteFileList, { serverId: serverId as string, path }),
  });
  const remotePreview = useQuery({
    queryKey: ["remote-file-preview", serverId, selectedRemote?.path],
    enabled: shell && Boolean(serverId && selectedRemote?.type === "file"),
    retry: false,
    refetchOnWindowFocus: false,
    refetchOnReconnect: false,
    queryFn: () => callDesktop(IPC_COMMANDS.remoteFilePreview, { serverId: serverId as string, path: selectedRemote?.path as string }),
  });
  const localPreview = useQuery({
    queryKey: ["local-file-preview", selectedLocal?.handleId],
    enabled: shell && selectedLocal?.kind === "file",
    queryFn: () => callDesktop(IPC_COMMANDS.localFilePreview, { handleId: selectedLocal?.handleId as string }),
  });
  const transfers = useQuery({
    queryKey: ["file-transfers", serverId],
    enabled: shell && Boolean(serverId),
    queryFn: async () => (await callDesktop(IPC_COMMANDS.fileTransferList, { serverId: serverId as string })).transfers,
    refetchInterval: (query) => (query.state.data ?? []).some((transfer) => ACTIVE_STATUSES.has(transfer.status)) ? 1500 : false,
  });

  useEffect(() => {
    const localDrop = subscribeDesktop("file.local_dropped", (event) => {
      if (event.handles.length) {
        setLocalHandles((current) => [...current, ...event.handles]);
        setSelectedLocal(event.handles[0] ?? null);
        setSelectedRemote(null);
      }
      if (event.rejectedCount) setError(event.rejectedCount + " 个项目不能作为普通文件或目录读取，已跳过。");
    });
    const transferUpdates = subscribeDesktop("file.transfer_updated", ({ transfer }) => {
      queryClient.setQueryData<TransferSnapshot[]>(["file-transfers", transfer.serverId], (current = []) => {
        const found = current.some((item) => item.transferId === transfer.transferId);
        return found ? current.map((item) => item.transferId === transfer.transferId ? transfer : item) : [transfer, ...current];
      });
      if (transfer.status === "completed" || transfer.status === "partial" || transfer.status === "failed") {
        void queryClient.invalidateQueries({ queryKey: ["remote-files", transfer.serverId] });
      }
    });
    return () => { localDrop.stop(); transferUpdates.stop(); };
  }, [queryClient]);

  const entries = files.data?.entries ?? [];
  const queuedCount = localHandles.length;
  const activeTransfers = useMemo(() => (transfers.data ?? []).filter((transfer) => ACTIVE_STATUSES.has(transfer.status)), [transfers.data]);
  const parent = path === "/" ? "/" : path.replace(/\/+$/, "").split("/").slice(0, -1).join("/") || "/";

  if (!serverId) return <EmptyPanel icon="folder" title="选择一台服务器" body="连接服务器后浏览远程文件并建立传输。" />;
  if (!shell) return <EmptyPanel icon="folder" title="浏览器预览模式" body="本机文件与 SFTP 传输需要 Yukinal 桌面应用。" />;

  const navigate = (nextPath: string) => {
    const normalized = nextPath.trim().replace(/\/{2,}/g, "/").replace(/\/$/, "") || "/";
    setDraftPath(normalized);
    setPath(normalized);
    setSelectedRemote(null);
    setSelectedLocal(null);
    if (normalized === path) void files.refetch();
  };
  const open = (entry: RemoteFileEntry) => {
    if (entry.type === "directory") { setPreparedDrag(null); navigate(entry.path); }
    else { setSelectedLocal(null); setSelectedRemote(entry); if (preparedDrag?.path !== entry.path) setPreparedDrag(null); }
  };
  const pickLocal = async (kind: "uploadFiles" | "uploadDirectory" | "downloadDirectory") => {
    setError(null);
    try {
      const response = await callDesktop(IPC_COMMANDS.localPathPick, { kind });
      if (kind === "downloadDirectory") {
        const handle = response.handles[0];
        if (handle) {
          setDownloadTarget(handle);
          if (selectedRemote?.type === "file") await startDownloadPaths([selectedRemote.path], handle);
        }
      } else if (response.handles.length) {
        setLocalHandles((current) => [...current, ...response.handles]);
        setSelectedLocal(response.handles[0] ?? null);
        setSelectedRemote(null);
      }
    } catch (reason) { setError(errorMessage(reason)); }
  };
  const startUpload = async (handles = localHandles) => {
    if (!serverId || !handles.length) return;
    setError(null);
    try {
      await callDesktop(IPC_COMMANDS.fileTransferUpload, {
        serverId, remoteDirectory: path, sourceHandleIds: handles.map((handle) => handle.handleId),
      });
      setLocalHandles((current) => current.filter((handle) => !handles.some((selected) => selected.handleId === handle.handleId)));
      if (selectedLocal && handles.some((handle) => handle.handleId === selectedLocal.handleId)) setSelectedLocal(null);
      await queryClient.invalidateQueries({ queryKey: ["file-transfers", serverId] });
    } catch (reason) { setError(errorMessage(reason)); }
  };
  const startDownloadPaths = async (remotePaths: string[], destination: LocalPathHandle) => {
    if (!serverId || !remotePaths.length) return;
    setError(null);
    try {
      await callDesktop(IPC_COMMANDS.fileTransferDownload, {
        serverId, remotePaths, destinationHandleId: destination.handleId,
      });
      await queryClient.invalidateQueries({ queryKey: ["file-transfers", serverId] });
    } catch (reason) { setError(errorMessage(reason)); }
  };
  const prepareRemoteDrag = async () => {
    if (!serverId || selectedRemote?.type !== "file") return;
    setError(null);
    setPreparingDragPath(selectedRemote.path);
    try {
      const prepared = await callDesktop(IPC_COMMANDS.filePrepareRemoteDrag, { serverId, path: selectedRemote.path });
      setPreparedDrag({ path: selectedRemote.path, dragId: prepared.dragId, name: prepared.name });
    } catch (reason) { setError(errorMessage(reason)); }
    finally { setPreparingDragPath(null); }
  };
  const beginNativeDrag = (dragId: string) => {
    void callDesktop(IPC_COMMANDS.fileDragOutStart, { dragId })
      .then(() => setPreparedDrag(null))
      .catch((reason) => setError(errorMessage(reason)));
  };
  const refreshTransfers = () => queryClient.invalidateQueries({ queryKey: ["file-transfers", serverId] });
  const resolveConflict = async (transfer: TransferSnapshot, action: TransferConflictAction) => {
    const conflict = transfer.activeConflict;
    if (!conflict) return;
    setError(null);
    try {
      await callDesktop(IPC_COMMANDS.fileTransferResolveConflict, { transferId: transfer.transferId, action });
      setConflictName("");
      await refreshTransfers();
    } catch (reason) { setError(errorMessage(reason)); }
  };
  const transferForRemoteDrag = (event: DragEvent) => {
    const entry = remoteFileForDrop(event.dataTransfer.getData(REMOTE_FILE_PATH_MIME), entries);
    if (!entry || !serverId) return;
    if (!downloadTarget) { setError("请先选择本机下载目录，再拖入远程文件。"); return; }
    void startDownloadPaths([entry.path], downloadTarget);
  };
  const transferLocalHandleForDrop = (event: DragEvent) => {
    const handle = localHandleForDrop(event.dataTransfer.getData(LOCAL_PATH_HANDLE_MIME), localHandles);
    setDropTarget(null);
    if (handle) void startUpload([handle]);
  };
  const transferRows = [...(transfers.data ?? [])].sort((left, right) => Number(Boolean(right.activeConflict)) - Number(Boolean(left.activeConflict)) || Number(ACTIVE_STATUSES.has(right.status)) - Number(ACTIVE_STATUSES.has(left.status)));

  return (
    <section className="remote-files-page">
      <form className="files-toolbar" onSubmit={(event) => { event.preventDefault(); navigate(draftPath); }}>
        <div className="files-location">
        <button type="button" className="icon-button" onClick={() => navigate(parent)} disabled={path === "/"} aria-label="返回上级目录"><Icon name="arrowUp" size="sm" /></button>
        <div className="files-address"><Icon name="folder" size="sm" /><input className="files-path-input" value={draftPath} onChange={(event) => setDraftPath(event.target.value)} aria-label="远程路径" pattern="/.*" required spellCheck={false} /><button type="submit" className="icon-button" aria-label="前往路径"><Icon name="chevronRight" size="sm" /></button></div>
        <button type="button" className="icon-button" onClick={() => void files.refetch()} disabled={files.isFetching} aria-label="刷新远程文件"><Icon name="refresh" size="sm" /></button>
        </div>
        <div className="files-upload-actions">
        <button type="button" className="button-secondary" aria-label="选择本机文件" onClick={() => void pickLocal("uploadFiles")}><Icon name="plus" size="sm" />文件</button>
        <button type="button" className="button-secondary" aria-label="选择本机文件夹" onClick={() => void pickLocal("uploadDirectory")}><Icon name="folder" size="sm" />文件夹</button>
        <button type="button" className="button-secondary" disabled={!queuedCount} onClick={() => void startUpload()}><Icon name="arrowUp" size="sm" />上传{queuedCount ? " · " + queuedCount : ""}</button>
        </div>
      </form>
      {error ? <p className="file-transfer-alert" role="alert">{error}</p> : null}
      {files.isError ? <ErrorPanel title="无法读取目录" message={errorMessage(files.error)} onRetry={() => void files.refetch()} /> : null}
      <FilesExplorer path={path} entries={entries} selectedName={selectedRemote?.name ?? selectedLocal?.name ?? null} queuedCount={queuedCount} transferCount={transfers.data?.length ?? 0} activeCount={activeTransfers.length} conflictCount={activeTransfers.filter((transfer) => transfer.activeConflict).length} onClosePreview={() => { setSelectedRemote(null); setSelectedLocal(null); setPreparedDrag(null); }}
        preview={(selectedLocal?.kind === "file" || selectedRemote?.type === "file") ? <>
          <PreviewContent preview={selectedLocal?.kind === "file" ? localPreview.data : remotePreview.data} loading={selectedLocal?.kind === "file" ? localPreview.isLoading : remotePreview.isLoading} error={selectedLocal?.kind === "file" ? localPreview.error : remotePreview.error} onRetry={() => void (selectedLocal?.kind === "file" ? localPreview.refetch() : remotePreview.refetch())} />
          {selectedLocal?.kind === "file" ? <div className="remote-file-actions"><button type="button" className="button-secondary" onClick={() => void startUpload([selectedLocal])}>上传到当前目录</button></div> : null}
          {selectedRemote?.type === "file" ? <div className="remote-file-actions">
            <button type="button" className="button-secondary file-download-button" onClick={() => downloadTarget ? void startDownloadPaths([selectedRemote.path], downloadTarget) : void pickLocal("downloadDirectory")}>下载文件</button>
            <button type="button" className="button-secondary file-download-button" disabled={preparingDragPath === selectedRemote.path} onClick={() => void prepareRemoteDrag()}>{preparingDragPath === selectedRemote.path ? "准备中…" : preparedDrag?.path === selectedRemote.path ? "已就绪，可拖出" : "准备拖到桌面"}</button>
            {preparedDrag?.path === selectedRemote.path ? <small className="file-drag-ready-note">{preparedDrag.name} 已就绪</small> : null}
          </div> : null}
        </> : null}
        localQueue={
          <div className={"local-file-queue" + (dropTarget === "local" ? " file-drop-zone-active" : "")} onDragOver={(event) => { if (event.dataTransfer.types.includes(REMOTE_FILE_PATH_MIME)) { event.preventDefault(); event.dataTransfer.dropEffect = "copy"; setDropTarget("local"); } }} onDragLeave={() => setDropTarget(null)} onDrop={(event) => { event.preventDefault(); setDropTarget(null); transferForRemoteDrag(event); }}>
            <div className="download-target-row"><span>{downloadTarget ? "下载到：" + downloadTarget.name : "本机下载目录"}</span><button type="button" className="button-secondary" onClick={() => void pickLocal("downloadDirectory")}>选择目录</button></div>
            {localHandles.length ? <ul className="local-handle-list">{localHandles.map((handle) => <li key={handle.handleId}><button type="button" draggable className={"local-handle-row" + (selectedLocal?.handleId === handle.handleId ? " remote-file-row-selected" : "")} onDragStart={(event) => { event.dataTransfer.setData(LOCAL_PATH_HANDLE_MIME, handle.handleId); event.dataTransfer.effectAllowed = "copy"; }} onDragEnd={() => setDropTarget(null)} onClick={() => { setSelectedLocal(handle); setSelectedRemote(null); }}><Icon name={handle.kind === "directory" ? "folder" : "file"} size="sm" /><span>{handle.name}</span><small>{handle.kind === "directory" ? "文件夹" : formatBytes(handle.size ?? 0)}</small></button></li>)}</ul> : <p className="file-drop-hint">暂无本机文件</p>}
          </div>
        }
        transfers={<section className="file-transfers-section" aria-label="文件传输任务">
          <div className="section-heading"><span>{activeTransfers.length ? `${activeTransfers.length} 项进行中` : "传输记录"}</span><button type="button" className="icon-button" aria-label="刷新传输记录" onClick={() => void refreshTransfers()}><Icon name="refresh" size="sm" /></button></div>
          {activeTransfers.length === 0 && !(transfers.data?.length) ? <p className="muted-copy">暂无传输任务</p> : null}
          <ul className="transfer-list">{transferRows.map((transfer) => <TransferCard key={transfer.transferId} transfer={transfer} conflictName={conflictName} setConflictName={setConflictName} onCancel={() => void callDesktop(IPC_COMMANDS.fileTransferCancel, { transferId: transfer.transferId }).then(refreshTransfers).catch((reason) => setError(errorMessage(reason)))} onResolve={(action) => void resolveConflict(transfer, action)} />)}</ul>
        </section>}
      >{(visibleEntries, filtering) =>
          <div className={"file-drop-zone" + (dropTarget === "remote" ? " file-drop-zone-active" : "")} onDragOver={(event) => { if (event.dataTransfer.types.includes(LOCAL_PATH_HANDLE_MIME)) { event.preventDefault(); event.dataTransfer.dropEffect = "copy"; setDropTarget("remote"); } }} onDragLeave={() => setDropTarget(null)} onDrop={(event) => { event.preventDefault(); transferLocalHandleForDrop(event); }}>
            {files.isLoading ? <div className="workpage-loading" role="status" aria-label="加载文件列表" /> : visibleEntries.length ? <ul className="remote-file-list" aria-label="远程文件">{visibleEntries.map((entry) => <li key={entry.path}><button type="button" draggable={entry.type === "file" && (Boolean(downloadTarget) || preparedDrag?.path === entry.path)} onDragStart={(event) => {
              if (preparedDrag?.path === entry.path) {
                event.preventDefault();
                beginNativeDrag(preparedDrag.dragId);
                return;
              }
              event.dataTransfer.setData(REMOTE_FILE_PATH_MIME, entry.path);
              event.dataTransfer.effectAllowed = "copy";
            }} className={"remote-file-row" + (selectedRemote?.path === entry.path ? " remote-file-row-selected" : "")} aria-current={selectedRemote?.path === entry.path ? "true" : undefined} onClick={() => open(entry)}><span className="remote-file-identity"><span className="remote-file-icon"><Icon name={entry.type === "directory" ? "folder" : entry.type === "symlink" ? "externalLink" : "file"} size="sm" /></span><span className="remote-file-name" title={entry.name}>{entry.name}</span></span><span className="remote-file-kind">{FILE_KIND_LABEL[entry.type]}</span><span className="remote-file-size">{entry.type === "directory" ? "—" : formatBytes(entry.size)}</span></button></li>)}</ul> : files.isSuccess ? <p className="files-empty-result">{filtering ? "没有匹配的文件" : "空目录"}</p> : null}
          </div>
      }</FilesExplorer>
    </section>
  );
}

const FILE_KIND_LABEL = { directory: "文件夹", file: "文件", symlink: "符号链接", other: "其他" } as const;
const FILE_NAME_ORDER = new Intl.Collator("zh-CN", { numeric: true, sensitivity: "base" });
type FileSort = "name" | "type" | "size";

function FilesExplorer({ path, entries, selectedName, queuedCount, transferCount, activeCount, conflictCount, preview, localQueue, transfers, onClosePreview, children }: {
  path: string; entries: readonly RemoteFileEntry[]; selectedName: string | null; queuedCount: number; transferCount: number; activeCount: number; conflictCount: number;
  preview: ReactNode; localQueue: ReactNode; transfers: ReactNode; onClosePreview: () => void;
  children: (visibleEntries: RemoteFileEntry[], filtering: boolean) => ReactNode;
}) {
  const drawerId = useId();
  const [filter, setFilter] = useState("");
  const [sort, setSort] = useState<{ key: FileSort; descending: boolean }>({ key: "name", descending: false });
  const [drawer, setDrawer] = useState<"local" | "transfers" | null>(null);
  useEffect(() => setFilter(""), [path]);
  useEffect(() => { if (activeCount || conflictCount) setDrawer("transfers"); else if (queuedCount) setDrawer("local"); }, [activeCount, conflictCount, queuedCount]);
  const visibleEntries = useMemo(() => {
    const query = filter.trim().toLocaleLowerCase();
    return entries.filter((entry) => !query || entry.name.toLocaleLowerCase().includes(query)).sort((left, right) => {
      const directoryOrder = Number(right.type === "directory") - Number(left.type === "directory");
      if (directoryOrder) return directoryOrder;
      const result = sort.key === "size" ? left.size - right.size : sort.key === "type" ? FILE_NAME_ORDER.compare(FILE_KIND_LABEL[left.type], FILE_KIND_LABEL[right.type]) : FILE_NAME_ORDER.compare(left.name, right.name);
      return (result || FILE_NAME_ORDER.compare(left.name, right.name)) * (sort.descending ? -1 : 1);
    });
  }, [entries, filter, sort]);
  const toggleSort = (key: FileSort) => setSort((current) => ({ key, descending: current.key === key && !current.descending }));
  return <>
    <div className="files-layout" data-preview={Boolean(preview)}>
      <div className="file-list-panel">
        <div className="files-list-toolbar"><span>远程文件 <small>{visibleEntries.length}{filter.trim() ? ` / ${entries.length}` : ""}</small></span><label className="files-filter"><Icon name="search" size="sm" /><input value={filter} onChange={(event) => setFilter(event.target.value)} placeholder="筛选文件" aria-label="筛选当前目录文件" spellCheck={false} /></label></div>
        <div className="files-columns" aria-label="文件列表排序">{(["name", "type", "size"] as const).map((key) => <button key={key} type="button" aria-label={`按${key === "name" ? "名称" : key === "type" ? "类型" : "大小"}排序`} aria-pressed={sort.key === key} data-descending={sort.descending} onClick={() => toggleSort(key)}>{key === "name" ? "名称" : key === "type" ? "类型" : "大小"}{sort.key === key ? <Icon name="arrowUp" size="xs" /> : null}</button>)}</div>
        {children(visibleEntries, Boolean(filter.trim()))}
      </div>
      {preview ? <aside className="file-preview-panel" aria-label="文件预览"><header className="files-preview-heading"><span title={selectedName ?? undefined}>{selectedName}</span><button type="button" className="icon-button" aria-label="关闭文件预览" onClick={(event) => { const root = event.currentTarget.closest(".remote-files-page"); (root?.querySelector<HTMLButtonElement>(".remote-file-row-selected, .local-handle-row.remote-file-row-selected") ?? root?.querySelector<HTMLInputElement>(".files-filter input"))?.focus(); onClosePreview(); }}><Icon name="close" size="sm" /></button></header><div className="files-preview-body">{preview}</div></aside> : null}
    </div>
    <div className="files-drawer" data-open={Boolean(drawer)}>
      <div className="files-statusbar"><span className="files-item-count">{entries.length} 项</span><div className="files-drawer-tabs">
        <button type="button" aria-expanded={drawer === "local"} aria-controls={`${drawerId}-local`} onClick={() => setDrawer((current) => current === "local" ? null : "local")}><Icon name="folder" size="sm" />本机文件{queuedCount ? ` · ${queuedCount}` : ""}</button>
        <button type="button" aria-expanded={drawer === "transfers"} aria-controls={`${drawerId}-transfers`} onClick={() => setDrawer((current) => current === "transfers" ? null : "transfers")}><Icon name="activity" size="sm" />传输{activeCount ? ` · ${activeCount} 进行中` : ` · ${transferCount}`}{conflictCount ? " · 需处理" : ""}</button>
      </div></div>
      <div id={`${drawerId}-local`} className="files-drawer-content" hidden={drawer !== "local"}>{localQueue}</div>
      <div id={`${drawerId}-transfers`} className="files-drawer-content" hidden={drawer !== "transfers"}>{transfers}</div>
    </div>
  </>;
}

function PreviewContent({ preview, loading, error, onRetry }: { preview?: FilePreviewResponse; loading: boolean; error: unknown; onRetry: () => void }) {
  if (loading) return <p className="muted-copy" role="status">正在读取预览…</p>;
  if (error) return <div role="alert"><p className="error-copy">{errorMessage(error)}</p><button type="button" className="button-secondary" onClick={onRetry}>重新读取</button></div>;
  if (!preview) return null;
  return <div className="file-preview-content-wrap">
    <p className="file-preview-metadata">{formatBytes(preview.size)}{preview.truncated ? " · 预览已截断" : ""}</p>
    {preview.kind === "text" ? <pre className="file-preview-content">{preview.text || "（空文件）"}</pre> : null}
    {preview.kind === "image" && preview.dataUrl ? <img className="file-preview-image" src={preview.dataUrl} alt={preview.name} /> : null}
    {preview.kind === "binary" ? <div className="file-preview-empty"><Icon name="file" size="xxl" /><p>二进制文件</p><small>大小 {formatBytes(preview.size)}。预览不会读取完整文件，使用传输任务复制原始内容。</small></div> : null}
  </div>;
}

function TransferCard({ transfer, conflictName, setConflictName, onCancel, onResolve }: { transfer: TransferSnapshot; conflictName: string; setConflictName: (value: string) => void; onCancel: () => void; onResolve: (action: TransferConflictAction) => void }) {
  const conflict = transfer.activeConflict;
  const percent = transferProgress(transfer);
  const terminal = !ACTIVE_STATUSES.has(transfer.status);
  return <li className="transfer-card">
    <div className="transfer-card-heading"><strong>{transfer.direction === "upload" ? "上传" : "下载"}</strong><span>{transfer.currentItem ?? TRANSFER_STATUS_LABEL[transfer.status]}</span><small>{transfer.completedFiles}/{transfer.totalFiles ?? "?"} 个文件 · {formatBytes(transfer.transferredBytes)}{transfer.totalBytes == null ? "" : " / " + formatBytes(transfer.totalBytes)}</small>{!terminal ? <button type="button" className="button-secondary" onClick={onCancel}>取消</button> : null}</div>
    {!terminal || percent !== undefined ? <progress max={100} value={percent} aria-label={`${transfer.direction === "upload" ? "上传" : "下载"}进度${transfer.currentItem ? `：${transfer.currentItem}` : ""}`} aria-valuetext={`${TRANSFER_STATUS_LABEL[transfer.status]}${percent === undefined ? "" : ` ${percent}%`}`} /> : null}
    {conflict ? <div className="transfer-conflict"><p><strong>目标已存在：</strong>{conflict.targetName}（{formatBytes(conflict.existingSize)}）</p><p>来源：{conflict.sourceName}（{formatBytes(conflict.incomingSize)}）</p><div className="transfer-conflict-actions">{conflict.allowedActions.includes("skip") ? <button type="button" className="button-secondary" onClick={() => onResolve({ action: "skip" })}>跳过</button> : null}{conflict.allowedActions.includes("rename") ? <><input className="form-input" value={conflictName} onChange={(event) => setConflictName(event.target.value)} placeholder="输入新文件名" aria-label="重命名目标" /><button type="button" className="button-secondary" disabled={!conflictName.trim()} onClick={() => onResolve({ action: "rename", name: conflictName.trim() })}>保留两份</button></> : null}{conflict.allowedActions.includes("overwrite") ? <button type="button" className="button-danger" onClick={() => onResolve({ action: "overwrite" })}>覆盖</button> : null}</div></div> : null}
    {transfer.failures.length ? <ul className="transfer-failures">{transfer.failures.map((failure, index) => <li key={index}>{failure.item}: {failure.message}{failure.stagingResidue ? " · 暂存残留：" + failure.stagingResidue : ""}</li>)}</ul> : null}
    <small>{transfer.verifiedFiles} 个已校验 · {transfer.unverifiedFiles} 个未校验 · 跳过 {transfer.skippedFiles}</small>
  </li>;
}
