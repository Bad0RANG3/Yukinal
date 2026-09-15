/**
 * MCP 设置（ADR 0014）：查询、四个动作，以及「这一行现在是什么状态」这条纯规则。
 *
 * 与 `lib/host-key.ts`、`lib/servers.ts` 同一套写法（查询键常量 + `isDesktopShell()` 门控 +
 * 动作成功后就地失效），所以这里没有第二套约定。
 *
 * 这个文件里唯一值得单独说的地方是**它不重复实现端点策略**。URL 的 HTTPS/回环规则与
 * 可连接性由 Rust 的 `McpHttpConfig` 决定，界面只负责把后端返回的理由照原样显示出来
 * （`mcpUnavailableText`）。
 */

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  IPC_COMMANDS,
  type McpOAuthClientAuth,
  type McpOAuthCancelResponse,
  type McpOAuthConnectResponse,
  type McpOAuthDeviceCodeEvent,
  type McpOAuthFlow,
  type McpServerDeleteResponse,
  type McpServerReviewInput,
  type McpServerSaveInput,
  type McpServerStopResponse,
  type McpServerView,
} from "@yukinal/shared";

import { callDesktop, isDesktopShell } from "./ipc.js";

/**
 * MCP 服务器列表的缓存键。
 *
 * 列表里既有存下来的配置，也有**进程状态**（pid、退出记录）。两者都会因为界面之外的事情
 * 变化（服务器自己崩了），所以 `staleTime` 是 0：打开设置就要重新问一次，而不是拿上一次
 * 打开时的 pid 当现在。
 */
export const MCP_SERVERS_QUERY_KEY = ["mcp-servers"] as const;

/**
 * 列表查询。**只读**：它不会启动任何服务器进程（后端那边 `mcp_server_list` 也不启动）。
 */
export function useMcpServers(options: { enabled?: boolean } = {}) {
  const enabled = (options.enabled ?? true) && isDesktopShell();
  return useQuery({
    queryKey: MCP_SERVERS_QUERY_KEY,
    enabled,
    staleTime: 0,
    queryFn: async () => callDesktop(IPC_COMMANDS.mcpServerList, {}),
  });
}

export interface McpServerActions {
  save: ReturnType<typeof useMutation<McpServerView, Error, McpServerSaveInput>>;
  remove: ReturnType<typeof useMutation<McpServerDeleteResponse, Error, string>>;
  start: ReturnType<typeof useMutation<McpServerView, Error, string>>;
  stop: ReturnType<typeof useMutation<McpServerStopResponse, Error, string>>;
  review: ReturnType<typeof useMutation<McpServerView, Error, McpServerReviewInput>>;
  oauthConnect: ReturnType<typeof useMutation<McpOAuthConnectResponse, Error, string>>;
  oauthCancel: ReturnType<typeof useMutation<McpOAuthCancelResponse, Error, string>>;
  /** 有没有动作在跑（按钮一律禁用：列表会被失效，边改边看会看到一半的旧状态）。 */
  busy: boolean;
}

/**
 * 四个动作。每一个都改 supervisor 里的进程状态，所以每一个成功之后都必须让列表重新读一次；
 * 用 `onSettled` 而不是 `onSuccess`，因为**失败也会改变状态**（启动失败会留下一条退出记录，
 * 而那正是用户需要看到的东西 —— 列表不刷新的话，界面会继续显示「没在跑」而不说为什么）。
 */
