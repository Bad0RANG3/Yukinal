import { queryOptions, useIsMutating, useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { IPC_COMMANDS } from "@yukinal/shared";
import { callDesktop, isDesktopShell } from "./ipc.js";
import { prepareServerSnapshot } from "./server-snapshot.js";

/** Shared cache; only the persistent ServerList owns its polling timer. */
export const SERVERS_QUERY_KEY = ["servers"] as const;

export function serverListQueryOptions({ enabled = true, poll = false }: { enabled?: boolean; poll?: boolean } = {}) {
  return queryOptions({
    queryKey: SERVERS_QUERY_KEY,
    enabled: enabled && isDesktopShell(),
    staleTime: 10_000,
    refetchInterval: poll ? 60_000 : false,
    queryFn: async () => (await callDesktop(IPC_COMMANDS.serverList, {})).servers,
  });
}

export function useServers(options: { enabled?: boolean; poll?: boolean } = {}) {
  return useQuery(serverListQueryOptions(options));
}

export function useServerAction() {
  const queryClient = useQueryClient();
  const busy = useIsMutating({ mutationKey: ["server-action"] }) > 0;
  const action = useMutation({
    mutationKey: ["server-action"],
    mutationFn: ({ type, serverId }: { type: "connect" | "disconnect" | "delete"; serverId: string }) => {
      const command = type === "connect" ? IPC_COMMANDS.serverConnect : type === "disconnect" ? IPC_COMMANDS.serverDisconnect : IPC_COMMANDS.serverDelete;
      return callDesktop(command, { serverId });
    },
    onSuccess: (_data, { type, serverId }) => {
      if (type === "connect") {
        void prepareServerSnapshot(queryClient, serverId);
      } else {
        void queryClient.cancelQueries({ queryKey: ["serverSnapshot", serverId], exact: true });
        // Retain a disconnected server's last sample; deleting removes its cache.
        if (type === "delete") queryClient.removeQueries({ queryKey: ["serverSnapshot", serverId], exact: true });
      }
    },
    onSettled: () => queryClient.invalidateQueries({ queryKey: SERVERS_QUERY_KEY }),
  });
  return { action, busy };
}
