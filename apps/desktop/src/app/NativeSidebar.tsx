import { useEffect, useMemo, useState } from "react";
import type { Server, WindowShellSidebar } from "@yukinal/shared";
import { AddServerModal } from "../features/servers/AddServerModal.js";
import { onNativeWindowAction, updateNativeWindowSidebar } from "../lib/ipc.js";
import { useServerAction, useServers } from "../lib/servers.js";
import { dispatchWindowShellAction } from "../lib/window-shell-actions.js";
import { ENVIRONMENT_LABEL_SHORT } from "../lib/labels.js";
import { useWorkspaceStore } from "../stores/workspace-store.js";

const displayText = (value: string, limit: number) => value.replace(/[\u0000-\u001f\u007f]/gu, " ")
  .slice(0, limit).replace(/\p{Surrogate}/gu, "�");

/** Native controls present the existing cache and send intent back to existing services. */
export function NativeSidebar({ onNavigate }: { onNavigate: () => void }) {
  const servers = useServers({ enabled: false });
  const selectedServerId = useWorkspaceStore((state) => state.selectedServerId);
  const { action, busy } = useServerAction();
  const [dialog, setDialog] = useState<"add" | Server | null>(null);
  const sidebar = useMemo<WindowShellSidebar>(() => ({
    selectedServerId,
    servers: (servers.data ?? []).map((server) => ({
      id: server.id, name: displayText(server.name, 160),
      detail: displayText(`${ENVIRONMENT_LABEL_SHORT[server.metadata.environment]} · ${server.connection.host}:${server.connection.port}${server.metadata.region ? ` · ${server.metadata.region}` : ""}`, 256), status: server.status,
    })),
    loading: servers.isLoading, failed: servers.isError, blocked: busy || dialog !== null,
  }), [selectedServerId, servers.data, servers.isLoading, servers.isError, busy, dialog]);

  useEffect(() => {
    if (servers.data) useWorkspaceStore.getState().syncServerSelection(servers.data.map((server) => server.id));
  }, [servers.data]);
  useEffect(() => {
    void updateNativeWindowSidebar(sidebar).catch((error: unknown) => console.warn("Could not update native sidebar", error));
  }, [sidebar]);
  useEffect(() => onNativeWindowAction((intent) => {
    dispatchWindowShellAction(intent, {
      servers: servers.data ?? [], blocked: busy || dialog !== null || Boolean(document.querySelector("dialog[open]")),
      navigate: (section) => { useWorkspaceStore.getState().setPrimary(section); onNavigate(); },
      select: (id) => { useWorkspaceStore.getState().selectServer(id); onNavigate(); },
      add: () => setDialog("add"), refresh: () => { void servers.refetch(); },
      edit: setDialog,
      confirmDelete: (server) => window.confirm(`确定删除服务器“${server.name}”？`),
      mutate: (input) => action.mutate(input),
    });
  }), [servers.data, servers.refetch, busy, dialog, onNavigate, action.mutate]);

  return <>
    {action.isError ? <div className="native-action-error" role="alert">{action.error.message}<button type="button" className="text-button" onClick={() => action.reset()}>关闭提示</button></div> : null}
    {dialog ? <AddServerModal server={dialog === "add" ? undefined : dialog} onClose={() => setDialog(null)} /> : null}
  </>;
}
