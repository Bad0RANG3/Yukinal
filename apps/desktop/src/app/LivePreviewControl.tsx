import { useSyncExternalStore } from "react";

import { Icon } from "../components/Icon.js";
import { useWorkspaceStore } from "../stores/workspace-store.js";

type PreviewState = "watching" | "updating" | "offline" | "error";
const LABELS: Record<PreviewState, string> = {
  watching: "实时预览", updating: "同步中…", offline: "预览已断开", error: "更新有误",
};

type PreviewStore = { state: PreviewState; listeners: Set<() => void> };
const hot = import.meta.hot;
const preview: PreviewStore = hot?.data.livePreviewStore ?? { state: "watching", listeners: new Set() };
function update(state: PreviewState) {
  preview.state = state;
  for (const listener of preview.listeners) listener();
}
function subscribe(listener: () => void) { preview.listeners.add(listener); return () => { preview.listeners.delete(listener); }; }
function snapshot() { return preview.state; }
if (hot) {
  // Register before React's refresh effects run, so updating this control itself
  // cannot miss afterUpdate and leave its status stuck at "syncing".
  hot.data.livePreviewStore = preview;
  hot.on("vite:ws:connect", () => update("watching"));
  hot.on("vite:ws:disconnect", () => update("offline"));
  hot.on("vite:beforeUpdate", () => update("updating"));
  hot.on("vite:afterUpdate", () => update("watching"));
  hot.on("vite:error", () => update("error"));
}

/** Vite updates this same native window; no polling or periodic page reloads. */
export function LivePreviewControl() {
  const state = useSyncExternalStore(subscribe, snapshot, snapshot);
  const busy = useWorkspaceStore((workspace) => workspace.agentBusy);
  const hasDraft = useWorkspaceStore((workspace) => Boolean(workspace.agentDraft.trim()) || workspace.agentAttachments.length > 0);
  if (!import.meta.env.DEV || !import.meta.hot) return null;
  const protectedInput = busy || hasDraft;
  const title = busy ? "任务运行中"
    : hasDraft ? "保留当前输入，发送或清空后可手动刷新；界面修改仍会自动同步"
    : "保存界面修改后自动同步到当前窗口；点击可手动刷新";
  return <button type="button" className={`live-preview-control is-${state}`} aria-label="刷新实时预览" title={title} disabled={protectedInput} onClick={() => window.location.reload()}>
    <span className="live-preview-dot" aria-hidden="true" />
    <span className="live-preview-label">{LABELS[state]}</span>
    <Icon name="refresh" size="xs" />
  </button>;
}
