/**
 * Runtime probes for the desktop shell.
 *
 * These are the *only* places React learns what native processes are alive — and the
 * answer always comes from Rust. React cannot spawn, list or signal a process.
 */

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
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

/* 这里曾经有两个没人用的导出，已删除：
 *
 * - `useKillAgent`：绑 `IPC_COMMANDS.agentKill` 的 mutation，除了它自己的函数体
 *   之外全仓库没有任何引用。留着一个「随手就能杀掉 Agent」的现成钩子在这个
 *   安全敏感的 IPC 层里，是别人顺手接上按钮的最短路径；而界面目前**故意**不提供
 *   这个操作，所以钩子应该是「要用的时候再写」。
 *
 *   `IPC_COMMANDS.agentKill` 本身**保留** —— 它不是死代码，而是与 Rust 侧的契约：
 *   `crates/core/src/ipc.rs:66` 用 `packages/shared/fixtures/ipc/agent_kill.json`
 *   给这个命令的响应做了序列化测试。删掉它会让那条契约测试失去依据。
 *
 * - `statusLabel`：拼运行时状态文案。它被两个地方各自内联重写了，措辞还不一样
 *   （`RuntimeSettings.tsx:48-50` 是「Agent 未启动 / 查询中…」，`AgentHeader.tsx:46-48`
 *   是另一套）。三份实现里只有这一份没人调用，所以删的是它。
 */

