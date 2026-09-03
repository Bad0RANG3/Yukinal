/**
 * 服务器侧栏：SQLite 里的真实行 + 添加表单。空列表给原因，不做假数据。
 */

import { useQuery } from "@tanstack/react-query";
import { IPC_COMMANDS, type Server } from "@yukinal/shared";
import { useState } from "react";

import { callDesktop, isDesktopShell } from "../../lib/ipc.js";
import { useWorkspaceStore } from "../../stores/workspace-store.js";
import { AddServerModal } from "./AddServerModal.js";

export function ServerList() {
  const selectedServerId = useWorkspaceStore((state) => state.selectedServerId);
  const selectServer = useWorkspaceStore((state) => state.selectServer);
  const [adding, setAdding] = useState(false);

  const servers = useQuery({
    queryKey: ["servers"],
    enabled: isDesktopShell(),
    refetchInterval: 60_000,
    queryFn: async () => (await callDesktop(IPC_COMMANDS.serverList, {})).servers,
  });

  return (
    <aside className="w-60 shrink-0 border-r border-zinc-800 p-2">
      <div className="flex items-center justify-between px-1 py-2">
        <span className="text-xs uppercase tracking-wide text-zinc-500">服务器</span>
        <button
          type="button"
          onClick={() => setAdding(true)}
          className="rounded border border-zinc-700 px-2 py-0.5 text-xs text-zinc-300 hover:border-zinc-500"
        >
          ＋ 添加
        </button>
      </div>

      {servers.isError ? (
        <p className="px-1 text-xs text-red-400">{String(servers.error)}</p>
      ) : null}

      {(servers.data ?? []).length === 0 ? (
        <p className="px-1 text-xs leading-relaxed text-zinc-500">
          还没有服务器。点「添加」录入第一台 —— 凭据会进系统钥匙串，列表存 SQLite。
        </p>
      ) : null}

      <ul className="space-y-1">
        {(servers.data ?? []).map((server: Server) => (
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

      {adding ? <AddServerModal onClose={() => setAdding(false)} /> : null}
    </aside>
  );
}