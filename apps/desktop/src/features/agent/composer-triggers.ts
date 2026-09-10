/**
 * 输入框里的触发词：斜杠命令与 @ 提及。
 *
 * 这两件事共用同一条规则 —— 「在光标处找一个尚未完成的触发词」—— 而且都需要
 * 在不接触 DOM 的前提下被验证。所以解析、过滤、替换、提交判定全部放在这里，
 * 组件只负责把结果画出来并处理键盘。
 *
 * 一条贯穿始终的原则：不猜。用户打出的 `/` 后面跟了不认识的名字时，
 * 宁可明确报错，也不静默当成普通提问发出去 —— 那会让一个笔误变成一次
 * 真实的远端操作。
 */

export const COMMAND_PREFIX = "/";
export const MENTION_PREFIX = "@";

export type TriggerKind = "command" | "mention";

/** 光标处一个尚未被空白终止的触发词。 */
export type ActiveTrigger = {
  kind: TriggerKind;
  /** 触发符本身在文本中的下标。 */
  start: number;
  /** 光标位置（不含）。 */
  end: number;
  /** 触发符之后、光标之前已输入的内容。 */
  query: string;
};

function isBoundary(text: string, index: number): boolean {
  return index === 0 || /\s/.test(text[index - 1] ?? "");
}

/**
 * 从光标往前找触发词。碰到空白就说明这个词已经打完了，不再是待选状态。
 */
export function findActiveTrigger(text: string, caret: number): ActiveTrigger | null {
  const before = text.slice(0, Math.max(0, Math.min(caret, text.length)));
  for (let i = before.length - 1; i >= 0; i -= 1) {
    const ch = before[i];
    if (ch === " " || ch === "\n" || ch === "\t") return null;
    if (ch !== COMMAND_PREFIX && ch !== MENTION_PREFIX) continue;
    if (!isBoundary(before, i)) return null;
    const kind: TriggerKind = ch === COMMAND_PREFIX ? "command" : "mention";
    // 命令作用于整条输入，因此只能出现在最前面。
    if (kind === "command" && i !== 0) return null;
    return { kind, start: i, end: before.length, query: before.slice(i + 1) };
  }
  return null;
}

/** 用选中的候选替换触发词，并把光标停在插入内容之后。 */
export function replaceTrigger(
  text: string,
  trigger: ActiveTrigger,
  insertion: string,
): { text: string; caret: number } {
  const tail = text.slice(trigger.end);
  const needsSpace = tail.length === 0 || !/^\s/.test(tail);
  const inserted = needsSpace ? `${insertion} ` : insertion;
  const next = text.slice(0, trigger.start) + inserted + tail;
  return { text: next, caret: trigger.start + inserted.length };
}

/* ------------------------------------------------------------------ 命令 */

/**
 * 命令的前置条件写成声明而不是谓词：这样「什么时候能用」可以被穷举测试，
 * 也不会因为某个命令忘记处理某个状态而出现幽灵入口。
 */
export type CommandRequirement = "always" | "idle" | "running" | "desktop";

export type CommandSpec = {
  name: string;
  description: string;
  requires: CommandRequirement;
};

export const AGENT_COMMANDS: readonly CommandSpec[] = [
  { name: "new", description: "开始一段新对话", requires: "idle" },
  { name: "clear", description: "清空当前动态", requires: "idle" },
  { name: "history", description: "打开对话记录", requires: "desktop" },
  { name: "stop", description: "停止正在运行的 Agent", requires: "running" },
  { name: "help", description: "查看可用命令", requires: "always" },
];

export type CommandContext = {
  /** 是否有运行在途。 */
  running: boolean;
  /** 当前对话是否已归档（归档后不能继续发送）。 */
  archived: boolean;
  /** 是否运行在桌面壳里 —— 「对话记录」依赖本机数据库。 */
  desktop: boolean;
};

/** 不可用的原因，用来在候选列表里直接说明为什么不能用。 */
export function unavailableReason(spec: CommandSpec, context: CommandContext): string | null {
  switch (spec.requires) {
    case "always":
      return null;
    case "idle":
      if (context.running) return "运行中不可用";
      if (context.archived) return "对话已归档";
      return null;
    case "running":
      return context.running ? null : "当前没有运行";
    case "desktop":
      return context.desktop ? null : "仅桌面应用可用";
  }
}

