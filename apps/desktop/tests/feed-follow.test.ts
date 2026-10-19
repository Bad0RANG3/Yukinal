import assert from "node:assert/strict";
import test from "node:test";
import { FeedFollowIntent, feedKeyDirection, feedTouchDirection, feedWheelDirection, listenForFeedLayout, type FeedFollowMetrics, type FeedKeyTarget } from "../src/features/agent/feed-follow.js";

const metrics = (top: number, content = 1000, height = 400): FeedFollowMetrics => ({ scrollTop: top, scrollHeight: content, clientHeight: height });

test("a small upward gesture detaches immediately and token growth never pulls it back", () => {
  const follow = new FeedFollowIntent();
  assert.equal(follow.measure(metrics(0)).target, 600);
  follow.scrolled(metrics(600));
  follow.userInput("backward", metrics(600));
  // The streaming update can beat the browser's physical scroll event.
  assert.equal(follow.measure(metrics(600, 1002)).target, null);
  follow.scrolled(metrics(596, 1002));
  assert.deepEqual(follow.measure(metrics(596, 1100)), { target: null, showJumpControl: true });
  for (let top = 595; top >= 585; top--) {
    follow.scrolled(metrics(top, 1100)); // upward momentum, without a new wheel event
    assert.equal(follow.measure(metrics(top, 1200)).target, null);
  }
});

test("only an explicit down gesture reaching the bottom restores automatic following", () => {
  const follow = new FeedFollowIntent();
  follow.measure(metrics(600));
  follow.userInput("backward", metrics(600));
  follow.scrolled(metrics(580));
  follow.userInput("forward", metrics(580));
  follow.scrolled(metrics(590));
  assert.equal(follow.measure(metrics(590, 1010)).target, null);
  follow.userInput("forward", metrics(590, 1010));
  follow.scrolled(metrics(610, 1010));
  assert.deepEqual(follow.measure(metrics(610, 1050)), { target: 650, showJumpControl: false });
});

test("resize/clamping and delayed scroll events cannot turn reading history into following", () => {
  const follow = new FeedFollowIntent();
  follow.measure(metrics(600));
  follow.userInput("backward", metrics(600));
  follow.scrolled(metrics(580));
  follow.userInput("forward", metrics(580));
  // Content/viewport shrinks before the intended scroll: this is not reaching
  // the latest entry through the user's gesture.
  follow.scrolled(metrics(500, 900));
  assert.deepEqual(follow.measure(metrics(500, 900)), { target: null, showJumpControl: true });
  follow.scrolled(metrics(500, 900));
  assert.equal(follow.measure(metrics(500, 1300)).target, null);
  assert.deepEqual(follow.measure(metrics(0, 300, 0)), { target: null, showJumpControl: false });
  assert.equal(follow.measure(metrics(500, 1300)).target, null);
});

test("one downward gesture may reach the bottom through momentum, but a layout change expires that intent", () => {
  const follow = new FeedFollowIntent();
  follow.userInput("backward", metrics(600));
  follow.scrolled(metrics(400));
  follow.userInput("forward", metrics(400));
  for (const top of [450, 500, 550, 600]) follow.scrolled(metrics(top));
  assert.deepEqual(follow.measure(metrics(600, 1100)), { target: 700, showJumpControl: false });
  follow.userInput("backward", metrics(700, 1100));
  follow.scrolled(metrics(400, 1100));
  follow.userInput("forward", metrics(400, 1100));
  follow.scrolled(metrics(500, 1100));
  follow.scrolled(metrics(600, 1000));
  assert.equal(follow.measure(metrics(600, 1200)).target, null);
});

test("rail drag stays detached while held, then follows only when explicitly released at the bottom", () => {
  const follow = new FeedFollowIntent();
  follow.measure(metrics(600));
  follow.beginPointer(metrics(600));
  assert.equal(follow.measure(metrics(600, 1100)).target, null);
  follow.scrolled(metrics(350, 1100));
  follow.endPointer(metrics(350, 1100));
  assert.equal(follow.measure(metrics(350, 1200)).target, null);
  follow.beginPointer(metrics(350, 1200));
  follow.scrolled(metrics(800, 1200));
  assert.equal(follow.measure(metrics(800, 1200)).showJumpControl, true);
  follow.endPointer(metrics(800, 1200));
  assert.deepEqual(follow.measure(metrics(800, 1250)), { target: 850, showJumpControl: false });
});