export function useMcpServerActions(): McpServerActions {
  const queryClient = useQueryClient();
  const refresh = () => queryClient.invalidateQueries({ queryKey: MCP_SERVERS_QUERY_KEY });

  const save = useMutation<McpServerView, Error, McpServerSaveInput>({
    mutationFn: (input) => callDesktop(IPC_COMMANDS.mcpServerSave, { input }),
    onSettled: () => void refresh(),
  });
  const remove = useMutation<McpServerDeleteResponse, Error, string>({
    mutationFn: (serverId) => callDesktop(IPC_COMMANDS.mcpServerDelete, { serverId }),
    onSettled: () => void refresh(),
  });
  const start = useMutation<McpServerView, Error, string>({
    mutationFn: (serverId) => callDesktop(IPC_COMMANDS.mcpServerStart, { serverId }),
    onSettled: () => void refresh(),
  });
  const stop = useMutation<McpServerStopResponse, Error, string>({
    mutationFn: (serverId) => callDesktop(IPC_COMMANDS.mcpServerStop, { serverId }),
    onSettled: () => void refresh(),
  });
  const review = useMutation<McpServerView, Error, McpServerReviewInput>({
    mutationFn: (input) => callDesktop(IPC_COMMANDS.mcpServerReview, input),
    onSettled: () => void refresh(),
  });
  const oauthConnect = useMutation<McpOAuthConnectResponse, Error, string>({
    mutationFn: (serverId) => callDesktop(IPC_COMMANDS.mcpOAuthConnect, { serverId }),
    onSettled: () => void refresh(),
  });
  // 取消不改数据库，但它结束一个正在等待的授权，而那之后列表里的状态（有没有拿到令牌）
  // 仍然要重新读一次：`onSettled` 与其余动作保持一致。
  const oauthCancel = useMutation<McpOAuthCancelResponse, Error, string>({
    mutationFn: (serverId) => callDesktop(IPC_COMMANDS.mcpOAuthCancel, { serverId }),
    onSettled: () => void refresh(),
  });

  return {
    save,
    remove,
    start,
    stop,
    review,
    oauthConnect,
    oauthCancel,
    busy:
      save.isPending ||
      remove.isPending ||
      start.isPending ||
      stop.isPending ||
      review.isPending ||
      oauthConnect.isPending ||
      oauthCancel.isPending,
  };
}

/* ── 纯规则 ───────────────────────────────────────────────────────────────── */

/** 表单里的内容。与 `McpServerSaveInput` 同形，只是 `args` 是**一行一个**的文本框。 */
export interface McpServerDraft {
  id: string;
  label: string;
  transport: string;
  command: string;
  args: string;
  url: string;
  authMode: "none" | "static" | "oauth";
  /** One `Header-Name: secret` per line; an empty secret preserves that existing header. */
  httpAuthHeaders: string;
  oauthIssuer: string;
  oauthClientId: string;
  /** Which OAuth flow the connect action runs. Part of the stored identity. */
  oauthFlow: McpOAuthFlow;
  /** How the client authenticates at the token endpoint. Also part of the identity. */
  oauthClientAuth: McpOAuthClientAuth;
  /** Write-only: never filled from the server, and empty means "keep the stored one". */
  oauthClientSecret: string;
  /** Ask for sender-constrained tokens (RFC 9449 DPoP). Part of the stored identity. */
  oauthDpop: boolean;
  oauthScopes: string;
  enabled: boolean;
}

export function emptyDraft(): McpServerDraft {
  return {
    id: "",
    label: "",
    transport: "stdio",
    command: "",
    args: "",
    url: "",
    authMode: "none",
    httpAuthHeaders: "",
    oauthIssuer: "",
    oauthClientId: "",
    oauthFlow: "authorization_code",
    oauthClientAuth: "none",
    oauthClientSecret: "",
    oauthDpop: false,
    oauthScopes: "",
    enabled: false,
  };
}

/** 从一行已存的配置填出表单。 */
export function draftFromServer(config: McpServerView["config"]): McpServerDraft {
  return {
    id: config.id,
    label: config.label,
    transport: config.transport,
    command: config.command ?? "",
    // 一行一个参数，而不是逗号分隔：参数里出现逗号是很正常的（`--label a,b`），
    // 用逗号分隔会把一个参数变成两个，而且用户看不出来。
    args: (config.args ?? []).join("\n"),
    url: config.url ?? "",
    authMode: config.oauth ? "oauth" : config.httpAuthHeaders.length > 0 ? "static" : "none",
    httpAuthHeaders: config.httpAuthHeaders
      .map((header) => `${header.name}:`)
      .join("\n"),
    oauthIssuer: config.oauth?.issuer ?? "",
    oauthClientId: config.oauth?.clientId ?? "",
    oauthFlow: config.oauth?.flow ?? "authorization_code",
    oauthClientAuth: config.oauth?.clientAuth ?? "none",
    // Deliberately not read from anywhere: the server never sends a secret back, and a form
    // that prefilled this field would be showing a credential the row does not contain.
    oauthClientSecret: "",
    oauthDpop: config.oauth?.dpop ?? false,
    oauthScopes: (config.oauth?.scopes ?? []).join("\n"),
    enabled: config.enabled,
  };
}

