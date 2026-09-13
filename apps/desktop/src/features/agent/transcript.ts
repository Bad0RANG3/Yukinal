/**
 * Agent 动态（transcript）：对话流背后的纯词汇表与归约器。
 *
 * 这里刻意不依赖 React 与 Tauri。让动态可读的那些顺序规则 —— 历史条数上限、
 * 流式增量拼接、终态文本覆盖、工具调用与结果的配对、从本地记录恢复 ——
 * 因此可以单独测试，而不必把它们塞进一个渲染组件里靠人工点界面来验证。
 */

import type {
  ApprovalRequest,
  ChatMessage,
  PermissionApprovalSource,
  PermissionMode,
  RiskLevel,
  ToolTarget,
} from "@yukinal/shared";

import { environmentLabel } from "../../lib/labels.js";

/** 一次工具调用的身份与风险事实，来自 `agent.tool_call`。 */
export type ToolCallFacts = {
  callId: string;
  toolName: string;
  target: string;
  riskLevel: RiskLevel;
  decision: PermissionMode;
  approvedBy?: PermissionApprovalSource;
};

/** 这次调用最后怎么了，来自 `agent.tool_result`。 */
export type ToolOutcome = {
  status: "success" | "failed" | "cancelled";
  durationMs: number;
  summary: string;
};

/**
 * 动态里的一行。四类各自回答一个问题：
 * 你说了什么 / Agent 说了什么 / Agent 做了什么 / 需要你决定什么。
 *
 * 「做了什么」是**一张**卡片而不是两张：工具调用与它的结果讲的是同一件事，
 * 拆成两行只会让人在中间插入无关内容时读错因果。
 *
 * `system` 是**应用自己的声音**，不是模型的输出（例如 `/help` 的回复）。
 * 它必须与 assistant 分开：把应用的话冒充成 Agent 说的，就是在伪造记录。
 */
export type Entry =
  | { kind: "user"; text: string }
  | { kind: "assistant"; text: string }
  | { kind: "reasoning"; text: string }
  | { kind: "system"; text: string }
  | (ToolCallFacts & {
      kind: "tool";
      /** 结果到达前为空 —— 卡片此时显示「执行中」。 */
      result?: ToolOutcome;
    })
  | { kind: "approval"; approval: ApprovalRequest }
  | { kind: "error"; text: string };

/**
 * 长任务会一直往动态里追加。上限只截断最旧的行，不阻止新行写入，
 * 因此一次长时间运行不会因为内存而失败。
 */
export const MAX_TRANSCRIPT_ENTRIES = 500;

export function appendEntries(current: Entry[], additions: Entry[]): Entry[] {
  return [...current, ...additions].slice(-MAX_TRANSCRIPT_ENTRIES);
}

/**
 * 流式文本按 run 累积。事件乱序也没关系：同一 run 的增量永远追加到
 * 末尾那条 assistant 行上，而不是新起一行。
 */
export function appendReasoningDelta(current: Entry[], delta: string): Entry[] {
  if (delta.length === 0) return current;
  const last = current.at(-1);
  if (last?.kind === "reasoning") {
    return appendEntries(current.slice(0, -1), [{ kind: "reasoning", text: last.text + delta }]);
  }
  return appendEntries(current, [{ kind: "reasoning", text: delta }]);
}

export function appendAssistantDelta(current: Entry[], delta: string): Entry[] {
  if (delta.length === 0) return current;
  const last = current.at(-1);
  if (last?.kind === "assistant") {
    return appendEntries(current.slice(0, -1), [{ kind: "assistant", text: last.text + delta }]);
  }
  return appendEntries(current, [{ kind: "assistant", text: delta }]);
}

/**
 * 运行的终态文本是权威版本：它覆盖已流式显示的最后一行，
 * 而不是在其后再追加一条重复内容。
 */
