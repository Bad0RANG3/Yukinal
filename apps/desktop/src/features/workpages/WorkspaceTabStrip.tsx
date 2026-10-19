/**
 * 工作页标签栏 —— 中央栏顶部那一行，浏览器式的。
 *
 * 位置：`AgentHeader` 之下、`WorkpagesDock` 之上。
 *
 *   AgentHeader（会话标题、运行状态、连接设置…）
 *   WorkspaceTabStrip  ← 这一行：四个启动按钮 + 已打开的标签
 *   WorkpagesDock（内容舞台 + 操作记录）
 *   对话正文 / 输入框
 *
 * 为什么把标签从 `WorkpagesDock` 里搬出来：以前标签在内容框内部，工作页一多就把舞台
 * 往下压，而且标签和内容边框贴在一起，看起来像内容的一部分。现在它是独立的一行，
 * 内容舞台不会再被顶动，对话区也始终留在下面。
 *
 * 为什么启动按钮留在标签行里、而不是收进一个"新建"菜单：终端和日志是日常运维的第一
 * 个动作，一次点击就能到，且永远停在同一像素位置 —— 标签横向滚动时它们不跟着滚走。
 * 这正是"不破坏原有运维体验"的部分。窄区域只折叠文字、不折叠入口。
 *
 * 三条不要绕过的规矩：
 *
 * 1. **标签只是视图。** 点标签只切可见性，不重新挂载内容，也不改每页打开时捕获的
 *    `serverId`（`workpages-store.ts` 的 `pages` 是唯一来源）。关掉终端标签，会话还在。
 * 2. **不授予任何执行权。** 这一行只做导航：没有"自动批准""替我继续"这类入口。审批
 *    仍然只能在对话里的审批卡上给出，权限模式仍由 `agentPermissionMode` 决定。
 * 3. **方向轴是真的。** `orientation` 同时决定排布方向（`data-orientation`，CSS 用）和
 *    方向键轴向（`tab-navigation.ts`，键盘用）。v1 不提供切换入口，但值已持久化；
 *    v2 只要加一个设置项，布局与键盘不会各说各话。
 */

import type { KeyboardEvent } from "react";

import { Icon } from "../../components/Icon.js";
import { useServers } from "../../lib/servers.js";
import { usePreferencesStore, type WorkpagesOrientation } from "../../stores/preferences-store.js";
import { useWorkspaceStore } from "../../stores/workspace-store.js";
import { PAGE_ICON } from "./workpage-icons.js";
import { WORKPAGE_LABEL, WORKPAGE_TOOLS, type Workpage, type WorkpageTool } from "./workpage-model.js";
import { focusWorkpageTab, nextTabIndex } from "./tab-navigation.js";
import { useWorkpagesStore } from "./workpages-store.js";

/**
 * 纯视图：所有状态由 props 传入。
 *
 * 拆出来不是为了复用，是为了**能测**。这个组件的价值几乎全在 DOM 契约上（tablist／
 * tabpanel 配对、roving tabIndex、关闭按钮的独立可访问名、方向写进两个属性），而这些
 * 只能从渲染结果里看出来。SSR 里 `useSyncExternalStore` 走的是初始快照，读不到
 * `setState` 之后的状态，所以把渲染和 store 分开，测试才能构造出"已经开着三个标签"
 * 的样子。
 */
export interface TabStripViewProps {
  pages: readonly Workpage[];
  activeId: string | null;
  orientation: WorkpagesOrientation;
  /** 服务器 id → 显示名；查不到就退回 id。 */
  serverNames: ReadonlyMap<string, string>;
  onLaunch(kind: WorkpageTool): void;
  onActivate(id: string): void;
  onClose(id: string): void;
  /** 焦点转移的收尾动作，默认按 id 找 DOM。 */
  focusTab?(id: string | null, options?: { preventScroll?: boolean }): void;
}