/**
 * 表单 → 保存入参。
 *
 * 只做**搬运**：trim 与丢掉空行是界面自己的事（文本框里的换行不代表一个空参数），
 * 而 id、label 和端点是否合法都由后端决定并把理由带回来。
 */
export function toSaveInput(draft: McpServerDraft): McpServerSaveInput {
  const transport = draft.transport.trim();
  const stdio = transport === "stdio";
  const http = transport === "http";
  const args = draft.args
    .split("\n")
    .map((line) => line.trim())
    .filter((line) => line.length > 0);
  const command = draft.command.trim();
  const httpAuthHeaders = parseHttpAuthHeaders(draft.httpAuthHeaders);
  const oauthScopes = draft.oauthScopes
    .split(/[\s,]+/)
    .map((scope) => scope.trim())
    .filter((scope) => scope.length > 0);
  return {
    id: draft.id.trim(),
    label: draft.label.trim(),
    transport,
    ...(stdio && command.length > 0 ? { command } : {}),
    ...(stdio && args.length > 0 ? { args } : {}),
    ...(http && draft.url.trim().length > 0 ? { url: draft.url.trim() } : {}),
    ...(http && draft.authMode === "static" && httpAuthHeaders.length > 0
      ? { httpAuthHeaders }
      : {}),
    ...(http && draft.authMode === "oauth"
      ? {
          oauth: {
            issuer: draft.oauthIssuer.trim(),
            clientId: draft.oauthClientId.trim(),
            flow: draft.oauthFlow,
            clientAuth: draft.oauthClientAuth,
            dpop: draft.oauthDpop,
            // Trimmed only in the sense of "not sent when empty": the secret itself is sent
            // verbatim, and Rust refuses control characters rather than silently fixing them.
            ...(draft.oauthClientSecret.length > 0
              ? { clientSecret: draft.oauthClientSecret }
              : {}),
            scopes: oauthScopes,
          },
        }
      : {}),
    enabled: draft.enabled,
  };
}

function parseHttpAuthHeaders(value: string): Array<{ name: string; secret?: string }> {
  return value
    .split("\n")
    .map((line) => line.trim())
    .filter((line) => line.length > 0)
    .map((line) => {
      const separator = line.indexOf(":");
      if (separator < 0) return { name: line };
      const name = line.slice(0, separator).trim();
      const secret = line
        .slice(separator + 1)
        .replace(/^\s/, "")
        .trimEnd();
      return secret.length > 0 ? { name, secret } : { name };
    });
}

/**
 * 输入（或正在输入的）服务器 id 会不会变成内部名。
 *
 * 这是**提示**，不是校验：真正的名字由后端规范化并可能被拒绝（`mcp..1` 就永远变不成段）。
 * 提前算出来是因为「你的工具会叫 `mcp.mcp-1.echo`」是用户按下保存之前就想知道的事，
 * 而不是保存之后从报错里猜。
 */
export function previewToolNamespace(id: string): string | null {
  const trimmed = id.trim().toLowerCase();
  if (trimmed.length === 0) return null;
  const segment = trimmed.replace(/_/g, "-");
  if (!/^[a-z][a-z0-9]*(-[a-z0-9]+)*$/.test(segment)) return null;
  return `mcp.${segment}.*`;
}

