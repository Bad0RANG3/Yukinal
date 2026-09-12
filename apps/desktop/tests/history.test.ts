/**
 * 对话记录视图的纯逻辑：分组、命中切段、目标名。
 *
 * 这些函数里有几处是**故意的取舍**（不能按毫秒差分组、不能把 id 当名字显示），
 * 断言写在这里，免得日后被当成「实现细节」顺手改掉。
 */

import assert from "node:assert/strict";
import { test } from "node:test";

import type { ChatSession } from "@yukinal/shared";

import {
  groupSessionsByDay,
  historyBucketOf,
  historyFilterParam,
  historyFilterTotal,
  historyTargetLabel,
  matchSegments,
} from "../src/features/agent/history.js";

/** 本地时间构造 ISO 串：这些用例说的是「本地日历日」，用 UTC 写会读不出意图。 */
function localIso(year: number, month: number, day: number, hour = 12, minute = 0): string {
  return new Date(year, month - 1, day, hour, minute).toISOString();
}

function session(id: string, updatedAt: string, extra: Partial<ChatSession> = {}): ChatSession {
  return {
    id,
    title: id,
    createdAt: updatedAt,
    updatedAt,
    messageCount: 1,
    ...extra,
  };
}

test("buckets follow the local calendar day, not a rolling 24-hour window", () => {
  const now = new Date(2026, 2, 10, 23, 0).getTime();
  // 46 小时前 —— 毫秒差上是「将近两天」，日历上是「昨天」。分组跟着日历走：
  // 「昨天」对使用者来说是一段时间的名字，不是 24 小时的减法。
  assert.equal(historyBucketOf(localIso(2026, 3, 9, 1), now), "yesterday");
  // 22 小时前，但仍是同一天。
  assert.equal(historyBucketOf(localIso(2026, 3, 10, 1), now), "today");
});

test("every bucket boundary lands where the label says it does", () => {
  const now = new Date(2026, 2, 10, 9, 0).getTime();
  assert.equal(historyBucketOf(localIso(2026, 3, 10, 0, 1), now), "today");
  assert.equal(historyBucketOf(localIso(2026, 3, 9, 23, 59), now), "yesterday");
  assert.equal(historyBucketOf(localIso(2026, 3, 8, 9, 0), now), "week");
  assert.equal(historyBucketOf(localIso(2026, 3, 4, 9, 0), now), "week");
  assert.equal(historyBucketOf(localIso(2026, 3, 3, 9, 0), now), "earlier");
  assert.equal(historyBucketOf(localIso(2025, 12, 31, 9, 0), now), "earlier");
});

test("a clock that runs behind the host does not claim a conversation is from the future", () => {
  // 记录里的时间由宿主写：本机时钟回拨时会出现「十分钟后发生的事」。它既不该被算成
  // 「更早」，也不该让分组崩掉 —— 按今天处理。
  const now = new Date(2026, 2, 10, 9, 0).getTime();
  assert.equal(historyBucketOf(localIso(2026, 3, 10, 9, 30), now), "today");
});

test("an unparsable timestamp is shown, but never as something recent", () => {
  const now = new Date(2026, 2, 10, 9, 0).getTime();
  assert.equal(historyBucketOf("not a date", now), "earlier");
  assert.equal(historyBucketOf("", now), "earlier");
});

test("groups keep the order they were given, drop empty buckets, and count what is in them", () => {
  const now = new Date(2026, 2, 10, 12, 0).getTime();
  const groups = groupSessionsByDay(
    [
      session("a", localIso(2026, 3, 10, 11, 0)),
      session("b", localIso(2026, 3, 10, 8, 0)),
      session("c", localIso(2026, 3, 9, 22, 0)),
      session("d", localIso(2026, 1, 2, 8, 0)),
    ],
    now,
  );
  assert.deepEqual(
    groups.map((bucket) => [bucket.key, bucket.label, bucket.sessions.map((item) => item.id)]),
    [
      ["today", "今天", ["a", "b"]],
      ["yesterday", "昨天", ["c"]],
      ["earlier", "更早", ["d"]],
    ],
  );
  // 「最近 7 天」这一段没有任何记录，于是它整体不出现 —— 空标题只会让人以为列表出错了。
  assert.equal(groups.some((bucket) => bucket.key === "week"), false);
});

test("a search match is split into segments so the renderer never builds HTML", () => {
  assert.deepEqual(matchSegments("排查 nginx 502", "nginx"), [
    { text: "排查 ", match: false },
    { text: "nginx", match: true },
    { text: " 502", match: false },
  ]);
  // 同一个词出现多次时每一处都要标出来。
  assert.deepEqual(matchSegments("api/api", "api"), [
    { text: "api", match: true },
    { text: "/", match: false },
    { text: "api", match: true },
  ]);
  // 大小写不敏感：搜索词不要求用户记住标题是怎么写的。
  assert.deepEqual(matchSegments("Nginx", "nginx"), [{ text: "Nginx", match: true }]);
});

test("a query that matches nothing still renders the whole text", () => {
  assert.deepEqual(matchSegments("扩容磁盘", "nginx"), [{ text: "扩容磁盘", match: false }]);
  assert.deepEqual(matchSegments("扩容磁盘", "   "), [{ text: "扩容磁盘", match: false }]);
});

test("a text whose lower-case form changes length gives up highlighting instead of cutting wrong", () => {
  // "İ".toLowerCase() 是两个码元：一旦按小写串的下标去切原文，切出来的就是错的字。
  const text = "İstanbul 部署";
  assert.deepEqual(matchSegments(text, "istanbul"), [{ text, match: false }]);
});

test("a record names its target, and never falls back to an opaque server id", () => {
  const servers = [
    { id: "srv_01abc", name: "生产 API" },
    { id: "srv_02def", name: "staging-web" },
  ];
  assert.equal(historyTargetLabel({ serverId: "srv_01abc" }, servers), "生产 API");
  assert.equal(historyTargetLabel({ serverId: undefined }, servers), "全局工作区");
  // 服务器已经删掉时也要说人话：srv_02xyz 对使用者不回答任何问题。
  assert.equal(historyTargetLabel({ serverId: "srv_02xyz" }, servers), "已移除的服务器");
});

test("filter totals and the archived parameter describe the same three tabs", () => {
  const counts = { active: 7, archived: 2 };
  assert.equal(historyFilterTotal(counts, "active"), 7);
  assert.equal(historyFilterTotal(counts, "archived"), 2);
  assert.equal(historyFilterTotal(counts, "all"), 9);
  // 「全部」必须是不传字段，而不是传一个第三态：契约里只有 true / false / 不传。
  assert.equal(historyFilterParam("active"), false);
  assert.equal(historyFilterParam("archived"), true);
  assert.equal(historyFilterParam("all"), undefined);
});
