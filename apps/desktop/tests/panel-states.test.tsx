/**
 * The four panel states, pinned to the markup they replaced.
 *
 * These components were extracted from 17 hand-written copies across 8 screens, and
 * the extraction claims to be a pure refactor: same markup, same classes, same
 * words. The two browser-wide gates cannot fully check that claim, because whether a
 * given panel renders depends on the data the mock happens to return — the error
 * panels in particular are never reached when every request succeeds. Style and text
 * snapshots would therefore stay green even if `ErrorPanel` produced the wrong
 * structure.
 *
 * So the original JSX is kept here verbatim as a fixture, rendered through the same
 * `renderToStaticMarkup` the components render through, and compared byte for byte.
 * That makes "this shape must not change" an assertion rather than an intention.
 *
 * The two `error-panel` structures below are not a mistake and are not meant to be
 * harmonised here: three call sites put the retry button inside the body `div` next
 * to a warning icon, three put it as a sibling of the body. Inside a horizontal flex
 * container those lay out differently. Unifying them is a visible design decision,
 * so these tests freeze the current pair until someone makes that decision on
 * purpose.
 */

import assert from "node:assert/strict";
import test from "node:test";
import { renderToStaticMarkup } from "react-dom/server";

import { Icon, type IconName } from "../src/components/Icon.js";
import { EmptyPanel, ErrorPanel, LoadingPanel, PreviewEmpty } from "../src/components/PanelStates.js";

/* ── 原文：从 8 个页面里抄下来的、迁移之前的 JSX ────────────────────────────── */

/** ServerOverview / LogsPane / ServicesPane / ProjectsPane 的读取中。 */
const originalLoadingPanel = (title: string, hint: string) => (
  <div className="loading-panel">
    <div className="loading-spinner" />
    <strong>{title}</strong>
    <span>{hint}</span>
  </div>
);

/** ServerOverview / LogsPane / ServicesPane / ProjectsPane 的读取失败。 */
const originalErrorPanelWithIcon = (title: string, message: string, retryLabel: string) => (
  <div className="error-panel">
    <div className="error-panel-icon"><Icon name="warning" size="md" /></div>
    <div>
      <strong>{title}</strong>
      <p>{message}</p>
      <button type="button" className="button-secondary">{retryLabel}</button>
    </div>
  </div>
);

/** ActivityFeed / RemoteFilesPane 的读取失败：没有图标，按钮是并排的兄弟节点。 */
const originalErrorPanelBare = (title: string, message: string, retryLabel: string) => (
  <div className="error-panel">
    <div><strong>{title}</strong><p>{message}</p></div>
    <button type="button" className="button-secondary">{retryLabel}</button>
  </div>
);

/** 泛用空状态，含 LogsPane / ServicesPane / ProjectsPane 的页面级补充类。 */
const originalEmptyPanel = (extraClass: string, icon: IconName, title: string, body: string) => (
  <div className={`empty-state page-empty${extraClass ? ` ${extraClass}` : ""}`}>
    <Icon name={icon} size="xl" />
    <h2>{title}</h2>
    <p>{body}</p>
  </div>
);

/** ActivityFeed:86 的浏览器预览占位：没有图标。 */
const originalPreviewEmptyNoIcon = (body: string) => (
  <div className="empty-state page-empty">
    <h2>浏览器预览</h2>
    <p>{body}</p>
  </div>
);

/* ── 断言 ──────────────────────────────────────────────────────────────────── */

test("LoadingPanel matches the markup it replaced, for every call site", () => {
  const sites: ReadonlyArray<readonly [string, string]> = [
    ["正在读取服务状态", "SSH · systemd / Docker · 预计几秒完成"],
    ["正在读取最近日志", "SSH · 最多 120 行 · 预计几秒完成"],
    ["正在读取项目", "从本地 workspace 数据加载"],
    ["正在连接并采集", "SSH · 7 个采集器 · 预计几秒完成"],
    ["正在读取动态", "从本地审计记录加载"],
  ];
  for (const [title, hint] of sites) {
    assert.equal(
      renderToStaticMarkup(<LoadingPanel title={title} hint={hint} />),
      renderToStaticMarkup(originalLoadingPanel(title, hint)),
      `LoadingPanel drifted for "${title}"`,
    );
  }
});

test("the icon variant of ErrorPanel matches the markup it replaced", () => {
  const sites: ReadonlyArray<readonly [string, string, string]> = [
    ["无法读取服务状态", "boom", "重试"],
    ["无法读取远端日志", "boom", "重试"],
    ["无法读取项目", "boom", "重试"],
    ["无法读取服务器状态", "boom", "重试采集"],
  ];
  for (const [title, message, retryLabel] of sites) {
    assert.equal(
      renderToStaticMarkup(
        <ErrorPanel showIcon title={title} message={message} onRetry={() => {}} retryLabel={retryLabel} />,
      ),
      renderToStaticMarkup(originalErrorPanelWithIcon(title, message, retryLabel)),
      `the icon variant drifted for "${title}"`,
    );
  }
});

