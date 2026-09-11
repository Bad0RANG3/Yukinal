/**
 * presence 状态机。
 *
 * 这几条不变量比实现更重要，因为破坏它们的代价不是「动画不好看」而是界面坏掉：
 *   - `mounted` 为假时绝不能渲染节点（否则一个空盒子会挡住点击）
 *   - 退场没结束时 `mounted` 必须保持为真（否则动画根本没机会播）
 *   - `open` 在退场中途回到真，必须撤消退场（否则弹层会先消失再出现）
 */

import assert from "node:assert/strict";
import test from "node:test";

import { presenceReducer, initialPresence, EXIT_FALLBACK_MS } from "../src/hooks/presence.js";

test("首次渲染就打开的面板不重播入场动画", () => {
  assert.deepEqual(initialPresence(true), { mounted: true, closing: false });
});

test("首次渲染就关闭的面板不在 DOM 里", () => {
  assert.deepEqual(initialPresence(false), { mounted: false, closing: false });
});

test("打开后没有关闭标记", () => {
  const state = presenceReducer(initialPresence(false), { type: "open" });
  assert.deepEqual(state, { mounted: true, closing: false });
});

test("关闭保留在 DOM 里播退场，而不是立刻卸载", () => {
  const open = presenceReducer(initialPresence(false), { type: "open" });
  const closing = presenceReducer(open, { type: "close" });
  assert.deepEqual(closing, { mounted: true, closing: true });
});

test("退场结束后才真的卸载", () => {
  const closing = presenceReducer({ mounted: true, closing: true }, { type: "settled" });
  assert.deepEqual(closing, { mounted: false, closing: false });
});

/** 退场中途重新打开：必须原地复活，否则弹层会先淡出再淡入，像闪了一下。 */
test("退场中途重新打开会撤消退场", () => {
  const revived = presenceReducer({ mounted: true, closing: true }, { type: "open" });
  assert.deepEqual(revived, { mounted: true, closing: false });
});

/** 从未打开过的元素收到 close 不该被塞进 DOM —— 那会渲染出一个空节点。 */
test("未挂载时收到关闭保持未挂载", () => {
  const state = presenceReducer({ mounted: false, closing: false }, { type: "close" });
  assert.deepEqual(state, { mounted: false, closing: false });
});

test("重复关闭是幂等的", () => {
  const once = presenceReducer({ mounted: true, closing: false }, { type: "close" });
  const twice = presenceReducer(once, { type: "close" });
  assert.deepEqual(twice, once);
});

test("重复收尾是幂等的，不会把已卸载的元素弄回 DOM", () => {
  const settled = presenceReducer({ mounted: true, closing: true }, { type: "settled" });
  assert.deepEqual(presenceReducer(settled, { type: "settled" }), settled);
});

/**
 * 一个迟到的 `settled` 绝不能把已经打开的元素摘掉。
 *
 * 这是状态机侧的第二道闸：hook 在重开时会清计时器，`animationend` 也用动画名挡了
 * 一道。但「收尾」的后果是把元素移出 DOM，而计时器与 animationend 是两条独立的
 * 触发路径 —— 一旦哪条漏了，用户看到的是弹层无故消失，且极难复现。所以这条
 * 不变量写在状态机里，而不是指望两条上游路径都不出错。
 */
test("未在退场时收到 settled 不改变任何东西", () => {
  const open = { mounted: true, closing: false };
  assert.deepEqual(presenceReducer(open, { type: "settled" }), open, "退场途中被重开后，迟到的 settled 摘掉了已打开的弹层");

  const neverMounted = { mounted: false, closing: false };
  assert.deepEqual(presenceReducer(neverMounted, { type: "settled" }), neverMounted);
});

/** 退场中途重开、再收到迟到的 settled：必须仍然打开着。 */
test("重开后迟到的 settled 不会关掉弹层", () => {
  let state = initialPresence(false);
  state = presenceReducer(state, { type: "open" });
  state = presenceReducer(state, { type: "close" });
  state = presenceReducer(state, { type: "open" }); // 用户又点开了
  state = presenceReducer(state, { type: "settled" }); // 旧的计时器到点
  assert.deepEqual(state, { mounted: true, closing: false });
});

/**
 * 兜底时长必须比 CSS 的退场动画长，否则计时器会先到、把动画截断 ——
 * 表现就是退场只播了一半。`--motion-fast` 是 160ms。
 */
test("兜底时长不短于 CSS 的 --motion-fast", () => {
  assert.ok(EXIT_FALLBACK_MS >= 160, `EXIT_FALLBACK_MS is ${EXIT_FALLBACK_MS}`);
});

/** 走一遍完整的开关循环，最终必须回到「不在 DOM 里」。 */
test("开关循环结束后回到未挂载状态", () => {
  let state = initialPresence(false);
  for (let i = 0; i < 3; i += 1) {
    state = presenceReducer(state, { type: "open" });
    assert.deepEqual(state, { mounted: true, closing: false });
    state = presenceReducer(state, { type: "close" });
    assert.deepEqual(state, { mounted: true, closing: true });
    state = presenceReducer(state, { type: "settled" });
    assert.deepEqual(state, { mounted: false, closing: false });
  }
});
