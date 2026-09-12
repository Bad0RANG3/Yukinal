/**
 * 主机指纹（ADR 0012）：查询、动作，以及「什么时候可以信任」这条规则。
 *
 * 这里刻意分成两半：
 *
 * - `hostKeyTrustEligibility` 是**纯函数**。它承载的是这个功能里唯一一条会出安全问题的
 *   规则 —— 「信任此指纹」什么时候可用。放在组件里写成一串 `&&` 的话，它就只能靠渲染
 *   测试去碰，而那些测试恰好最难覆盖「不一致」这种状态。
 * - 三个 hook 只负责 IPC 与失效。
 *
 * 与 `lib/servers.ts` 同一套写法（查询键常量 + `isDesktopShell()` 门控 + 成功后就地失效），
 * 所以这里没有第二套约定。
 */

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  IPC_COMMANDS,
  type ServerHostKeyForgetResult,
  type ServerHostKeyProbeResult,
  type ServerHostKeyTrustResult,
} from "@yukinal/shared";

import { callDesktop, isDesktopShell } from "./ipc.js";

/**
 * 某个 server id 的主机指纹缓存键。
 *
 * 用函数而不是字面量，是为了让「拼错 key 不会报错」这件事在这里同样不可能：调用点拿到
 * 的是一个已定型的元组，失效时不会写成一个不存在的键（`servers.ts` 里同样的理由）。
 */
export const HOST_KEY_QUERY_KEY = (serverId: string) => ["host-key", serverId] as const;

/** 当前钉住的指纹与状态（**不触网**：打开面板就要画出来）。 */
export function useHostKeyStatus(serverId: string, options: { enabled?: boolean } = {}) {
  const enabled = (options.enabled ?? true) && isDesktopShell();
  return useQuery({
    queryKey: HOST_KEY_QUERY_KEY(serverId),
    enabled: enabled && Boolean(serverId),
    // 指纹只由这台机器上的用户动作改变（探针不写、连接只在 TOFU 时写），所以不需要轮询。
    staleTime: 5_000,
    queryFn: async () => callDesktop(IPC_COMMANDS.serverHostKeyStatus, { serverId }),
  });
}

export interface HostKeyActions {
  probe: ReturnType<typeof useMutation<ServerHostKeyProbeResult, Error, void>>;
  trust: ReturnType<typeof useMutation<ServerHostKeyTrustResult, Error, string>>;
  forget: ReturnType<typeof useMutation<ServerHostKeyForgetResult, Error, void>>;
  /** 三个动作里有没有正在跑的 —— 用来禁用按钮，避免边探边钉。 */
  busy: boolean;
}

/**
 * 三个动作。**探针不写任何东西**，所以它不失效任何查询；「钉住」和「遗忘」都改 store，
 * 成功后必须让状态查询重新读一次（否则界面会继续显示旧钉子）。
 *
 * 两个 mutation 的返回类型是各自命令的响应（而不是 `unknown`）：界面要能说出「本来就钉着
 * 这个指纹」和「本来就没有钉子」这两种「什么都没变」的情况，而不是一律显示「成功」。
 */
export function useHostKeyActions(serverId: string): HostKeyActions {
  const queryClient = useQueryClient();
  const refreshStatus = () =>
    queryClient.invalidateQueries({ queryKey: HOST_KEY_QUERY_KEY(serverId) });

  const probe = useMutation<ServerHostKeyProbeResult, Error, void>({
    mutationFn: () => callDesktop(IPC_COMMANDS.serverHostKeyProbe, { serverId }),
    // 故意不 invalidate：探针不改变任何持久状态，刷新状态反而会让界面看起来
    // 「探针做了点什么」。状态里唯一会变的是比较结果，而那由探针结果自己带回来。
  });

  const trust = useMutation<ServerHostKeyTrustResult, Error, string>({
    mutationFn: (fingerprint) =>
      callDesktop(IPC_COMMANDS.serverHostKeyTrust, { serverId, fingerprint }),
    onSettled: () => void refreshStatus(),
  });

  const forget = useMutation<ServerHostKeyForgetResult, Error, void>({
    mutationFn: () => callDesktop(IPC_COMMANDS.serverHostKeyForget, { serverId }),
    onSettled: () => void refreshStatus(),
  });

  return {
    probe,
    trust,
    forget,
    busy: probe.isPending || trust.isPending || forget.isPending,
  };
}

/** 「信任此指纹」此刻能不能用，以及不能用是为什么。 */
export type HostKeyTrustEligibility =
  | { trustable: true; fingerprint: string }
  | { trustable: false; reason: "no-probe" | "mismatch" | "already-pinned" };

/**
 * 决定「信任此指纹」按钮的可用性。
 *
 * 输入只有**这一次会话里探到的那个响应**，而不是「当前状态 + 某个字符串」：可确认的指纹
 * 必须是这一次真的从服务器那里拿到的那个（ADR 0012 第 3、4 条）。拿别的东西来比 ——
 * 比如界面自己拼的、或者上一次会话留下的 —— 就等于允许确认一个没人核验过的值。
 *
 * 三条规则：
 *
 * 1. **必须先探针。** 没有这次会话的探针结果就没有可确认的指纹。
 * 2. **不一致时不可用。** 探针当时已钉着别的指纹 → `trust` 会被后端拒绝（第 5 条），
 *    所以界面不该摆出一个只会失败的动作，该摆出来的是「先遗忘」。
 * 3. **已经钉着同一个指纹时无事可做**（不是错误，只是没有动作）。
 *
 * 规则 2 的后果值得写下来：不一致状态下，界面上**没有**任何一次点击就能接受新指纹的
 * 路径 —— 这正是「不提供『不一致时仍然继续』」在界面上的形状（第 5 条与被否决的备选）。
 *
 * 调用点要在「钉住」或「遗忘」成功之后丢掉这个探针结果：它的 `pinnedFingerprint` 说的是
 * 探针**当时**的钉子，钉子变了以后它就不再描述现在。
 */
export function hostKeyTrustEligibility(
  probe:
    | Pick<ServerHostKeyProbeResult, "presentedFingerprint" | "pinnedFingerprint">
    | null
    | undefined,
): HostKeyTrustEligibility {
  if (!probe) return { trustable: false, reason: "no-probe" };
  if (probe.pinnedFingerprint === undefined) {
    return { trustable: true, fingerprint: probe.presentedFingerprint };
  }
  if (probe.pinnedFingerprint === probe.presentedFingerprint) {
    return { trustable: false, reason: "already-pinned" };
  }
  return { trustable: false, reason: "mismatch" };
}
