/**
 * SSH 失败 → 一句能照做的话。
 *
 * 宿主的 C++ SSH 核心本来就分得清失败原因：`native/src/ssh.cpp` 对每种传输失败
 * 都写自己的 `error_code`（`authentication_failed`、`credential_unavailable`、
 * `timeout`、`host_key_unverified`、`connection_failed`、`resolver_*`、`proxy_*`）。
 * 但桌面宿主过去把这十二种全部折叠成 `SSH_ERROR`，于是界面只剩一句英文
 * "SSH authentication failed"，而它能给出的建议只有「检查地址、端口、用户名及
 * 认证信息」—— 恰恰是最没有用的一句，因为它把五件不同的事说成了一件。
 *
 * 现在宿主保留原因（`SSH_` 前缀的码，见 `ipc_router.cpp` 的 `ssh_failure_code`），
 * 前端负责把它翻成「出了什么事 + 下一步做什么」。两个约束：
 *
 * - **不猜。** 没有把握的码一律走 `SSH_ERROR` 的通用文案，绝不编一个具体原因。
 *   说错原因比说得笼统更糟：用户会按错误的建议去改配置。
 * - **不吞原话。** 核心写的 `error_message` 是唯一的一手信息，界面要留着它，
 *   只是不再让它独自承担全部解释责任。
 *
 * 刻意不依赖 React，可直接单测（见 tests/ssh-error.test.ts）。
 */

export interface SshFailureAdvice {
  /** 宿主返回的码，保持原样（`SSH_AUTHENTICATION_FAILED` 等）。 */
  code: string;
  /** 出了什么事，一句话。 */
  title: string;
  /** 下一步做什么。 */
  hint: string;
}

interface Advice {
  title: string;
  hint: string;
}

const GENERIC_ADVICE: Advice = {
  title: "SSH 连接失败。",
  hint: "连接在建立过程中失败，原因没有更细的信息。先确认地址、端口与用户名，再重试。",
};

const ADVICE: Record<string, Advice> = {
  SSH_AUTHENTICATION_FAILED: {
    title: "服务器拒绝了这次认证。",
    hint:
      "服务器接受了连接、但没接受凭据。核对用户名，然后在服务器设置里重新填一遍密码并保存（编辑时密码框留空表示沿用旧凭据）；如果这台机器只允许私钥，改用私钥认证。",
  },
  SSH_CREDENTIAL_UNAVAILABLE: {
    title: "这台服务器的凭据读不出来。",
    hint:
      "本机凭据库里没有可用的密码或私钥。在服务器设置里重新填写认证信息并保存；如果系统凭据库刚被清理过，需要重新输入一次。",
  },
  SSH_HOST_KEY_UNVERIFIED: {
    title: "还没确认过这台服务器的主机指纹。",
    hint:
      "先在「概览」里点「探针」，把它出示的 SHA256 指纹与你从可信渠道拿到的指纹逐字核对，一致后再点「信任此指纹」。指纹不一致时不要信任。",
  },
  SSH_TIMEOUT: {
    title: "连接超时。",
    hint:
      "握手阶段没有响应。确认主机地址与端口可达（防火墙、安全组、跳板机），再重试。",
  },
  SSH_CONNECTION_FAILED: {
    title: "连不上这台服务器。",
    hint: "端口没有接受连接。确认 SSH 服务在运行、地址与端口写对了、网络路径通畅。",
  },
  SSH_DENIED_BY_POLICY: {
    title: "这次连接被本机策略拒绝。",
    hint: "目标没有被主机白名单允许，或命中了执行黑名单。检查服务器配置与主机策略后重试。",
  },
  SSH_RESOLVER_UNAVAILABLE: {
    title: "解析不出这个主机名。",
    hint: "域名解析服务不可用。检查本机 DNS 设置，或直接填写 IP 地址。",
  },
  SSH_RESOLVER_BUSY: {
    title: "解析服务忙，这次没排上。",
    hint: "同名解析请求过多。稍等几秒后重试。",
  },
  SSH_PROXY_TLS_FAILED: {
    title: "出站代理的 TLS 握手失败。",
    hint: "代理证书不被信任或已被替换。检查「设置 → 网络」里的代理地址与证书。",
  },
  SSH_PROXY_CONNECT_FAILED: {
    title: "连不上出站代理。",
    hint: "代理地址或端口不可达。检查「设置 → 网络」里的代理配置。",
  },
  SSH_PROXY_AUTH_REQUIRED: {
    title: "出站代理要求认证。",
    hint: "在「设置 → 网络」里补上代理用户名与密码后重试。",
  },
  SSH_UNAVAILABLE: {
    title: "SSH 服务没有就绪。",
    hint:
      "桌面宿主没能拉起 C++ SSH 工作进程。重新启动一次桌面应用；如果仍然出现，检查安装目录里的 SSH 工作进程是否被杀毒软件隔离。",
  },
  SSH_ERROR: GENERIC_ADVICE,
};

/**
 * 从抛出物里取宿主给的错误码。
 *
 * 两条来源都要认：
 *
 * - `error.code` —— 当前宿主把码放在 Error 的属性上（`main.cpp` 的 JS 桥）。
 * - 消息尾巴的 `(SSH_XXX)` —— 旧版宿主只把码拼进消息，用户可能还开着这样一个
 *   宿主进程，直到重启。认它，界面就不会在旧宿主上退化成通用文案。
 */
export function sshFailureCode(error: unknown): string | undefined {
  if (error && typeof error === "object") {
    const code = (error as { code?: unknown }).code;
    if (typeof code === "string" && code.length > 0) return code;
  }
  const message = error instanceof Error ? error.message : typeof error === "string" ? error : "";
  const trailing = /\(([A-Z][A-Z0-9_]*)\)\s*$/.exec(message);
  return trailing ? trailing[1] : undefined;
}

/**
 * 是否是一个 SSH 失败（码以 `SSH_` 开头）。
 *
 * 只认前缀：宿主还有 `STORAGE_ERROR`、`INVALID_PARAMS` 等码，它们与 SSH 无关，
 * 不该被套上「重新填密码」的建议。
 */
export function isSshFailure(error: unknown): boolean {
  return sshFailureCode(error)?.startsWith("SSH_") === true;
}

/**
 * 抛出物 → 可显示的建议。不是 SSH 失败时返回 `undefined`，调用方保留原有渲染。
 */
export function sshFailureAdvice(error: unknown): SshFailureAdvice | undefined {
  const code = sshFailureCode(error);
  if (!code || !code.startsWith("SSH_")) return undefined;
  // 不认识的码用通用文案，而不是把码本身当成解释。
  const advice = ADVICE[code] ?? GENERIC_ADVICE;
  return { code, title: advice.title, hint: advice.hint };
}

/**
 * 去掉消息尾巴的 `(SSH_XXX)`。
 *
 * 界面已经在旁边显示了码与解释，再把码重复贴在句尾只是噪音 —— 但**只在确实要
 * 显示码的地方**用它；日志与错误详情保留原话。
 */
export function stripFailureCode(message: string): string {
  return message.replace(/\s*\([A-Z][A-Z0-9_]*\)\s*$/, "");
}
