/**
 * MCP 设置（ADR 0014）：查询、四个动作，以及「这一行现在是什么状态」这条纯规则。
 *
 * 与 `lib/host-key.ts`、`lib/servers.ts` 同一套写法（查询键常量 + `isDesktopShell()` 门控 +
 * 动作成功后就地失效），所以这里没有第二套约定。
 *
 * 这个文件里唯一值得单独说的地方是**它不自己判断 http 能不能用**。`transport: "http"` 的
 * 拒绝理由由 Rust 的 `McpStdioConfig::from_server_config` 给出，界面只负责把它照原样显示出来
 * （`mcpUnavailableText`）。在这里再写一遍「不支持 http」会造出第二个真相来源：哪天后端支持了，
 * 界面会继续拒绝；哪天后端换了措辞，用户会看到两句不一样的解释。
 */

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  IPC_COMMANDS,
  type McpServerDeleteResponse,
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

  return {
    save,
    remove,
    start,
    stop,
    busy: save.isPending || remove.isPending || start.isPending || stop.isPending,
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
  enabled: boolean;
}

export function emptyDraft(): McpServerDraft {
  return { id: "", label: "", transport: "stdio", command: "", args: "", enabled: false };
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
    enabled: config.enabled,
  };
}

/**
 * 表单 → 保存入参。
 *
 * 只做**搬运**：trim 与丢掉空行是界面自己的事（文本框里的换行不代表一个空参数），
 * 而「id/label 不能为空」「http 不能用」都由后端决定并把理由带回来。
 */
export function toSaveInput(draft: McpServerDraft): McpServerSaveInput {
  const args = draft.args
    .split("\n")
    .map((line) => line.trim())
    .filter((line) => line.length > 0);
  const command = draft.command.trim();
  return {
    id: draft.id.trim(),
    label: draft.label.trim(),
    transport: draft.transport.trim(),
    ...(command.length === 0 ? {} : { command }),
    ...(args.length === 0 ? {} : { args }),
    enabled: draft.enabled,
  };
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
  // 崩过与从未启动过是两件不同的事：「没在跑」会让用户去点「启动」，而一台崩过的服务器
  // 需要的是先看退出记录（ADR 0014 不自动重启）。
  if (view.status.lastExit) return { running: false, text: "已退出" };
  return { running: false, text: "未启动" };
}

/**
 * 「现在为什么不能用」那一句。没有已知阻碍时返回 `null`。
 *
 * `unavailable.message` 来自 Rust，**原样显示**：它里面写着下一步该做什么
 * （`transport_not_implemented` 会解释出站网络策略，`exited` 会带上退出码并说明不会自动重启）。
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

/**
 * stderr 尾部与诊断信息。用 `<pre>` 之外的普通文本行呈现，并且**不做截断之外的处理**：
 * 这些行由 Rust 侧截断并脱敏过（`crates/core/src/mcp/mod.rs`），界面再脱敏一次会让两边
 * 的规则开始分叉。
 */
export function serverMessages(view: McpServerView): string[] {
  const stderr = view.status.stderrTail.map((line) => `stderr: ${line}`);
  return [...view.status.diagnostics, ...stderr];
}