test("native upward scrolling is detected without treating own programmatic scroll as user input", () => {
  const follow = new FeedFollowIntent();
  follow.measure(metrics(0));
  follow.scrolled(metrics(600));
  assert.equal(follow.measure(metrics(600, 1100)).target, 700);
  follow.scrolled(metrics(700, 1100));
  follow.scrolled(metrics(698, 1100));
  assert.equal(follow.measure(metrics(698, 1200)).target, null);
});

test("submit/latest and scope reset restore following even without a new token", () => {
  const follow = new FeedFollowIntent();
  for (const restore of [() => follow.pinToLatest(), () => follow.reset()]) {
    follow.userInput("backward", metrics(600));
    follow.scrolled(metrics(500));
    assert.equal(follow.measure(metrics(500)).target, null);
    restore();
    assert.deepEqual(follow.measure(metrics(500)), { target: 600, showJumpControl: false });
  }
  // A wheel over a short feed is not an attempt to read older content.
  follow.userInput("backward", metrics(0, 300));
  assert.equal(follow.measure(metrics(0, 1000)).target, 600);
});

test("horizontal, zoom and IME/shortcut events are not vertical conversation intent", () => {
  const wheel = (deltaY: number, deltaX = 0, ctrlKey = false, shiftKey = false) => feedWheelDirection({ deltaX, deltaY, ctrlKey, shiftKey });
  assert.equal(wheel(-.5), "backward");
  assert.equal(wheel(5), "forward");
  assert.equal(wheel(1, 10), null);
  assert.equal(wheel(3, 0, true), null);
  assert.equal(wheel(3, 0, false, true), null);
  assert.equal(wheel(0), null);
  assert.equal(feedTouchDirection({ x: 0, y: 10 }, { x: 0, y: 11 }), "backward");
  assert.equal(feedTouchDirection({ x: 0, y: 10 }, { x: 0, y: 9 }), "forward");
  assert.equal(feedTouchDirection({ x: 0, y: 10 }, { x: 12, y: 11 }), null);
  const key = (value: string, overrides = {}) => feedKeyDirection({ key: value, shiftKey: false, ctrlKey: false, metaKey: false, altKey: false, isComposing: false, ...overrides });
  for (const value of ["ArrowUp", "PageUp", "Home"]) assert.equal(key(value), "backward");
  for (const value of ["ArrowDown", "PageDown", "End", " "]) assert.equal(key(value), "forward");
  assert.equal(key(" ", { shiftKey: true }), "backward");
  assert.equal(key("Enter"), null);
  assert.equal(key("ArrowUp", { isComposing: true }), null);
  assert.equal(key("Home", { ctrlKey: true }), null);
});

test("cancelling a rail gesture or hiding its window preserves detached intent but does not leave the pointer latched", () => {
  const follow = new FeedFollowIntent();
  follow.measure(metrics(600));
  follow.beginPointer(metrics(600));
  follow.cancelPointer();
  assert.equal(follow.measure(metrics(600, 1100)).target, null);
  follow.userInput("forward", metrics(700, 1100));
  assert.equal(follow.measure(metrics(700, 1200)).target, 800);
});

const keyEvent = (key: string, overrides = {}) => ({ key, shiftKey: false, ctrlKey: false, metaKey: false, altKey: false, isComposing: false, ...overrides });
const keyTarget = (tagName: string, attributes: Record<string, string> = {}, parentElement: FeedKeyTarget | null = null): FeedKeyTarget => ({
  tagName, parentElement, getAttribute: (name) => attributes[name] ?? null,
});

test("summary activation and nested control keys never detach or reattach a conversation", () => {
  const follow = new FeedFollowIntent();
  follow.measure(metrics(600));
  follow.userInput("backward", metrics(600));
  const summaryLabel = keyTarget("SPAN", {}, keyTarget("SUMMARY"));
  const direction = feedKeyDirection(keyEvent(" "), summaryLabel);
  if (direction) follow.userInput(direction, metrics(600));
  assert.equal(direction, null);
  assert.equal(follow.measure(metrics(600, 1100)).target, null, "Space toggles details rather than restoring following");
  follow.pinToLatest();
  const reverse = feedKeyDirection(keyEvent(" ", { shiftKey: true }), summaryLabel);
  if (reverse) follow.userInput(reverse, metrics(600));
  assert.equal(reverse, null);
  assert.equal(follow.measure(metrics(600, 1200)).target, 800, "summary activation does not become a history-reading gesture");
});

