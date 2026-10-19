import { queryOptions } from "@tanstack/react-query";
import { IPC_COMMANDS } from "@yukinal/shared";
import { callDesktop, isDesktopShell } from "./ipc.js";

/** Remote inventories are requested on entry or explicit refresh, never by a hidden timer. */
const remoteFeedPolicy = {
  staleTime: 60_000,
  gcTime: 120_000,
  retry: false,
  refetchOnWindowFocus: false,
  refetchOnReconnect: false,
  refetchInterval: false,
  refetchIntervalInBackground: false,
} as const;

export function serverLogsQueryOptions(serverId: string | null | undefined, active = true) {
  return queryOptions({
    ...remoteFeedPolicy,
    queryKey: ["serverLogs", serverId],
    enabled: active && Boolean(serverId) && isDesktopShell(),
    queryFn: () => serverId ? callDesktop(IPC_COMMANDS.serverLogs, { serverId }) : null,
  });
}

export function serverServicesQueryOptions(serverId: string | null | undefined, active = true) {
  return queryOptions({
    ...remoteFeedPolicy,
    queryKey: ["serverServices", serverId],
    enabled: active && Boolean(serverId) && isDesktopShell(),
    queryFn: () => serverId ? callDesktop(IPC_COMMANDS.serverServices, { serverId }) : null,
  });
}
