/**
 * 对话记录视图的纯逻辑：时间分组、搜索命中切段、目标名。
 *
 * 全部是纯函数 —— 不依赖 React、不依赖 Tauri、不读当前时间（`now` 由调用方传入），
 * 因此可以直接单测（见 `tests/history.test.ts`）。视图本身只负责把结果画出来。
 */

import type { ChatSession, ChatSessionCounts } from "@yukinal/shared";

const DAY_MS = 86_400_000;

/** 复盘一段对话时的三种看：正在进行、已经归档、以及两者一起搜。 */
export type HistoryFilter = "active" | "archived" | "all";

export type HistoryBucketKey = "today" | "yesterday" | "week" | "earlier";

export interface HistoryBucket {
  key: HistoryBucketKey;
  label: string;
  sessions: ChatSession[];
}

/**
 * 分组顺序：从最近到最远。键的顺序就是渲染顺序，所以它必须显式写出来，
 * 而不能依赖 Map 的插入顺序（那取决于数据是怎么排的）。
 */
const BUCKET_ORDER: HistoryBucketKey[] = ["today", "yesterday", "week", "earlier"];

const BUCKET_LABELS: Record<HistoryBucketKey, string> = {
  today: "今天",
  yesterday: "昨天",
  week: "最近 7 天",
  earlier: "更早",
};

/**
 * 「谁更近」按**本地日历日**算，不按毫秒差。
 *
 * 夏令时切换那天只有 23 小时，用 `(今天零点 - 那天零点) / 86400000` 会把相邻两天
 * 算成 0.96 天，于是「昨天」的对话落进「今天」。先把本地年月日搬到 UTC（那里没有
 * 夏令时），再做整数天减法，就不会有这个偏差。
 */
function calendarDay(ms: number): number {
  const date = new Date(ms);
  return Math.floor(Date.UTC(date.getFullYear(), date.getMonth(), date.getDate()) / DAY_MS);
}

export function historyBucketOf(updatedAt: string, now: number): HistoryBucketKey {
  const time = new Date(updatedAt).getTime();
  // 读不懂的时间戳不猜日期，直接放进「更早」：它能被看见，但不会冒充今天发生的事。
  if (Number.isNaN(time)) return "earlier";
  const days = calendarDay(now) - calendarDay(time);
  // 未来的时间戳（两侧时钟不一致）按「今天」，与 `formatRelativeTime` 的取舍一致。
  if (days <= 0) return "today";
  if (days === 1) return "yesterday";
  if (days <= 6) return "week";
  return "earlier";
}

/** 按更新时间分组，保持传入顺序；空分组不出现。 */
export function groupSessionsByDay(sessions: ChatSession[], now: number): HistoryBucket[] {
  const grouped = new Map<HistoryBucketKey, ChatSession[]>();
  for (const session of sessions) {
    const key = historyBucketOf(session.updatedAt, now);
    const bucket = grouped.get(key);
    if (bucket) bucket.push(session);
    else grouped.set(key, [session]);
  }
  const buckets: HistoryBucket[] = [];
  for (const key of BUCKET_ORDER) {
    const bucket = grouped.get(key);
    if (bucket?.length) buckets.push({ key, label: BUCKET_LABELS[key], sessions: bucket });
  }
  return buckets;
}

export interface MatchSegment {
  text: string;
  /** true 表示这一段是被搜到的部分，渲染时加底色。 */
  match: boolean;
}

/**
 * 把一段文本按搜索词切成「命中」与「未命中」两种片段。
 *
 * 返回数据而不是带 `<mark>` 的 HTML：命中高亮是**不可信文本**进入界面的又一条路径
 * （标题来自用户输入、预览来自模型输出），所以它必须和 Markdown 渲染走同一条规矩 ——
 * 只产出元素，不注入标记（ADR 0015）。
 *
 * 有界：输入是标题（≤200 字符）与预览（≤240 字符），所以片段数天然有上限，
 * 不需要在这里再设一个阈值。
 */
export function matchSegments(text: string, query: string): MatchSegment[] {
  const needle = query.trim().toLowerCase();
  if (!needle) return [{ text, match: false }];
  const haystack = text.toLowerCase();
  // `toLowerCase` 不保证长度不变（"İ" 会变成两个码元），长度一变，按下标切片就会切到
  // 错的位置。与其猜，不如放弃高亮：文本照常显示，只是没有底色。
  if (haystack.length !== text.length) return [{ text, match: false }];
  const segments: MatchSegment[] = [];
  let from = 0;
  for (;;) {
    const at = haystack.indexOf(needle, from);
    if (at < 0) break;
    if (at > from) segments.push({ text: text.slice(from, at), match: false });
    segments.push({ text: text.slice(at, at + needle.length), match: true });
    from = at + needle.length;
  }
  if (from < text.length) segments.push({ text: text.slice(from), match: false });
  return segments.length ? segments : [{ text, match: false }];
}

/**
 * 一条记录的目标该叫什么。
 *
 * 显示服务器**名字**而不是 `srv_...` id：id 是不透明的，它对使用者不回答任何问题，
 * 而「这段对话是冲着哪台机器去的」正是列表要回答的问题。服务器已经不在了就说出来，
 * 也不回落成 id。
 */
export function historyTargetLabel(
  session: Pick<ChatSession, "serverId">,
  servers: { id: string; name: string }[],
): string {
  if (!session.serverId) return "全局工作区";
  return servers.find((server) => server.id === session.serverId)?.name ?? "已移除的服务器";
}

/** 当前筛选下「一共有多少条」，用于判断还能不能再翻一页。 */
export function historyFilterTotal(counts: ChatSessionCounts, filter: HistoryFilter): number {
  if (filter === "active") return counts.active;
  if (filter === "archived") return counts.archived;
  return counts.active + counts.archived;
}

/** 筛选 → `chat_session_list` 的 `archived` 参数；「全部」必须是不传这个字段。 */
export function historyFilterParam(filter: HistoryFilter): boolean | undefined {
  if (filter === "active") return false;
  if (filter === "archived") return true;
  return undefined;
}
