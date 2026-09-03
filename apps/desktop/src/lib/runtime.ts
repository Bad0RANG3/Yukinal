/**
 * Runtime probes for the desktop shell.
 *
 * These are the *only* places React learns what native processes are alive — and the
 * answer always comes from Rust. React cannot spawn, list or signal a process.
 */

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { IPC_COMMANDS } from "@yukinal/shared";
import type { AgentSpawnResponse, AgentStatus } from "@yukinal/shared";

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
 * Sidecar status. Polled rather than pushed until the agent loop wires `agent.*` events:
 * a 1.5s poll of a local state read is invisible cost, and it survives a sidecar
 * that dies while nobody is looking.
 */
export function useAgentStatus() {
  return useQuery({
    queryKey: RUNTIME_QUERY_KEYS.status,
    enabled: isDesktopShell(),
    refetchInterval: (query) => (query.state.data?.running ? 1_500 : 5_000),
    queryFn: () => callDesktop(IPC_COMMANDS.agentStatus, {}),
  });
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

export function useKillAgent() {
  const queryClient = useQueryClient();
  return useMutation<{ killed: boolean }, Error>({
    mutationFn: () => callDesktop(IPC_COMMANDS.agentKill, {}),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: RUNTIME_QUERY_KEYS.status });
      void queryClient.invalidateQueries({ queryKey: RUNTIME_QUERY_KEYS.logs });
    },
  });
}

export function statusLabel(status: AgentStatus | undefined, shellAvailable: boolean): string {
  if (!shellAvailable) return "浏览器预览 —— 原生命中不可用";
  if (!status) return "查询 core 中…";
  if (status.running && status.pid !== null) {
    return `agent · pid ${status.pid} · 协议 ${status.protocolVersion ?? "?"} · ${status.toolCount ?? 0} 个工具`;
  }
  if (status.lastExit) {
    return `agent 已退出（${status.lastExit.code ?? status.lastExit.signal ?? "未知"}）`;
  }
  return "agent 未启动";
}
