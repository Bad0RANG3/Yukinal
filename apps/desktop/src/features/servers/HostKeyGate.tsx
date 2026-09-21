/**
 * 主机指纹闸门：未核验的主机不会建立 SSH 连接。
 *
 * ## 它解决的是什么
 *
 * 终端、文件、日志、服务与概览都走同一条 `ensure_session`，而它在未知主机上会在
 * **认证之前**失败（`KnownHostsPolicy::RequireMatch`，审计第一阶段 3.1）。这条安全策略
 * 本身是对的：首连自动信任会把一次中间人劫持固化成「可信」。但如果核验入口只存在于
 * 「编辑服务器」弹窗里，第一次点开终端的人只会看到一句英文错误，不知道该做什么。
 *
 * 这个组件把「会失败」提前成一次**本地状态检查**：`server_host_key_status` 不触网，只回答
 * 这个 `host:port` 现在有没有钉子。没有钉子时直接渲染已有的 `HostKeySection`
 * （探针 → 与别处核对 → 信任）；钉住之后自动放行 children。
 *
 * ## 为什么不是解析错误文本
 *
 * 仓库的规则是「不要让 UI 根据英文错误文本判断下一步」（见
 * `docs/project-review-and-roadmap.md` 的错误分类工作项）。这里的判断来自结构化命令
 * `server_host_key_status`，与错误措辞无关；错误分类落地后这个组件也不需要改。
 *
 * ## CA 策略是例外
 *
 * 显式配置了 host CA 的服务器只接受由该 CA 签发、principal 匹配的 host certificate，
 * 叶子 host key 的 pin 不参与校验。所以它不需要（也不该被要求）先钉叶子指纹。
 *
 * ## 为什么规则是纯函数
 *
 * `hostKeyGateDecision` 不在组件里写成一串 `&&`：这条规则决定「放行还是拦下」，是这里
 * 唯一会出安全问题的地方，所以它和 `hostKeyTrustEligibility` 一样被抽成纯函数单独测
 * （不依赖 Tauri、不依赖 react-query，也就不依赖测试环境里有没有 `window`）。
 */

import { useId, type ReactNode } from "react";
import type { Server } from "@yukinal/shared";

import { errorMessage } from "../../lib/format.js";
import { useHostKeyStatus } from "../../lib/host-key.js";
import { isDesktopShell } from "../../lib/ipc.js";
import { useServers } from "../../lib/servers.js";
import { HostKeySection } from "./HostKeySection.js";

/** 闸门在某一刻要显示的东西。 */
export type HostKeyGateDecision = "children" | "loading" | "error" | "gate";

/**
 * 「放行还是拦下」的纯规则。
 *
 * 输入是闸门需要的**全部**事实，没有隐含状态：
 * - 没有目标、或不在 Tauri 壳里 → 放行（让面板显示它自己的空状态/预览态）；
 * - 还没读到本机指纹 → loading（绝不在这时先放行：那会在状态未知时建立连接）；
 * - 读状态失败 → error（fail closed，不放行）；
 * - 显式 CA 证书链，或已有钉子 → 放行；
 * - 其余 → gate。
 */
export function hostKeyGateDecision(input: {
  serverId: string | null;
  desktopShell: boolean;
  serversLoading: boolean;
  statusLoading: boolean;
  statusFailed: boolean;
  caPolicyEnabled: boolean;
  pinned: boolean;
}): HostKeyGateDecision {
  if (!input.serverId || !input.desktopShell) return "children";
  if (input.serversLoading || input.statusLoading) return "loading";
  if (input.statusFailed) return "error";
  if (input.caPolicyEnabled || input.pinned) return "children";
  return "gate";
}

export function HostKeyGate({
  serverId,
  children,
}: {
  /** 当前选中的服务器；`null` 时原样放行，让面板显示它自己的空状态。 */
  serverId: string | null;
  children: ReactNode;
}) {
  const titleId = useId();
  // hooks 必须无条件调用：`serverId` 为空时下面的查询是 disabled（不会发 IPC）。
  const servers = useServers();
  const status = useHostKeyStatus(serverId ?? "");
  const server: Server | undefined = servers.data?.find((candidate) => candidate.id === serverId);

  const decision = hostKeyGateDecision({
    serverId,
    desktopShell: isDesktopShell(),
    serversLoading: servers.isLoading,
    statusLoading: status.isLoading,
    statusFailed: status.isError,
    caPolicyEnabled: Boolean(server?.connection.hostCertificateAuthority),
    pinned: status.data?.pinned === true,
  });

  if (decision === "children") return <>{children}</>;
  if (decision === "loading") {
    return (
      <p className="muted-copy host-key-gate-loading" role="status">
        正在读取主机指纹…
      </p>
    );
  }
  if (decision === "error") {
    return (
      <p className="form-error" role="alert">
        {errorMessage(status.error)}
      </p>
    );
  }

  // `gate` 蕴含 serverId 非空；显式收窄一次，别让类型系统去推导渲染分支。
  if (!serverId) return <>{children}</>;
  const caPolicyEnabled = Boolean(server?.connection.hostCertificateAuthority);

  return (
    <div className="host-key-gate">
      <section className="host-key-gate-intro" aria-labelledby={titleId}>
        <p className="eyebrow">需要先核验</p>
        <h2 id={titleId}>这台主机还没有核验过指纹</h2>
        <p>
          Yukinal 不会在首次连接时自动信任主机密钥 —— 那会让中间人有机会把一次自动信任
          固化成「可信」。请先用「探针」读取服务器这次出示的指纹，与你在服务器上执行
          <code> ssh-keygen -lf /etc/ssh/ssh_host_ed25519_key.pub </code>
          （路径随发行版与密钥类型而变）或在云控制台里看到的那一串 <code>SHA256:</code>
          指纹核对一致后，再点「信任此指纹」。
        </p>
      </section>
      <HostKeySection serverId={serverId} caPolicyEnabled={caPolicyEnabled} />
    </div>
  );
}
