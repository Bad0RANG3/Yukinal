/**
 * 自动恢复在界面上到底说了什么。
 *
 * 这条路径值得钉住，因为它的三种状态在用户眼里差别极大，而在代码里只差一个字段：
 *
 * - 崩溃后**已经**自己起来了：没有阻塞，但必须解释上一条回答为什么断了；
 * - 自动恢复的**预算用完了**：这时才需要用户动手，而最要紧的一句话是「不会再有下一次」；
 * - 一切正常：什么都不说。
 *
 * 把它们写成断言，同时把措辞冻结下来 —— 「已自动重启」与「已退出，可以重新启动」是两种
 * 不同的处境（一个不用做任何事，一个必须按按钮），混成一句话会让用户在两者之间猜。
 */

import assert from "node:assert/strict";
import test from "node:test";
import { renderToStaticMarkup } from "react-dom/server";

import { AgentNotices } from "../src/features/agent/AgentNotices.js";

const base = {
  shell: true,
  providerReady: true,
  statusUnreadable: false,
  spawning: false,
  onSpawn: () => {},
  onOpenSettings: () => {},
} as const;

const restart = (over: Partial<{ attempt: number; maxAttempts: number; exhausted: boolean }> = {}) => ({
  attempt: 2,
  maxAttempts: 5,
  exhausted: false,
  at: "2026-01-01T00:06:00Z",
  ...over,
});

test("a healthy agent with no restart says nothing at all", () => {
  const markup = renderToStaticMarkup(
    <AgentNotices {...base} agentRunning agentExited={false} />,
  );
  assert.equal(markup, "");
});

test("a crash that recovered says so without blocking anything", () => {
  const markup = renderToStaticMarkup(
    <AgentNotices {...base} agentRunning agentExited restart={restart()} />,
  );
  // 进程在跑、provider 就绪 —— 用户不需要做任何事，所以没有按钮。
  assert.ok(!markup.includes("<button"), markup);
  assert.ok(markup.includes("已自动重启"), markup);
  assert.ok(markup.includes("第 2/5 次"), markup);
  assert.ok(markup.includes("上一次运行已中断"), markup);
});

test("an exhausted budget asks the user to act and says there is no next attempt", () => {
  const markup = renderToStaticMarkup(
    <AgentNotices
      {...base}
      agentRunning={false}
      agentExited
      restart={restart({ attempt: 5, exhausted: true })}
      spawning={false}
    />,
  );
  assert.ok(markup.includes("不会再次自动尝试"), markup);
  assert.ok(markup.includes("5/5"), markup);
  // 这一状态下必须给出按钮：不会再有自动尝试了。
  assert.ok(markup.includes("重新启动"), markup);
  // 而「可以重新启动」那句是**没有**恢复记录时的说法，两者不能同时出现。
  assert.ok(!markup.includes("Agent 已退出，可以重新启动。"), markup);
});

test("a crashed agent without a restart keeps the original wording", () => {
  const markup = renderToStaticMarkup(
    <AgentNotices {...base} agentRunning={false} agentExited />,
  );
  assert.ok(markup.includes("Agent 已退出，可以重新启动。"), markup);
  assert.ok(markup.includes("启动 / 重试"), markup);
  assert.ok(!markup.includes("自动"), markup);
});