test("input, editable and semantic widgets keep their keyboard actions while feed and rail retain supported scrolling keys", () => {
  const controls = [
    ...["INPUT", "TEXTAREA", "SELECT", "BUTTON", "SUMMARY", "AUDIO", "VIDEO"].map((tag) => keyTarget(tag)),
    keyTarget("A", { href: "https://example.invalid" }),
    keyTarget("SPAN", {}, keyTarget("DIV", { contenteditable: "plaintext-only" })),
    ...["button", "slider", "combobox", "checkbox", "spinbutton", "treeitem"].map((role) => keyTarget("SPAN", {}, keyTarget("DIV", { role }))),
  ];
  for (const target of controls) for (const key of ["ArrowUp", "ArrowDown", "Home", "End", " "])
    assert.equal(feedKeyDirection(keyEvent(key), target), null, `${target.tagName} owns ${key}`);
  const feed = keyTarget("DIV", { role: "region" });
  assert.equal(feedKeyDirection(keyEvent("PageUp"), feed), "backward");
  assert.equal(feedKeyDirection(keyEvent(" "), feed), "forward");
  const rail = keyTarget("DIV", { role: "scrollbar" });
  assert.equal(feedKeyDirection(keyEvent(" "), rail, true), null);
  assert.equal(feedKeyDirection(keyEvent("End"), rail, true), "forward");
  assert.equal(feedKeyDirection(keyEvent("Home"), rail, true), "backward");
  assert.equal(feedKeyDirection(keyEvent("PageDown", { defaultPrevented: true }), feed), null);
});

test("visibility or capture loss discards stale geometry while preserving whether the user was following", () => {
  const following = new FeedFollowIntent();
  following.measure(metrics(600));
  following.cancelPointer();
  following.scrolled(metrics(450)); // browser restores a previous offset before the next measured frame
  assert.equal(following.measure(metrics(450, 1200)).target, 800);
  const detached = new FeedFollowIntent();
  detached.userInput("backward", metrics(600));
  detached.scrolled(metrics(450));
  detached.userInput("forward", metrics(450));
  detached.cancelPointer();
  detached.scrolled(metrics(600));
  assert.equal(detached.measure(metrics(600, 1200)).target, null, "a stale forward gesture cannot survive hiding");
});

test("media, details, window and optional font fallback signals stop completely after cleanup", () => {
  const feed = new EventTarget();
  const viewport = new EventTarget();
  const fonts = new EventTarget();
  let first = 0;
  const stop = listenForFeedLayout(feed, viewport, () => { first++; }, fonts);
  const dispatch = () => {
    for (const name of ["load", "error", "toggle"]) feed.dispatchEvent(new Event(name));
    viewport.dispatchEvent(new Event("resize"));
    fonts.dispatchEvent(new Event("loadingdone"));
  };
  dispatch();
  assert.equal(first, 5);
  stop();
  stop();
  dispatch();
  assert.equal(first, 5, "no listener may retain an unmounted feed");
  let second = 0;
  const stopWithoutFonts = listenForFeedLayout(feed, viewport, () => { second++; });
  dispatch();
  assert.equal(first, 5);
  assert.equal(second, 4, "reattaching without document.fonts neither throws nor doubles listeners");
  stopWithoutFonts();
  dispatch();
  assert.equal(second, 4);
});

test("fallback layout signals update following views but never turn a detached resize into following", () => {
  const feed = new EventTarget();
  const viewport = new EventTarget();
  const follow = new FeedFollowIntent();
  follow.userInput("backward", metrics(600));
  follow.scrolled(metrics(500));
  const decisions: Array<number | null> = [];
  const stop = listenForFeedLayout(feed, viewport, () => { decisions.push(follow.measure(metrics(500, 1300)).target); });
  feed.dispatchEvent(new Event("toggle"));
  feed.dispatchEvent(new Event("load"));
  viewport.dispatchEvent(new Event("resize"));
  assert.deepEqual(decisions, [null, null, null]);
  follow.pinToLatest();
  feed.dispatchEvent(new Event("load"));
  assert.equal(decisions.at(-1), 900);
  stop();
});