export function TabStripView({ pages, activeId, orientation, serverNames, onLaunch, onActivate, onClose, focusTab = focusWorkpageTab }: TabStripViewProps) {
  const onTabKeyDown = (event: KeyboardEvent<HTMLElement>, index: number) => {
    const page = pages[index];
    if (!page) return;
    // Delete 是桌面标签的常规关闭键；浏览器里 Ctrl+W 才是，但这里没有窗口级快捷键。
    if (event.key === "Delete") {
      event.preventDefault();
      onClose(page.id);
      return;
    }
    const next = nextTabIndex(event.key, index, pages.length, orientation);
    if (next === null) return;
    const target = pages[next];
    if (!target) return;
    event.preventDefault();
    onActivate(target.id);
    // 方向键可以选择不滚：目标标签可能在横向滚动区之外，那一次必须让它滚进来。
    focusTab(target.id, { preventScroll: false });
  };

  return (
    <div className="workspace-tabstrip" data-orientation={orientation}>
      {/* `role="group"` 而不是光秃秃的 aria-label：普通 div 上的 aria-label 会被忽略，
          四个启动按钮就读不出"它们是一组打开入口"。 */}
      <div className="workspace-tabstrip-launcher" role="group" aria-label="打开工作页">
        {WORKPAGE_TOOLS.map((kind) => (
          <button
            key={kind}
            type="button"
            className="workspace-tabstrip-launch"
            aria-label={`打开${WORKPAGE_LABEL[kind]}`}
            title={`打开${WORKPAGE_LABEL[kind]}`}
            onClick={() => onLaunch(kind)}
          >
            <Icon name={PAGE_ICON[kind]} size="sm" />
            <span>{WORKPAGE_LABEL[kind]}</span>
          </button>
        ))}
      </div>
      {pages.length ? (
        <div className="workspace-tabstrip-tabs" role="tablist" aria-label="已打开的工作页" aria-orientation={orientation}>
          {pages.map((page, index) => {
            const selected = page.id === activeId;
            const label = WORKPAGE_LABEL[page.kind];
            return (
              <div className="workspace-tabstrip-cell" key={page.id} data-active={selected}>
                <button
                  type="button"
                  role="tab"
                  id={`workpage-tab-${page.id}`}
                  aria-controls={`workpage-panel-${page.id}`}
                  aria-selected={selected}
                  tabIndex={selected ? 0 : -1}
                  className="workspace-tabstrip-tab"
                  onClick={() => onActivate(page.id)}
                  onKeyDown={(event) => onTabKeyDown(event, index)}
                  onAuxClick={(event) => {
                    if (event.button !== 1) return;
                    event.preventDefault();
                    onClose(page.id);
                  }}
                >
                  <Icon name={PAGE_ICON[page.kind]} size="sm" />
                  <span>{label}</span>
                  {page.serverId ? <small>{serverNames.get(page.serverId) ?? page.serverId}</small> : null}
                </button>
                <button
                  type="button"
                  className="workspace-tabstrip-close"
                  /* 关闭按钮不进 Tab 序：tablist 内部只该有一个 Tab 停点（活动标签）。
                     Tab 顺序里多一个按钮会破坏 roving tabIndex 的语义 —— 键盘关闭走
                     标签上的 Delete，鼠标点击不受 tabIndex 影响。 */
                  tabIndex={-1}
                  /* 名字里带"标签"，与内容框标题栏那个"关闭 X 工作页"区分开：两者做的
                     是同一件事的两个入口，但屏幕阅读器不该念出两个一模一样的按钮名。 */
                  aria-label={`关闭${label}标签`}
                  title={page.kind === "terminal" ? "关闭标签 · 终端会话继续" : "关闭标签"}
                  onClick={() => onClose(page.id)}
                >
                  <Icon name="close" size="xs" />
                </button>
              </div>
            );
          })}
        </div>
      ) : (
        <p className="workspace-tabstrip-hint">文件、终端、日志、服务会作为标签停在这里，对话保持可见。</p>
      )}
    </div>
  );
}

/** 接线：把 store 与纯视图拼起来。 */
export function WorkspaceTabStrip() {
  const pages = useWorkpagesStore((state) => state.pages);
  const activeId = useWorkpagesStore((state) => state.activeId);
  const activate = useWorkpagesStore((state) => state.activate);
  const close = useWorkpagesStore((state) => state.close);
  const orientation = usePreferencesStore((state) => state.workpagesOrientation);
  const selectedServerId = useWorkspaceStore((state) => state.selectedServerId);
  const servers = useServers();
  const serverNames = new Map((servers.data ?? []).map((server) => [server.id, server.name]));

  /** 关掉一页后焦点必须落在后继标签上，不能掉回 `<body>`。 */
  const closeTab = (id: string) => {
    close(id);
    requestAnimationFrame(() => focusWorkpageTab(useWorkpagesStore.getState().activeId));
  };

  const launch = (kind: WorkpageTool) => {
    useWorkpagesStore.getState().openTool(kind, { serverId: selectedServerId });
    // 新页就是活动页；顺手把焦点交给它，键盘用户不用再找一遍。
    requestAnimationFrame(() => focusWorkpageTab(useWorkpagesStore.getState().activeId));
  };

  return (
    <TabStripView
      pages={pages}
      activeId={activeId}
      orientation={orientation}
      serverNames={serverNames}
      onLaunch={launch}
      onActivate={activate}
      onClose={closeTab}
    />
  );
}
