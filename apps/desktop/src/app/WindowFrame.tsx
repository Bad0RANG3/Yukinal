import { useEffect, type ReactNode } from "react";

import { isNativeDesktopShell, updateNativeWindowContext } from "../lib/ipc.js";
import { useServers } from "../lib/servers.js";
import { useWorkspaceStore } from "../stores/workspace-store.js";

/** Native chrome reflects the same workspace state as the content it hosts. */
export function WindowFrame({ children }: { children: ReactNode }) {
  const native = isNativeDesktopShell();
  const section = useWorkspaceStore((state) => state.primary);
  const selectedServerId = useWorkspaceStore((state) => state.selectedServerId);
  const agentBusy = useWorkspaceStore((state) => state.agentBusy);
  // Observe the existing cache; AppShell owns fetching and polling.
  const servers = useServers({ enabled: false });
  const selected = section === "servers"
    ? servers.data?.find((server) => server.id === selectedServerId) : undefined;
  const serverName = selected?.name.replace(/[\u0000-\u001f\u007f]/gu, " ").slice(0, 160).replace(/\p{Surrogate}/gu, "�") ?? null;
  const serverStatus = selected?.status ?? null;

  useEffect(() => {
    if (!native) return;
    void updateNativeWindowContext({ section, serverName, serverStatus, agentBusy })
      .catch((error: unknown) => console.warn("Could not update native window context", error));
  }, [native, section, serverName, serverStatus, agentBusy]);

  if (!native) return children;
  return <div className="native-workspace">{children}</div>;
}
