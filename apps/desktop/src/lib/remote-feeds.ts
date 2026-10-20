import { queryOptions } from "@tanstack/react-query";
import { IPC_COMMANDS } from "@yukinal/shared";
import { callDesktop, isDesktopShell } from "./ipc.js";

/** Visible feeds collect independently of AI. Hidden panes perform no remote reads. */
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
    staleTime: 3_000,
    refetchInterval: active ? 3_000 : false,
    queryKey: ["serverLogs", serverId],
    enabled: active && Boolean(serverId) && isDesktopShell(),
    queryFn: () => serverId ? callDesktop(IPC_COMMANDS.serverLogs, { serverId }) : null,
  });
}

export function serverServicesQueryOptions(serverId: string | null | undefined, active = true) {
  return queryOptions({
    ...remoteFeedPolicy,
    staleTime: 10_000,
    refetchInterval: active ? 10_000 : false,
    queryKey: ["serverServices", serverId],
    enabled: active && Boolean(serverId) && isDesktopShell(),
    queryFn: () => serverId ? callDesktop(IPC_COMMANDS.serverServices, { serverId }) : null,
  });
}
