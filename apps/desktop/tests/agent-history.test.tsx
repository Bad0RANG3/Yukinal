/**
 * 对话记录视图：钉住两件逻辑测不到的事。
 *
 * 1. **浏览器预览不伪造记录。** 这个视图读的是本机 SQLite，预览模式里没有数据库，
 *    所以它必须明说「桌面应用中可查询」，而不是画几行假会话出来 —— 一个假列表会让人
 *    以为记录真的存在。这一条只能在渲染里验（`shell === false` 的分支）。
 * 2. **删除不弹窗。** `window.confirm` 会挡住整个窗口，也是这套界面里唯一一个不属于
 *    它的对话框；重设计把它换成了行内确认。渲染测试里没有 DOM，点不动那个按钮，所以
 *    这一条按源码断言 —— 与 `labels.test.ts` 检查 `styles.css` 的写法一致。
 *
 * 行的结构与文案（分组、命中高亮、服务器名而不是 id）由 `history.test.ts` 与
 * `format.test.ts` 覆盖：那些是纯函数，不需要渲染就能测准。
 */

import assert from "node:assert/strict";
import { readdirSync, readFileSync } from "node:fs";
import test from "node:test";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { renderToStaticMarkup } from "react-dom/server";

import { AgentHistoryPane } from "../src/features/agent/AgentHistoryPane.js";

const noop = () => {};

/**
 * 记录视图的源码：绘制拆在同目录下的一组 `AgentHistory*.tsx` 里（容器、搜索与筛选、
 * 行、分组标题、行内的改名与删除确认），但下面两条断言盯的是**整个视图**的性质 ——
 * 「没有任何地方弹窗」「整行永远不会被 disabled」—— 所以这里读的是这一组文件的拼接，
 * 而不是某一个文件。判据一个字都没改，改的只是从哪里读。
 */
function historyPaneSource(): string {
  const directory = new URL("../src/features/agent/", import.meta.url);
  const files = readdirSync(directory)
    .filter((name) => /^AgentHistory.*\.tsx$/.test(name))
    .sort();
  // 没有这一条，目录改名之后上面那个 filter 会安静地读出一份空字符串，断言于是变成
  // 「空文件里没有 window.confirm」这种真而无意义的话。
  assert.ok(files.includes("AgentHistoryPane.tsx"), "the view's own files must be the ones being read");
  return files.map((name) => readFileSync(new URL(name, directory), "utf8")).join("\n");
}

function renderPane(): string {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return renderToStaticMarkup(
    <QueryClientProvider client={client}>
      <AgentHistoryPane
        activeSessionId={null}
        onClose={noop}
        onNewSession={noop}
        onOpenSession={noop}
        onSessionUpdated={noop}
        onSessionDeleted={noop}
      />
    </QueryClientProvider>,
  );
}

test("the record view offers search, the three filters, and its own exit", () => {
  const markup = renderPane();
  assert.ok(markup.includes("对话记录"), "the view names itself");
  assert.ok(markup.includes("新建任务"), "starting a new task is reachable from the record view");
  assert.ok(markup.includes("关闭对话记录"), "and so is leaving it");
  // 三个筛选各自带状态，而不是只有「进行中 / 已归档」两态：搜一段旧对话时也需要
  // 「两边一起搜」这个选项。
  for (const label of ["进行中", "已归档", "全部"]) {
    assert.ok(markup.includes(label), `the ${label} filter must exist`);
  }
  assert.ok(markup.includes('role="tablist"'), "the filters are tabs");
  // 摆出 tabs 就得连配套的东西一起摆：每个标签指向同一个面板，且只有选中的那个进 Tab
  // 序列（roving tabindex），否则读屏用户按「Tab」会依次落进三个筛选里。
  assert.ok(markup.includes('id="agent-history-panel"'), "the tabs must control a panel");
  assert.equal(
    markup.match(/aria-controls="agent-history-panel"/g)?.length,
    3,
    "all three tabs point at the panel",
  );
  assert.equal(markup.match(/tabindex="0"/g)?.length, 1, "only the selected tab is tabbable");
});

test("browser preview says the records are unreadable instead of inventing some", () => {
  const markup = renderPane();
  assert.ok(markup.includes("桌面应用中可查询记录"));
  // 没有数据库就没有会话：一个 srv_ id、一个「条消息」都不该出现。
  assert.equal(markup.includes("srv_"), false, "preview must not fabricate targets");
  assert.equal(markup.includes("条消息"), false, "preview must not fabricate rows");
  assert.ok(markup.includes("只在桌面应用里可用"), "the summary says where the data lives");
});

test("deleting a conversation confirms inside the row instead of blocking the window", () => {
  const source = historyPaneSource();
  // 调用而不是提及：这个视图的模块注释里确实写着 `window.confirm`，说的正是它为什么
  // 不在这里用。断言要盯的是调用。
  assert.equal(
    /\bwindow\.confirm\s*\(/.test(source),
    false,
    "the record view must not open a blocking confirm dialog",
  );
  // 行内确认要真的存在，而不只是把弹窗删掉：确认条自己也说明后果。
  assert.ok(source.includes("agent-history-confirm"), "the row must render its own confirmation");
  assert.ok(source.includes("不能撤销"), "the confirmation states what cannot be undone");
});

test("a row is never disabled while a write is in flight, so focus stays in the list", () => {
  // 这条按源码断言，理由和上面一样：行只在有数据时才渲染，而这个视图的数据来自 Tauri。
  //
  // `disabled` 会让正在聚焦的元素立刻失焦（焦点掉到 body），于是键盘用户每归档一条
  // 就要重新 Tab 回列表。待写入状态改用 `aria-disabled` + 事件里的 early return 表达。
  const source = historyPaneSource();
  // 「新建任务」是唯一可以真的被禁用的东西：它不在列表的焦点流程里，而且它整块换掉
  // 面板的内容。列表里的每一个按钮都只能走 `aria-disabled`。
  const hardDisabled = source.match(/[^-\w]disabled=\{/g)?.length ?? 0;
  assert.equal(hardDisabled, 2, "only 新建任务 and an empty rename title may use disabled");
  assert.ok(
    /className="button-secondary agent-history-new"[^>]*disabled=\{busy\}/.test(source),
    "the new-task button is the one that may be disabled while busy",
  );
  for (const pending of ["busy", "deleteSession.isPending", "sessions.isFetchingNextPage"]) {
    assert.ok(
      source.includes(`aria-disabled={${pending} || undefined}`),
      `${pending} must be expressed as aria-disabled`,
    );
  }
});
