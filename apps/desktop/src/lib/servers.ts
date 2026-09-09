import { useIsMutating, useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { IPC_COMMANDS } from "@yukinal/shared";
import { callDesktop, isDesktopShell } from "./ipc.js";

export function useServers() {
  return useQuery({
    queryKey: ["servers"],
    enabled: isDesktopShell(),
    staleTime: 10_000,
    refetchInterval: 60_000,
    queryFn: async () => (await callDesktop(IPC_COMMANDS.serverList, {})).servers,
  });
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
    onSettled: () => queryClient.invalidateQueries({ queryKey: ["servers"] }),
  });
  return { action, busy };
}
