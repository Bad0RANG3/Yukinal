/**
 * Runtime probes for the desktop shell.
 *
 * These are the *only* places React learns what native processes are alive — and the
 * answer always comes from the desktop host.
 */

import { queryOptions, useMutation, useQuery, useQueryClient, type QueryClient } from "@tanstack/react-query";
import { IPC_COMMANDS } from "@yukinal/shared";
import type { AgentSpawnResponse, AgentStatus } from "@yukinal/shared";

import { callDesktop, isDesktopShell } from "./ipc.js";
import { PROVIDERS_QUERY_KEY } from "./providers.js";

export const RUNTIME_QUERY_KEYS = {
  ping: ["runtime", "core"] as const,
  status: ["runtime", "agent"] as const,
  logs: ["runtime", "agent-logs"] as const,
};

const lastAgentRunning = new WeakMap<QueryClient, boolean>();

export function useCorePing({ enabled = true }: { enabled?: boolean } = {}) {
  return useQuery({
    queryKey: RUNTIME_QUERY_KEYS.ping,
    enabled: enabled && isDesktopShell(),
    // Version + OS never change while the window is alive; refetching is noise.
    staleTime: Infinity,
    queryFn: () => callDesktop(IPC_COMMANDS.corePing, {}),
  });
}

/**
 * Sidecar status is polled as a durable health signal. The agent also emits `agent.*`
 * events for active runs, but a poll still catches a process that dies while nobody
 * is looking at the event stream.
 */
export function agentStatusQueryOptions({ poll = false, enabled = true }: { poll?: boolean; enabled?: boolean } = {}) {
  return queryOptions<AgentStatus>({
    queryKey: RUNTIME_QUERY_KEYS.status,
    enabled: enabled && isDesktopShell(),
    refetchInterval: poll ? (query) => (query.state.data?.running ? 1_500 : 5_000) : false,
    queryFn: async ({ client }): Promise<AgentStatus> => {
      const status = await callDesktop(IPC_COMMANDS.agentStatus, {});
      const wasRunning = lastAgentRunning.get(client);
      lastAgentRunning.set(client, status.running);
      // One shared status fetch handles both explicit and automatic startup.
      // Provider reads that failed before startup must be retried once it runs.
      if (status.running && wasRunning !== true) {
        void client.invalidateQueries({ queryKey: PROVIDERS_QUERY_KEY });
      }
      return status;
    },
  });
}

/** AgentPanel owns polling; settings and onboarding observe the same cache. */
export function useAgentStatus(options: { poll?: boolean; enabled?: boolean } = {}) {
  return useQuery(agentStatusQueryOptions(options));
}

export function useAgentLogs(enabled: boolean) {
  return useQuery({
    queryKey: RUNTIME_QUERY_KEYS.logs,
    enabled: enabled && isDesktopShell(),
    queryFn: () => callDesktop(IPC_COMMANDS.agentLogs, {}),
  });
}

export function useSpawnAgent() {
  const queryClient = useQueryClient();
  return useMutation<AgentSpawnResponse, Error>({
    mutationFn: () => callDesktop(IPC_COMMANDS.agentSpawn, {}),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: RUNTIME_QUERY_KEYS.status });
      void queryClient.invalidateQueries({ queryKey: RUNTIME_QUERY_KEYS.logs });
    },
  });
}
