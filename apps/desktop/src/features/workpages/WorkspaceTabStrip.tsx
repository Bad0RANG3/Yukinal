/**
 * 工作页标签栏 —— 中央栏顶部那一行，浏览器式的。
 *
 * 位置：`AgentHeader` 之下、`WorkpagesDock` 之上。
 *
 *   AgentHeader（会话标题、运行状态、连接设置…）
 *   WorkspaceTabStrip  ← 这一行：新增菜单 + 已打开的标签 + 当前目标
 *   WorkpagesDock（内容舞台 + 操作记录）
 *   对话正文 / 输入框
 *
 * 为什么把标签从 `WorkpagesDock` 里搬出来：以前标签在内容框内部，工作页一多就把舞台
 * 往下压，而且标签和内容边框贴在一起，看起来像内容的一部分。现在它是独立的一行，
 * 内容舞台不会再被顶动，对话区也始终留在下面。
 *
 * 新建入口与标签分开：标签区可滚动，入口和目标在窄窗口中保持可见。
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

import { useCallback, useEffect, useRef, useState, type KeyboardEvent } from "react";

import { Icon } from "../../components/Icon.js";
import { useDismissOnOutsidePointer } from "../../hooks/useDismissOnOutsidePointer.js";
import { useServers } from "../../lib/servers.js";
import { usePreferencesStore, type WorkpagesOrientation } from "../../stores/preferences-store.js";
import { useWorkspaceStore } from "../../stores/workspace-store.js";
import { PAGE_ICON } from "./workpage-icons.js";
import { WORKPAGE_LABEL, WORKPAGE_TOOLS, type Workpage, type WorkpageTool } from "./workpage-model.js";
import { focusWorkpageTab, nextTabIndex } from "./tab-navigation.js";
import { useWorkpagesStore } from "./workpages-store.js";

/**
 * 页面数据由 props 传入；组件只保留新增菜单的展开状态。
 *
 * 拆出来不是为了复用，是为了**能测**。这个组件的价值几乎全在 DOM 契约上（tablist／
 * tabpanel 配对、roving tabIndex、关闭按钮的独立可访问名、方向写进两个属性），而这些
 * 只能从渲染结果里看出来。SSR 里 `useSyncExternalStore` 走的是初始快照，读不到
 * `setState` 之后的状态，所以把渲染和 store 分开，测试才能构造出"已经开着三个标签"
 * 的样子。
 */
export interface TabStripViewProps {
  pages: readonly Workpage[];
  recent?: readonly Workpage[];
  activeId: string | null;
  orientation: WorkpagesOrientation;
  /** 服务器 id → 显示名；查不到就退回 id。 */
  serverNames: ReadonlyMap<string, string>;
  targetName?: string;
  onChooseTarget?: () => void;
  onLaunch(kind: WorkpageTool): void;
  onActivate(id: string): void;
  onClose(id: string): void;
  onReopen?(page: Workpage): void;
  /** 焦点转移的收尾动作，默认按 id 找 DOM。 */
  focusTab?(id: string | null, options?: { preventScroll?: boolean }): void;
}