export function settleAssistantText(current: Entry[], text: string): Entry[] {
  const last = current.at(-1);
  return last?.kind === "assistant"
    ? [...current.slice(0, -1), { kind: "assistant", text }]
    : appendEntries(current, [{ kind: "assistant", text }]);
}

/**
 * 开一张工具卡片。结果通常稍后才到，所以卡片先以「执行中」出现 ——
 * 这正是用户在等待时想看的东西。
 */
export function appendToolCall(current: Entry[], call: ToolCallFacts): Entry[] {
  return appendEntries(current, [{ kind: "tool", ...call }]);
}

/**
 * 把结果填回它那次调用。用 callId 配对而不是「最后一张卡片」——
 * 并发或乱序时后者会张冠李戴。
 *
 * 找不到对应调用时（结果先到、或从本地记录恢复后只拿到结果）就补一张
 * 已完成的卡片，绝不静默丢弃结果。
 */
export function settleToolResult(current: Entry[], call: ToolCallFacts, result: ToolOutcome): Entry[] {
  const index = current.findLastIndex(
    (entry) => entry.kind === "tool" && entry.callId === call.callId && entry.result === undefined,
  );
  if (index === -1) return appendEntries(current, [{ kind: "tool", ...call, result }]);
  const settled: Entry = { kind: "tool", ...call, result };
  return [...current.slice(0, index), settled, ...current.slice(index + 1)];
}

/** 从本地记录恢复一段对话。工具与审批只存在于实时动态中。 */
export function entriesFromMessages(messages: ChatMessage[]): Entry[] {
  return messages
    .flatMap((message): Entry[] => {
      if (message.role === "user") return [{ kind: "user", text: message.content }];
      if (message.role === "assistant") return [{ kind: "assistant", text: message.content }];
      return [];
    })
    .slice(-MAX_TRANSCRIPT_ENTRIES);
}

/** 首条消息即标题；Markdown 标题行会被丢掉，避免标题变成一整段正文。 */
export function sessionTitleFromPrompt(prompt: string): string {
  const title = prompt
    .replace(/^\s*#\s+[^\n]+\n?/gm, "")
    .replace(/\s+/g, " ")
    .trim();
  return (title || "未命名任务").slice(0, 48);
}

export function lastUserPrompt(entries: Entry[]): string | undefined {
  return [...entries].reverse().find((entry): entry is Extract<Entry, { kind: "user" }> => entry.kind === "user")?.text;
}

/**
 * 文件正文从不进入动态，只回一句说明；其余工具输出按长度截断。
 * 这条规则同时服务于隐私与可读性，所以它必须有一个名字。
 */
export function toolResultSummary(toolName: string, outputSummary: string): string {
  return toolName === "filesystem.read"
    ? "文件内容已返回给 Agent（正文不在动态中保存）"
    : outputSummary.slice(0, 240);
}

export const RUN_STATE_LABEL: Record<string, string> = {
  starting: "正在提交…",
  thinking: "思考中…",
  running_tool: "执行工具…",
  waiting_approval: "等待审批…",
  completed: "完成",
  failed: "失败",
  cancelled: "已停止",
};

export function runStateLabel(state: string | null): string | null {
  return state ? RUN_STATE_LABEL[state] ?? state : null;
}

export function targetLabel(target: ToolTarget): string {
  const scope = target.serverId ?? target.host;
  return `${scope} · ${environmentLabel(target.environment)}`;
}

/* `riskLabel` / `decisionLabel` / `approvalSourceLabel` / `resultLabel` 曾经定义在这里，
   现已移入 `lib/labels.js`。原因是活动页（`features/activity/ActivityFeed.tsx`）把
   同样四个词表又抄了一份，而两个 feature 目录互相 import 都不合适 —— 谁都不该拥有
   这份共享词汇。它们同时是安全叙事的一部分：Agent 面板和审计日志是同一个「要不要
   放行这次生产环境写入」决定的两个视图，措辞分叉就等于两处说法互相矛盾。
   调用方（`AgentEntryView.tsx`）改为直接从 `lib/labels.js` 引入。 */

