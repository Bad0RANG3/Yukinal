import { useIsMutating, useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { IPC_COMMANDS } from "@yukinal/shared";
import { callDesktop, isDesktopShell } from "./ipc.js";

/**
 * 服务器列表的缓存键，全项目唯一。
 *
 * 这个字面量原先在五个地方各写了一遍（本文件两处、ProjectsPane、ActivityFeed、
 * AddServerModal），而其中三处**各自带一套不同的新鲜度策略**：
 *
 *   - 本文件：`staleTime: 10s` + `refetchInterval: 60s`（侧栏要轮询）
 *   - ProjectsPane：只有 `staleTime: 10s`，不轮询
 *   - ActivityFeed：两个都不写，落到全局默认的 `staleTime: 5s`
 *
 * 同一个 key 就是同一份缓存，所以「侧栏显示的是 60 秒内的数据、活动页显示的是
 * 5 秒内的数据」这种事本来就不该由三个组件各自决定。现在统一走 `useServers()`，
 * 轮询由唯一那个常驻的观察者（侧栏）承担，另外两处复用同一份缓存。
 *
 * 写死的字面量还有第二个问题：`invalidateQueries({ queryKey: ["servers"] })` 里的
 * key 拼错时**不会报任何错**，只会静默地刷新不到东西。改成常量后拼错就是编译错误。
 */
export const SERVERS_QUERY_KEY = ["servers"] as const;

export function useServers(options: { enabled?: boolean } = {}) {
  const enabled = options.enabled ?? true;
  return useQuery({
    queryKey: SERVERS_QUERY_KEY,
    enabled: enabled && isDesktopShell(),
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
    onSettled: () => queryClient.invalidateQueries({ queryKey: SERVERS_QUERY_KEY }),
  });
  return { action, busy };
}
