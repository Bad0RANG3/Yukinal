import { useQuery, useQueryClient } from "@tanstack/react-query";
import { IPC_COMMANDS } from "@yukinal/shared";
import { useEffect } from "react";

import { callDesktop, isDesktopShell } from "./ipc.js";

export const PROVIDERS_QUERY_KEY = ["providers"] as const;

/** Durable provider list shared by Settings, Agent and the startup importer. */
export function useProviders() {
  return useQuery({
    queryKey: PROVIDERS_QUERY_KEY,
    enabled: isDesktopShell(),
    queryFn: async () => (await callDesktop(IPC_COMMANDS.providerList, {})).providers,
  });
}

/**
 * Import local OpenCode/Codex/CC Switch configuration once per app session. The
 * native command is idempotent, so a fresh launch also picks up changed local
 * configuration without creating duplicate provider rows.
 */
export function useStartupProviderImport() {
  const queryClient = useQueryClient();
  const query = useQuery({
    queryKey: ["providers", "auto-import"],
    enabled: isDesktopShell(),
    staleTime: Infinity,
    retry: false,
    queryFn: () => callDesktop(IPC_COMMANDS.providerImportAuto, {}),
  });

  useEffect(() => {
    if (!query.data) return;
    void queryClient.invalidateQueries({ queryKey: PROVIDERS_QUERY_KEY });
  }, [query.data, queryClient]);

  return query;
}
