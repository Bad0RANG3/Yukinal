import { useQuery } from "@tanstack/react-query";
import { IPC_COMMANDS } from "@yukinal/shared";
import { callDesktop, isDesktopShell } from "../../lib/ipc.js";
import { useWorkspaceStore } from "../../stores/workspace-store.js";

/**
 * Server list. Empty by design until the local database gives us SQLite rows: an empty
 * list with a reason beats a fake list with demo servers.
 */
export function ServerList() {
  const selectedServerId = useWorkspaceStore((state) => state.selectedServerId);
  const selectServer = useWorkspaceStore((state) => state.selectServer);

  const servers = useQuery({
    queryKey: ["servers"],
    enabled: isDesktopShell(),
    queryFn: async () => (await callDesktop(IPC_COMMANDS.serverList, {})).servers,
  });

  return (
    <aside className="w-60 shrink-0 border-r border-zinc-800 p-2">
      <div className="flex items-center justify-between px-1 py-2">
        <span className="text-xs uppercase tracking-wide text-zinc-500">Servers</span>
        <button type="button" className="rounded border border-zinc-700 px-2 py-0.5 text-xs text-zinc-300">
          Add
        </button>
      </div>

      {servers.isError ? (
        <p className="px-1 text-xs text-zinc-500">{String(servers.error)}</p>
      ) : null}

      {(servers.data ?? []).length === 0 ? (
        <p className="px-1 text-xs text-zinc-500">
          No servers yet. Storage + SSH is not implemented yet; the UI shell is live now.
        </p>
      ) : null}

      <ul className="space-y-1">
        {(servers.data ?? []).map((server) => (
          <li key={server.id}>
            <button
              type="button"
              onClick={() => selectServer(server.id)}
              className={`w-full truncate rounded px-2 py-1.5 text-left ${
                selectedServerId === server.id ? "bg-zinc-800" : "hover:bg-zinc-900"
              }`}
            >
              {server.name}
            </button>
          </li>
        ))}
      </ul>
    </aside>
  );
}
