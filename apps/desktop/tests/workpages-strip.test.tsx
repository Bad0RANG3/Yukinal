/**
 * 工作页标签栏的渲染契约。
 *
 * 这里钉的是纯逻辑测不到、评审时又最容易悄悄丢的几件事：
 *
 * 1. **入口一直在。** 文件／终端／日志／服务四个启动按钮不依赖已打开的标签存在 ——
 *    标签为空时它们也必须渲染，否则「先打开一个工作页」这条路就断了。
 * 2. **tablist / tabpanel 是配对的。** 标签上的 `aria-controls` 必须正好等于舞台里那个
 *    `role="tabpanel"` 的 id，且一次只有一个标签进 Tab 序（roving tabIndex）。
 * 3. **关闭按钮有独立的名字。** 标签本体是 `role="tab"`，关闭是相邻的另一个按钮；两者
 *    若共用一个可访问名，屏幕阅读器读到的是两个"文件"。
 * 4. **方向是真的写在 DOM 上。** `aria-orientation` 与 `data-orientation` 来自同一个值。
 *
 * 测的是 `TabStripView` 而不是外层接线组件：SSR 里 `useSyncExternalStore` 拿的是初始
 * 快照，`setState` 之后的状态照不出来。store 一侧的规则（8 页上限、关闭后继、方向清洗）
 * 在 `workpages.test.ts` 里按纯 store 断言。
 */

import assert from "node:assert/strict";
import test from "node:test";
import { renderToStaticMarkup } from "react-dom/server";

import { TabStripView } from "../src/features/workpages/WorkspaceTabStrip.js";
import type { Workpage } from "../src/features/workpages/workpage-model.js";

const noop = () => {};

const terminalPage: Workpage = { id: "tool:terminal:server-a", kind: "terminal", title: "终端", serverId: "server-a", dock: "left" };
const logsPage: Workpage = { id: "tool:logs:server-b", kind: "logs", title: "日志", serverId: "server-b", dock: "right" };

function render(overrides: Partial<Parameters<typeof TabStripView>[0]> = {}): string {
  return renderToStaticMarkup(
    <TabStripView
      pages={[]}
      activeId={null}
      orientation="horizontal"
      serverNames={new Map([["server-a", "生产机"], ["server-b", "预发机"]])}
      onLaunch={noop}
      onActivate={noop}
      onClose={noop}
      focusTab={noop}
      {...overrides}
    />,
  );
}

test("the launcher is present before any tab exists", () => {
  const markup = render();
  for (const label of ["打开文件", "打开终端", "打开日志", "打开服务"]) {
    assert.ok(markup.includes(`aria-label="${label}"`), `missing launcher entry: ${label}`);
  }
  // 空状态没有 tablist；一条空的标签栏会让键盘停在不存在的东西上。
  assert.equal(markup.includes('role="tablist"'), false);
  assert.ok(markup.includes("对话保持可见"));
});

test("open tabs render as a named tablist with one roving tab stop", () => {
  const markup = render({ pages: [terminalPage, logsPage], activeId: logsPage.id });
  assert.ok(markup.includes('role="tablist"'));
  assert.ok(markup.includes('aria-label="已打开的工作页"'));
  // 只取标签本体，不含旁边的关闭按钮。
  const tabs = markup.match(/<button[^>]*role="tab"[^>]*>/g) ?? [];
  assert.equal(tabs.length, 2);
  const selected = tabs.filter((tag) => tag.includes('aria-selected="true"'));
  assert.equal(selected.length, 1);
  assert.ok(selected[0]!.includes('tabindex="0"'), "the active tab is the tab stop");
  for (const tag of tabs.filter((item) => item !== selected[0])) assert.ok(tag.includes('tabindex="-1"'));
  // 关闭按钮不进 Tab 序：tablist 里多一个停点会破坏 roving tabIndex 的语义。
  const closeButtons = markup.match(/<button[^>]*workspace-tabstrip-close[^>]*>/g) ?? [];
  assert.equal(closeButtons.length, 2);
  for (const tag of closeButtons) assert.ok(tag.includes('tabindex="-1"'), tag);
  // 单元格只是布局壳：`role="presentation"` 让 tablist 的直接拥有者成为 tab 本身，
  // 否则 tablist 拥有的是 div，ARIA 的 required-owned-element 规则就落空了。
  assert.equal(markup.match(/class="workspace-tabstrip-cell"[^>]*role="presentation"/g)?.length, 2);
  assert.ok(markup.includes('id="workpage-tab-tool:terminal:server-a"'));
  assert.ok(markup.includes('aria-controls="workpage-panel-tool:terminal:server-a"'));
  assert.ok(markup.includes('id="workpage-tab-tool:logs:server-b"'));
  // 服务器名而不是裸 id，标签才读得懂。
  assert.ok(markup.includes("生产机"));
  assert.ok(markup.includes("预发机"));
});

test("each close button names its own tool, and the terminal says the session survives", () => {
  const markup = render({ pages: [terminalPage, logsPage], activeId: terminalPage.id });
  assert.ok(markup.includes('aria-label="关闭终端标签"'));
  assert.ok(markup.includes('aria-label="关闭日志标签"'));
  assert.ok(markup.includes("终端会话继续"));
  // 与内容框标题栏的「关闭 X 工作页」不是同一个名字：同功能两个入口，读屏不该念成一样。
  assert.equal(markup.includes("关闭终端工作页"), false);
});

test("the orientation value is what the DOM advertises, on both attributes", () => {
  const horizontal = render({ pages: [terminalPage], activeId: terminalPage.id });
  assert.ok(horizontal.includes('data-orientation="horizontal"'));
  assert.ok(horizontal.includes('aria-orientation="horizontal"'));
  const vertical = render({ pages: [terminalPage], activeId: terminalPage.id, orientation: "vertical" });
  assert.ok(vertical.includes('data-orientation="vertical"'));
  assert.ok(vertical.includes('aria-orientation="vertical"'));
});

test("a tab without a server falls back to no target instead of an empty label", () => {
  const local: Workpage = { id: "tool:logs:none", kind: "logs", title: "日志", serverId: null, dock: "right" };
  const markup = render({ pages: [local], activeId: local.id });
  assert.ok(markup.includes('aria-label="关闭日志标签"'));
  // 没有目标就不画 `<small>`：一个空的服务器名比没有更糟。
  assert.equal(markup.includes("<small></small>"), false);
});
