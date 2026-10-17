/**
 * Runtime probes for the desktop shell.
 *
 * These are the *only* places React learns what native processes are alive — and the
 * answer always comes from the desktop host.
 */

import { queryOptions, useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { IPC_COMMANDS } from "@yukinal/shared";
import type { AgentSpawnResponse } from "@yukinal/shared";

import { callDesktop, isDesktopShell } from "./ipc.js";

export const RUNTIME_QUERY_KEYS = {
  ping: ["runtime", "core"] as const,
  status: ["runtime", "agent"] as const,
  logs: ["runtime", "agent-logs"] as const,
};

export function useCorePing() {
  return useQuery({
    queryKey: RUNTIME_QUERY_KEYS.ping,
    enabled: isDesktopShell(),
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
export function agentStatusQueryOptions({ poll = false }: { poll?: boolean } = {}) {
  return queryOptions({
    queryKey: RUNTIME_QUERY_KEYS.status,
    enabled: isDesktopShell(),
    refetchInterval: poll ? (query) => (query.state.data?.running ? 1_500 : 5_000) : false,
    queryFn: () => callDesktop(IPC_COMMANDS.agentStatus, {}),
  });
}

/** AgentPanel owns polling; settings and onboarding observe the same cache. */
export function useAgentStatus(options: { poll?: boolean } = {}) {
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