export function TabStripView({ pages, recent = [], activeId, orientation, serverNames, targetName, onChooseTarget, onLaunch, onActivate, onClose, onReopen, focusTab = focusWorkpageTab }: TabStripViewProps) {
  const [menuOpen, setMenuOpen] = useState(false);
  const menuRoot = useRef<HTMLDivElement>(null);
  const menuRef = useRef<HTMLDivElement>(null);
  const createRef = useRef<HTMLButtonElement>(null);
  useDismissOnOutsidePointer(menuOpen, [menuRoot], useCallback(() => setMenuOpen(false), []));
  useEffect(() => { if (menuOpen) menuRef.current?.querySelector<HTMLButtonElement>('button:not([disabled])')?.focus(); }, [menuOpen]);
  const closeMenu = (returnFocus = false) => {
    setMenuOpen(false);
    if (returnFocus) requestAnimationFrame(() => createRef.current?.focus());
  };
  const menuKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    if (event.key === "Escape") { event.preventDefault(); closeMenu(true); return; }
    if (!["ArrowDown", "ArrowUp", "Home", "End"].includes(event.key)) return;
    const items = Array.from(menuRef.current?.querySelectorAll<HTMLButtonElement>('button:not([disabled])') ?? []);
    if (!items.length) return;
    event.preventDefault();
    const current = items.indexOf(document.activeElement as HTMLButtonElement);
    const next = event.key === "Home" ? 0 : event.key === "End" ? items.length - 1 : event.key === "ArrowDown" ? (current + 1) % items.length : (current - 1 + items.length) % items.length;
    items[next]?.focus();
  };
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
      <div className="workspace-tabstrip-create-wrap" ref={menuRoot}>
        <button ref={createRef} type="button" className={`workspace-tabstrip-create ${menuOpen ? "is-open" : ""}`} aria-label="新建或重开工作页" title="新建或重开工作页" aria-haspopup="menu" aria-expanded={menuOpen} onClick={() => setMenuOpen((value) => !value)}><Icon name="plus" size="md" /></button>
        {menuOpen ? <div ref={menuRef} className="workspace-tabstrip-menu" role="menu" aria-label="工作页" onKeyDown={menuKeyDown} onBlur={(event) => { if (!menuRoot.current?.contains(event.relatedTarget as Node | null)) setMenuOpen(false); }}>
          <span className="workspace-tabstrip-menu-heading">新建工作页</span>
          {targetName ? WORKPAGE_TOOLS.map((kind) => <button key={kind} type="button" role="menuitem" onClick={() => { onLaunch(kind); closeMenu(); }}><Icon name={PAGE_ICON[kind]} size="sm" /><span>{WORKPAGE_LABEL[kind]}</span><small>{targetName}</small></button>) : <button type="button" role="menuitem" onClick={() => { onChooseTarget?.(); closeMenu(); }}><Icon name="servers" size="sm" /><span>选择服务器以打开工具</span></button>}
          {recent.length && onReopen ? <><span className="workspace-tabstrip-menu-heading workspace-tabstrip-menu-divider">最近关闭</span>{recent.map((page) => <button key={page.id} type="button" role="menuitem" onClick={() => { onReopen(page); closeMenu(); }}><Icon name={PAGE_ICON[page.kind]} size="sm" /><span>{page.kind === "artifact" || page.kind === "evidence" ? page.title : WORKPAGE_LABEL[page.kind]}</span><small>{page.serverId ? serverNames.get(page.serverId) ?? page.serverId : ""}</small></button>)}</> : null}
        </div> : null}
      </div>
      {pages.length ? (
        <div className="workspace-tabstrip-tabs" role="tablist" aria-label="已打开的工作页" aria-orientation={orientation}>
          {pages.map((page, index) => {
            const selected = page.id === activeId;
            const label = page.kind === "artifact" || page.kind === "evidence" ? page.title : WORKPAGE_LABEL[page.kind];
            return (
              <div className="workspace-tabstrip-cell" key={page.id} data-active={selected} role="presentation">
                <button
                  type="button"
                  role="tab"
                  id={`workpage-tab-${page.id}`}
                  aria-controls={`workpage-panel-${page.id}`}
                  aria-selected={selected}
                  tabIndex={selected ? 0 : -1}
                  className="workspace-tabstrip-tab"
                  title={`${label}${page.serverId ? ` · ${serverNames.get(page.serverId) ?? page.serverId}` : ""}`}
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
        <span className="workspace-tabstrip-empty">工作页</span>
      )}
      <button type="button" className={`workspace-tabstrip-target ${targetName ? "" : "workspace-tabstrip-choose"}`} title={targetName ? `当前目标：${targetName} · 切换服务器` : "选择服务器"} aria-label={targetName ? `当前目标：${targetName}，切换服务器` : "选择服务器"} onClick={onChooseTarget}><Icon name="servers" size="sm" /><span>{targetName ?? "选择服务器"}</span><Icon name="chevronDown" size="xs" /></button>
    </div>
  );
}

/** 接线：把 store 与纯视图拼起来。 */
export function WorkspaceTabStrip({ onChooseTarget }: { onChooseTarget?: () => void }) {
  const pages = useWorkpagesStore((state) => state.pages);
  const recent = useWorkpagesStore((state) => state.recent);
  const activeId = useWorkpagesStore((state) => state.activeId);
  const activate = useWorkpagesStore((state) => state.activate);
  const close = useWorkpagesStore((state) => state.close);
  const reopen = useWorkpagesStore((state) => state.reopen);
  const orientation = usePreferencesStore((state) => state.workpagesOrientation);
  const selectedServerId = useWorkspaceStore((state) => state.selectedServerId);
  const servers = useServers();
  const serverNames = new Map((servers.data ?? []).map((server) => [server.id, server.name]));
  const targetName = selectedServerId ? serverNames.get(selectedServerId) ?? selectedServerId : undefined;

  /** 关掉一页后焦点必须落在后继标签上，不能掉回 `<body>`。 */
  const closeTab = (id: string) => {
    close(id);
    requestAnimationFrame(() => focusWorkpageTab(useWorkpagesStore.getState().activeId));
  };

  const launch = (kind: WorkpageTool) => {
    if (!selectedServerId) return;
    useWorkpagesStore.getState().openTool(kind, { serverId: selectedServerId });
    // 新页就是活动页；顺手把焦点交给它，键盘用户不用再找一遍。
    requestAnimationFrame(() => focusWorkpageTab(useWorkpagesStore.getState().activeId));
  };
  const reopenPage = (page: Workpage) => {
    reopen(page);
    requestAnimationFrame(() => focusWorkpageTab(useWorkpagesStore.getState().activeId));
  };

  return (
    <TabStripView
      pages={pages}
      recent={recent}
      activeId={activeId}
      orientation={orientation}
      serverNames={serverNames}
      targetName={targetName}
      onChooseTarget={onChooseTarget}
      onLaunch={launch}
      onActivate={activate}
      onClose={closeTab}
      onReopen={reopenPage}
    />
  );
}
