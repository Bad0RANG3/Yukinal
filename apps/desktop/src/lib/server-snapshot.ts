import { queryOptions, type QueryClient } from "@tanstack/react-query";
import { IPC_COMMANDS } from "@yukinal/shared";
import { callDesktop } from "./ipc.js";

/** The connection warm-up and inspector share one in-flight request and cache. */
export function serverSnapshotQueryOptions(serverId: string) {
  return queryOptions({
    queryKey: ["serverSnapshot", serverId] as const,
    staleTime: 15_000,
    retry: false,
    queryFn: async () => {
      const { snapshot } = await callDesktop(IPC_COMMANDS.serverSnapshot, { serverId });
      if (snapshot.serverId !== serverId) throw new Error("快照所属服务器不匹配");
      return snapshot;
    },
  });
}

export async function prepareServerSnapshot(client: QueryClient, serverId: string): Promise<void> {
  const options = serverSnapshotQueryOptions(serverId);
  await client.cancelQueries({ queryKey: options.queryKey, exact: true });
  // Keep the old timestamp visible until a newly collected sample arrives.
  await client.invalidateQueries({ queryKey: options.queryKey, exact: true, refetchType: "none" });
  await client.prefetchQuery(options);
}