/** 状态徽标：跑着 / 没跑。 */
export function statusLabel(view: McpServerView): { running: boolean; text: string } {
  if (view.status.running) return { running: true, text: "运行中" };
  if (view.status.restart?.exhausted) return { running: false, text: "恢复已停止" };
  if (view.status.restart) return { running: false, text: "正在恢复" };
  // A crash and a never-started row are different: both are stopped, but the crash has
  // an exit record that explains why.
  if (view.status.lastExit) return { running: false, text: "已退出" };
  return { running: false, text: "未启动" };
}

/**
 * 「现在为什么不能用」那一句。没有已知阻碍时返回 `null`。
 *
 * `unavailable.message` 来自 Rust，**原样显示**：它里面写着下一步该做什么
 * （`invalid_config` 会指出端点规则，`exited` 会带上退出码并说明不会自动重启）。
 */
export function mcpUnavailableText(view: McpServerView): string | null {
  return view.unavailable?.message ?? null;
}

/** 退出记录那一行（崩过才有）。 */
export function mcpExitText(view: McpServerView): string | null {
  const exit = view.status.lastExit;
  if (!exit) return null;
  return `上次退出：${exit.reason}（${exit.at}）`;
}

/* ── 设备码（RFC 8628）的纯规则 ────────────────────────────────────────────── */

/**
 * 这种客户端认证方式会不会用到已存的 client secret。
 *
 * 与 Rust 的 `McpOAuthClientAuth::needs_secret` 是同一条规则：界面用它决定「要不要显示密钥
 * 输入框」，后端用它决定「要不要读凭据库」。两边都从这一个判断出发，所以不会出现「界面
 * 显示了一个永远不会被读的输入框」。
 */
export function oauthClientAuthNeedsSecret(auth: McpOAuthClientAuth): boolean {
  return auth !== "none";
}

/**
 * 要打开的验证链接。
 *
 * 服务器给了 `verification_uri_complete` 就用它：那个链接里已经带着 code，点一下就行，
 * 而纯 `verification_uri` 需要用户自己把屏幕上那串字符敲进去。两者都会显示，所以浏览器
 * 没有预填时用户仍然有办法。
 */
export function deviceCodeLink(prompt: McpOAuthDeviceCodeEvent): string {
  return prompt.verificationUriComplete ?? prompt.verificationUri;
}

/**
 * 验证码的有效期提示。`null` 表示**没有**需要提醒的事。
 *
 * 当前时间由参数传入而不是在函数里读时钟，「还剩几分钟」才是一条可以被断言的话。
 */
export function deviceCodeExpiryText(
  prompt: McpOAuthDeviceCodeEvent,
  now: number,
): string | null {
  const expiresAt = Date.parse(prompt.expiresAt);
  if (Number.isNaN(expiresAt)) return null;
  const remaining = expiresAt - now;
  if (remaining <= 0) return "验证码已过期：重新发起授权会拿到一个新的代码。";
  return `验证码大约在 ${Math.ceil(remaining / 60_000)} 分钟后过期。`;
}

/** Bounded automatic-recovery progress, when a crash had a restart attempt. */
export function mcpRestartText(view: McpServerView): string | null {
  const restart = view.status.restart;
  if (!restart) return null;
  if (view.status.running) {
    return `已自动恢复：第 ${restart.attempt}/${restart.maxAttempts} 次尝试成功；这次只重建进程与工具目录，没有重放中断的调用。`;
  }
  return restart.exhausted
    ? `自动恢复已停止：${restart.attempt}/${restart.maxAttempts} 次尝试均未成功。`
    : `自动恢复中：第 ${restart.attempt}/${restart.maxAttempts} 次尝试，仅重建进程与工具目录，不重放刚才的调用。`;
}

/**
 * stderr 尾部与诊断信息。用 `<pre>` 之外的普通文本行呈现，并且**不做截断之外的处理**：
 * 这些行由 Rust 侧截断并脱敏过（`crates/core/src/mcp/mod.rs`），界面再脱敏一次会让两边
 * 的规则开始分叉。
 */
export function serverMessages(view: McpServerView): string[] {
  const stderr = view.status.stderrTail.map((line) => `stderr: ${line}`);
  return [...view.status.diagnostics, ...stderr];
}