export function commandAvailable(spec: CommandSpec, context: CommandContext): boolean {
  return unavailableReason(spec, context) === null;
}

export type CommandMatch = { spec: CommandSpec; reason: string | null };

/**
 * 候选项保留不可用的命令并附上原因：让用户看到「有这个命令，但现在不能用，
 * 因为正在运行」，比让它凭空消失更有教育意义。
 */
export function matchCommands(query: string, context: CommandContext): CommandMatch[] {
  const needle = query.trim().toLowerCase();
  return AGENT_COMMANDS
    .filter((spec) => spec.name.startsWith(needle))
    .map((spec) => ({ spec, reason: unavailableReason(spec, context) }));
}

/** `/help` 的正文：直接由命令表生成，因此永远不会和实际能力脱节。 */
export function commandHelpText(context: CommandContext): string {
  return [
    "可用命令：",
    ...AGENT_COMMANDS.map((spec) => {
      const reason = unavailableReason(spec, context);
      return `/${spec.name} — ${spec.description}${reason ? `（${reason}）` : ""}`;
    }),
    "",
    "输入 @ 可以提及服务器，把这次运行的目标指向它。",
  ].join("\n");
}

/* --------------------------------------------------------------- 提交判定 */

export type Submission =
  | { kind: "command"; spec: CommandSpec; args: string }
  /** 以 / 开头但没有对应命令 —— 必须报错，不能当成提问发出去。 */
  | { kind: "unknown"; name: string }
  | { kind: "prompt"; text: string };

export function resolveSubmission(text: string): Submission {
  const trimmed = text.trim();
  if (!trimmed.startsWith(COMMAND_PREFIX)) return { kind: "prompt", text: trimmed };
  const [name = "", ...rest] = trimmed.slice(COMMAND_PREFIX.length).split(/\s+/);
  const spec = AGENT_COMMANDS.find((candidate) => candidate.name === name.toLowerCase());
  if (!spec) return { kind: "unknown", name };
  return { kind: "command", spec, args: rest.join(" ") };
}

/* ----------------------------------------------------------------- 提及 */

export type MentionCandidate = {
  id: string;
  label: string;
  /** 次要说明，例如环境或主机。 */
  detail?: string;
};

/** 前缀命中排在包含命中之前，这样输入 `prod` 时 `prod-1` 不被 `staging-prod` 挤掉。 */
export function matchMentions(
  candidates: readonly MentionCandidate[],
  query: string,
  limit = 6,
): MentionCandidate[] {
  const needle = query.trim().toLowerCase();
  const scored = candidates
    .map((candidate) => {
      const label = candidate.label.toLowerCase();
      if (needle.length === 0) return { candidate, rank: 2 };
      if (label.startsWith(needle)) return { candidate, rank: 0 };
      if (label.includes(needle)) return { candidate, rank: 1 };
      return null;
    })
    .filter((entry): entry is { candidate: MentionCandidate; rank: number } => entry !== null)
    .sort((a, b) => a.rank - b.rank || a.candidate.label.localeCompare(b.candidate.label));
  return scored.slice(0, limit).map((entry) => entry.candidate);
}

/**
 * 找出提示词里第一个被 @ 提到的已知服务器。
 *
 * 只认完整标签而不是任意子串：服务器名可能互相包含，按子串匹配会把
 * `@prod` 错认成 `@prod-1`。这里要求提及后面紧跟词边界。
 */
export function resolveMentionedServer(
  text: string,
  candidates: readonly MentionCandidate[],
): MentionCandidate | null {
  // 长标签优先，避免短名抢先命中长名的一部分。
  const ordered = [...candidates].sort((a, b) => b.label.length - a.label.length);
  let best: { candidate: MentionCandidate; at: number } | null = null;
  for (const candidate of ordered) {
    const needle = `${MENTION_PREFIX}${candidate.label}`;
    for (let from = 0; ; ) {
      const at = text.indexOf(needle, from);
      if (at === -1) break;
      const after = text[at + needle.length] ?? "";
      if (isBoundary(text, at) && (after === "" || /\s/.test(after))) {
        if (!best || at < best.at) best = { candidate, at };
        break;
      }
      from = at + 1;
    }
  }
  return best?.candidate ?? null;
}