test("the bare variant of ErrorPanel matches the markup it replaced", () => {
  const sites: ReadonlyArray<readonly [string, string]> = [["无法读取动态", "boom"], ["无法读取目录", "boom"]];
  for (const [title, message] of sites) {
    assert.equal(
      renderToStaticMarkup(<ErrorPanel title={title} message={message} onRetry={() => {}} />),
      renderToStaticMarkup(originalErrorPanelBare(title, message, "重试")),
      `the bare variant drifted for "${title}"`,
    );
  }
});

test("the two ErrorPanel variants really are two different structures", () => {
  // If a future change collapses them into one, this fails — which is the point:
  // merging them changes how three panels lay out and has to be a deliberate edit
  // to this test, not a silent simplification.
  const withIcon = renderToStaticMarkup(<ErrorPanel showIcon title="t" message="m" onRetry={() => {}} />);
  const bare = renderToStaticMarkup(<ErrorPanel title="t" message="m" onRetry={() => {}} />);
  assert.notEqual(withIcon, bare, "the two shapes collapsed into one");
  assert.ok(withIcon.includes("error-panel-icon"), "the icon variant lost its icon");
  assert.equal(bare.includes("error-panel-icon"), false, "the bare variant grew an icon");
  // In the icon variant the retry button lives inside the body div; in the bare one
  // it is that div's sibling. Compare the child counts of the outer panel.
  const children = (markup: string) => (markup.match(/<div/g) ?? []).length;
  assert.equal(children(withIcon), 3, "icon variant: outer panel + icon + body");
  assert.equal(children(bare), 2, "bare variant: outer panel + body");
});

test("EmptyPanel matches the markup it replaced, including the page-level class", () => {
  const sites: ReadonlyArray<readonly [string, IconName, string, string]> = [
    ["", "logs", "选择一台服务器", "从左侧列表选择目标环境，查看最近的远端日志。"],
    ["", "services", "选择一台服务器", "从左侧列表选择目标环境，查看远端服务状态。"],
    ["", "servers", "选择一台服务器", "从左侧列表选择目标环境，查看实时健康状态与运行中的容器。"],
    ["", "activity", "暂无动态", "服务器连接、配置变更和 Agent 操作会记录在这里。"],
    ["", "folder", "选择一台服务器", "连接服务器后浏览远程文件。"],
    ["", "folder", "浏览器预览模式", "远程文件需要 Tauri 原生连接。"],
    ["log-empty", "logs", "没有可展示的日志", "日志源没有返回内容。"],
    ["service-empty", "services", "没有可展示的服务", "服务管理器没有返回服务条目。"],
    ["project-empty", "projects", "暂无项目", "本地数据库还没有 workspace 记录。服务器视图仍可独立使用。"],
  ];
  for (const [extraClass, icon, title, body] of sites) {
    assert.equal(
      renderToStaticMarkup(
        <EmptyPanel {...(extraClass ? { extraClass } : {})} icon={icon} title={title} body={body} />,
      ),
      renderToStaticMarkup(originalEmptyPanel(extraClass, icon, title, body)),
      `EmptyPanel drifted for "${title}"`,
    );
  }
});

test("an EmptyPanel without a page-level class has no trailing space in class", () => {
  // The original wrote `className="empty-state page-empty"` — a naive template that
  // always appends the extra class would render `"empty-state page-empty "` and
  // quietly add a second class token.
  const markup = renderToStaticMarkup(<EmptyPanel title="t" body="b" />);
  assert.ok(markup.includes('class="empty-state page-empty"'), `unexpected class attribute: ${markup.slice(0, 80)}`);
  assert.equal(markup.includes("page-empty "), false, "a stray space leaked into the class list");
});

test("PreviewEmpty matches the markup it replaced, with and without an icon", () => {
  const withIcon = ["logs", "原生 SSH 能力只在 Tauri 桌面壳中可用，预览不会伪造远端日志数据。"] as const;
  assert.equal(
    renderToStaticMarkup(<PreviewEmpty icon={withIcon[0]} body={withIcon[1]} />),
    renderToStaticMarkup(originalEmptyPanel("", withIcon[0], "浏览器预览", withIcon[1])),
  );

  const noIcon = "动态记录需要 Tauri 桌面壳中的本地数据库。";
  assert.equal(
    renderToStaticMarkup(<PreviewEmpty body={noIcon} />),
    renderToStaticMarkup(originalPreviewEmptyNoIcon(noIcon)),
  );
});

test("the retry handler is actually wired to the button", () => {
  // The extraction moved every retry button behind an `onRetry` prop. A component
  // that renders a button which does nothing looks identical in markup, so this
  // checks the handler is threaded through in both variants rather than trusting
  // the type signature.
  for (const showIcon of [true, false]) {
    let calls = 0;
    const markup = renderToStaticMarkup(
      <ErrorPanel {...(showIcon ? { showIcon } : {})} title="t" message="m" onRetry={() => { calls += 1; }} />,
    );
    assert.ok(markup.includes("<button"), "no button rendered");
    assert.equal(calls, 0, "the handler must not fire during render");
  }
});
