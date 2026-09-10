import { useQuery } from "@tanstack/react-query";
import { IPC_COMMANDS } from "@yukinal/shared";

import { callDesktop, isDesktopShell } from "./ipc.js";

export const PROVIDERS_QUERY_KEY = ["providers"] as const;

/** Durable provider list shared by Settings and Agent runs. */
export function useProviders() {
  return useQuery({
    queryKey: PROVIDERS_QUERY_KEY,
    enabled: isDesktopShell(),
    queryFn: async () => (await callDesktop(IPC_COMMANDS.providerList, {})).providers,
  });
}
