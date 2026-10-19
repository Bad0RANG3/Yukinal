# 工作页标签栏设计说明（顶部菜单 → 浏览器式标签）

- 状态：v1 已施工（分支 `feat/workspace-tab-strip`），通过桌面端类型检查、单元测试与生产构建；**未经真实桌面宿主与真实主体验收**。
- 范围：仅桌面端 UI 结构（`apps/desktop`）与为其服务的最小原生契约讨论。
- 不改：审批链、宿主硬边界、执行语义。本文所有"对 Agent 的可见性"都只是**呈现**，不授予任何执行权（见[安全与数据边界](./security.md)）。
- 依据：以下行号取自施工前的工作树（分支 `codex/ui-layout-direction`）；实现差异集中记在 2.2 与 2.3 的"实现备注"里。

## 1. 现状盘点

> 本节是**施工前**的快照（分支 `codex/ui-layout-direction`），保留下来是为了说明为什么要动结构。里面提到的 `WorkpagesLauncher`、`.workpages-launcher`、Dock 内标签条等在 v1 已被替换，现行落点见第 2 节。

### 1.1 工作页的打开与身份

- 唯一入口是 `WorkpagesLauncher`，四个按钮来自 `WORKPAGE_TOOLS = ["files","terminal","logs","services"]`（`apps/desktop/src/features/workpages/workpage-model.ts:4`），逐个渲染 `aria-label={打开${WORKPAGE_LABEL[kind]}}`（`apps/desktop/src/features/workpages/WorkpagesDock.tsx:23-27`）。
- 打开时**捕获当次选中的服务器**：`open(kind, { serverId })`，`serverId` 来自 `useWorkspaceStore.selectedServerId`（`WorkpagesDock.tsx:24-26`）。
- 页身份 = ``tool:${kind}:${serverId ?? "none"}``，因此同一工具在不同服务器上可并存，同一服务器同一工具则去重（`apps/desktop/src/features/workpages/workpages-store.ts:36-39`）。
- 另有两条非工具来源：证据页 ``evidence:${id}`` 与产物页 ``artifact:${id}``，固定停靠右侧（`workpages-store.ts:40-41`）。

### 1.2 激活、关闭与容量

- `activate` 只接受仍存在的 id，并顺带把 `expandedId` 指向新活动页（`workpages-store.ts:43`）。
- `close` 把页推入 `recent`（上限 12），活动页被关闭时活动性回退到 `pages.at(-1)`（即最近追加的一页），而不是"右邻居"（`workpages-store.ts:44-48`）。
- `showPage` 把 `pages` 截断到最近 **8** 页，被挤出的页进入 `recent`（`workpages-store.ts:25-28`）。终端页额外进入 `retainedTerminals`，用于"关闭视图但会话继续"（`workpages-store.ts:29`）。
- 重新打开走 `reopen(page)`，仍是同一 `showPage` 通道（`workpages-store.ts:42`）。

### 1.3 停靠、展开与弹窗

- 内容停靠只有左右两档：`WorkpageDock = "left" | "right"`（`workpage-model.ts:6`），默认 files/terminal 左、logs/services 右（`workpages-store.ts:38`）。
- 舞台是两列网格，左侧只显示 `visibleLeft`、右侧只显示 `visibleRight`；非活动页仍挂载但 `hidden`（`WorkpagesDock.tsx:48-49,61-62`，CSS `apps/desktop/src/features/workpages/workpages.css:10-17`）。
- 展开是单页占满整行（`workpages.css:27-28`），由 `toggleExpanded` 设置 `activeId + expandedId`（`workpages-store.ts:50`）。
- 独立窗口走原生：`openNativeWorkpage({kind, serverId})`（`apps/desktop/src/lib/ipc.ts:71-75`），弹出窗口读 `?workpage=&server=` 并以 `dock:"right"` 渲染同一份内容（`apps/desktop/src/app/WorkpageWindow.tsx:11,31`）。证据页与产物页不能弹出（`apps/desktop/src/app/AppShell.tsx:113-115`）。
- Dock 宽度 ≤720px 时降级为"只显示活动页"（`WorkpagesDock.tsx:37` 的 ResizeObserver，加 `workpages.css:103-111`）。

### 1.4 标签条与启动器的当前位置

- 启动器挂在 Agent 面板头部右侧的控制区里：`workspaceControls={<><WorkpagesLauncher />…</>}`（`AppShell.tsx:116-122`），由 `AgentHeader` 的 `.agent-header-actions` 承载（`apps/desktop/src/features/agent/AgentHeader.tsx:71-86`）。
- 标签条在 Dock 内部，是 Dock 的第一个子节点（`WorkpagesDock.tsx:57`），即位于中央栏中部而非窗口顶部。
- 容器宽度不足时，启动器文字被裁成图标（`apps/desktop/src/styles/agent-history.css:615-625` 的 `@container agent-workspace (max-width: 1000px)`）。

### 1.5 键盘与 ARIA

- 标签条：`role="tablist"` + `aria-label="已打开的工作页"`，每个标签 `role="tab"`、`aria-controls="workpage-panel-<id>"`、roving `tabIndex`（`WorkpagesDock.tsx:57`）。
- 方向键/Home/End 循环切换，焦点通过 `document.getElementById('workpage-tab-' + id)` 显式转移（`WorkpagesDock.tsx:50-55`）。注意它按**下标全量遍历 `pages`**，不区分停靠侧。
- 关闭后焦点回退：优先新活动标签，否则回退到 `.workpages-launcher button`（`WorkpagesDock.tsx:74-81`）。
- 面板侧 `role="tabpanel"` + `aria-labelledby`，且 `onFocusCapture` 会把焦点捕获当作激活（`WorkpagesDock.tsx:89`）。
- 全局焦点样式已覆盖 `[role="tab"]`（`apps/desktop/src/styles/ui-foundation.css:33-42`）。

### 1.6 持久化 vs 临时态

| 状态 | 位置 | 是否持久化 |
| --- | --- | --- |
| `pages / activeId / expandedId / retainedTerminals / recent / operationsOpen` | `workpages-store.ts:34-35` | 否，纯内存，重启清空 |
| `primary / serverPage / selectedServerId / agentBusy / agentOpen` | `apps/desktop/src/stores/workspace-store.ts:82-97` | 否 |
| 终端字体/行高/光标、Agent 运行与批准方式 | `apps/desktop/src/stores/preferences-store.ts:88-97` | 是，key `yukinal.ui-preferences.v1` |
| 首次引导已关闭 | `apps/desktop/src/features/onboarding/onboarding-store.ts:5-8` | 是 |

结论：**当前没有任何"标签栏方向/位置"偏好**，`partialize` 白名单里没有布局字段（`preferences-store.ts:88-97`），所以垂直标签未来落地时必须扩这条白名单与 `sanitizePreferences`（`preferences-store.ts:60-76`）。

## 2. v1 推荐架构

### 2.1 组件边界

最终形态（与提案的差异：标签条叫 `WorkspaceTabStrip`，启动按钮并入标签条，不再单独保留 `WorkpagesLauncher`）：

| 模块 | 职责 | 不负责 |
| --- | --- | --- |
| `WorkspaceTabStrip`（`TabStripView` + 接线） | 四个启动按钮、`pages` 顺序的标签、横向滚动、键盘导航、关闭/中键关闭 | 不渲染页面内容，不决定停靠 |
| `WorkpagesDock` | 只渲染 `workpages-stage` + `WorkpageOperations` | 不再渲染标签条与启动器 |
| `WorkpageFrame` / `WorkpageContent` | 单页内容，保持不变 | 不感知标签条 |

启动按钮为什么不单独留一个 `WorkpagesLauncher`：它在 `AgentHeader` 里时会被右侧状态区挤压、在窄容器下被裁成纯图标，而且与标签不在同一行会让人以为"打开"和"已打开"是两件事。并进标签行后，一行里从左到右就是"能打开什么 → 已经打开了什么"。

关键约束：`WorkpageContent` 仍被独立窗口复用（`WorkpagesDock.tsx:91`，被 `app/WorkpageWindow.tsx:31` 使用），所以标签条**不能**进入 `WorkpageContent`，否则弹窗里会多出一条无意义的标签栏。

### 2.2 store 形状（含方向轴）

**实现落点与原提案不同的一处**：`orientation` 没有放进 `workpages-store.ts`，而是进了 `preferences-store.ts`。原因是分层约定——`apps/desktop/src/stores/*` 不得反向 import `features/*`，而方向键映射（`tab-navigation.ts`，在 features 下）需要读这个类型；把它定义在 `stores/preferences-store.ts` 里，两边就都能 import 而不产生环。附带好处是它天然进了持久化与清洗表。

```ts
// apps/desktop/src/stores/preferences-store.ts
export type WorkpagesOrientation = "horizontal" | "vertical";   // 标签条方向
interface UiPreferences {
  workpagesOrientation: WorkpagesOrientation;                   // 默认 "horizontal"
}
// dock 仍表示内容落位（left | right），与方向轴正交，仍在 workpages-store.ts
```

三条硬要求（实现后仍是这三条）：

1. `orientation` 只影响标签条的排布方向与键盘轴向（水平 → `ArrowLeft/Right`；垂直 → `ArrowUp/Down`），**不影响** `dock`、`expandedId`、页 id。判定集中在 `apps/desktop/src/features/workpages/tab-navigation.ts` 的 `nextTabIndex`，布局与按键读同一个值。
2. v1 只渲染 `horizontal`，且**不提供可见切换入口**；但值已进 `partialize` 与 `sanitizePreferences`，v2 只需加一个设置项，不需要 store 迁移。
3. 方向轴不能塞进 `Workpage` 对象（每页一个方向会让"整个标签栏"语义碎裂），必须是 store 级单值。`workpages.test.ts` 有一条断言钉住工作页对象的键集合。

### 2.3 标签条移出 Dock 的落点

v1 把标签条 `WorkspaceTabStrip` 放在 AI 面板内部、`AgentHeader` 之下、`WorkpagesDock` 之上（`AgentPanel.tsx` 的 `workspaceWorkpages` 插槽），即：

```
AgentHeader（会话标题、运行状态、连接设置、预览与仪表台开关）
WorkspaceTabStrip   ← 新增：四个启动按钮 + 已打开的标签，整行、可横向滚动
WorkpagesDock（stage + operations）
对话正文 / 输入框
```

理由：`.agent-panel.agent-workspace` 已是中央栏的视觉容器（`ai-workspace.css:60`），位于其内部就不触碰原生标题栏的几何契约（见第 3 节）。视觉上它仍是"应用顶部工具栏"，但不是窗口坐标意义上的顶部 40px。

实现备注：启动按钮从 `AgentHeader` 的 `workspaceControls` 里搬进了标签行 —— 它们是"打开工作页"这件事的一部分，跟着标签一起横滚会失去稳定位置。`agent-history.css` 里针对 `.agent-header .workpages-launcher-button` 的折叠规则随之移到 `styles/workpages-strip.css`，断点仍是 `@container agent-workspace` 的 1000/780/560px。

渲染与接线拆成两个导出：`TabStripView`（纯 props，可 SSR 断言）与 `WorkspaceTabStrip`（读 store）。拆的理由是可测性——SSR 里 `useSyncExternalStore` 走初始快照，`setState` 之后的状态照不出来。

### 2.4 明确保持不变

- `HostKeyGate` 包裹：终端/文件/日志/服务四类内容都必须继续在 Gate 内（`WorkpagesDock.tsx:105-107`，判定逻辑 `apps/desktop/src/features/servers/HostKeyGate.tsx:55-69`）。
- 终端"关闭视图不结束会话"：`retainedTerminals` + `TerminalPane` 的 `active` 闩锁与 `open` 语义（`workpages-store.ts:29`，`apps/desktop/src/features/terminal/TerminalPane.tsx:69,119-121`）。标签栏只能改变可见性，不能改成条件挂载。
- 每页打开时捕获的 `serverId`：标签切换**不得**把它改成"当前选中服务器"。这是与右侧服务器视图标签最重要的区别。
- 原生弹出窗口、证据页、产物页渲染路径不动（`WorkpageWindow.tsx:11,31`）。

## 3. 原生标题栏：满宽 40px 条的真实风险

### 3.1 当前契约

原生融合只有在三件事同时成立时才发生：宿主报告 `updateSurface` 能力、面数据通过两侧校验、视口 ≥1181px（`apps/desktop/src/lib/ipc.ts:63-65`，`apps/desktop/src/app/caption-surface.ts:15`）。面数据必须满足：

```ts
// packages/shared/src/window-shell.ts:17-26
left < right && right <= inspectorLeft && inspectorLeft <= viewportWidth
radius <= 64
```

即 **`right` 不得越过检查器左边界**，更不可能等于视口右边缘。`readCaptionSurface` 取的 `right` 就是 AI 面板的右边界（`caption-surface.ts:23-31`）。

### 3.2 违反后的实际后果

如果 v1 直接把标签条做成**贯穿整个窗口宽度的 40px 条**并据此上报 `right = viewportWidth`：

1. TS 侧 `WindowShellSurfaceSchema` 的 refine 直接拒绝（`window-shell.ts:25`）。
2. 即使绕过 TS，C++ 侧还有一道更严的检查：`update_caption_surface` 只在满足下式时接受候选面

   `candidate.right * client.right / candidate.viewport_width <= client.right - frame_scale(window, 160)`
   （`native/desktop_host/src/windows_window_frame.cpp:379-381`）

   满宽条必然违反，结果是 `surface = nullopt`。
3. 一旦 `surface` 为空，`layout_native_content` 的 `overlay` 为假，`top = frame_scale(kChromeHeight)`（`windows_window_frame.cpp:129-130`），WebView 被**推到 40 DIP 原生标题下方**（同文件 `:137`）。于是屏幕上出现两条 40px 顶栏：上面是原生绘制、下面是 React 标签条。
4. 拖拽区随之错位：`.native-caption-controls` 把 `.workspace-brand` 与 `.agent-header` 设为 `app-region: drag`（`ai-workspace.css:75,80`），而这两块此时已不在原生标题行内；`WindowFrame` 也会因原生调用失败而不加 `captionJoined`（`apps/desktop/src/app/WindowFrame.tsx:48-55`）。
5. 原生绘制细节同样不配合满宽平面条：`paint_caption_surface` 的圆角半径由 `radius` 推导、上沿固定从 `top = 12*scale` 起画，并带 `max(1,5*scale)` 的阴影溢出（`windows_window_frame.cpp:90-108`）——满宽条需要 `radius = 0` 且 `top = 0` 的绘制分支，当前没有。

### 3.3 为什么不能"只改一处"

`titlebarOverlay` 打开后，WebView 整块被移到 `top = 0` 并挖掉右上 160 DIP 按钮区与顶部 resize 边（`windows_window_frame.cpp:137-151`）。也就是说：**窗口最顶部那 40px 是被原生托管的一行**，React 只有在其下方才有像素权。要让标签条真的占满这 40px，需要同时改：

- `packages/shared/src/window-shell.ts` 的 refine（允许 `right == viewportWidth`）；
- `windows_window_frame.cpp:373-381` 的校验与 160px 排除规则（改为"按钮区单独保留，其余全宽可取"）；
- `paint_caption_surface` 的圆角/上沿分支；
- `caption-surface.ts` 的取面逻辑（从 AI 面板几何改成标签条几何）。

这是一次跨 TS/C++ 的契约升级，且两侧都有严格 schema 与失败关闭策略，属于**原生侧工作**，不能塞进 v1。

### 3.4 v1 的正确做法

v1 把标签条放进 AI 面板内部（2.3），`readCaptionSurface` 与面数据完全不变。若某天确实需要"标签条吃掉原生 40px"，再按 3.3 四项做一次契约升级，并单独验收：拖拽区、双击最大化、右上角三键、`≥1181px` 与 `<1181px` 两条路径、以及 `smoke` 下的非融合降级路径。

## 4. HITL 约束：只用真实信号做提示

前提：本项目不是全自动托管。审批是产品语义的一部分（"自动批准只适用于明确委托……扩展范围要重新授权"，见[安全与数据边界](./security.md#用户批准意味着什么)），v1 的任何"状态提示"都不得暗示"Agent 会替你做完了"。

### 4.1 实际存在的信号（可直接读）

| 信号 | 来源 | 粒度 |
| --- | --- | --- |
| `agentBusy: boolean` | `workspace-store.ts:36,45,79`，由 `AgentPanel` 用 `run.running` 写入（`AgentPanel.tsx:129-132`） | 全局布尔 |
| `running` | `useAgentRun` 返回值（`apps/desktop/src/features/agent/useAgentRun.ts:508-526`） | 单次运行 |
| `runState` | 同上；取值 `starting / thinking / running_tool / waiting_approval / completed / failed / cancelled`（`apps/desktop/src/features/agent/transcript.ts:184-192`） | 阶段字符串 |
| `pendingApprovalIds` / `approvalStatuses` | `useAgentRun.ts:458,480`，事件 `agent.waiting_approval` / `agent.approval_expired`（`useAgentRun.ts:254-266`） | 逐审批 |
| `stopping` | `useAgentRun.ts:508-526` | 单次运行 |
| 工具调用行（含 `waiting_approval` 状态） | `apps/desktop/src/features/workpages/useWorkpageOperations.ts:35-55` | 每工具调用 |

**不存在**的东西：per-页进度百分比、计划/步骤模型、每个工作页与 Agent 动作的关联关系。因此任何"这页正在被 Agent 操作到 63%"都是伪造。

### 4.2 v1 允许的 affordance（全部可由上表算出）

1. **不做标签条内的审批提示。** 提案里这条被否了：`AgentHeader` 的状态区已经在同一屏内显示"等待审批…"（`runState === "waiting_approval"`），标签条再放一个同类按钮，等于在同一行里出现两个指向同一件事的入口，而标签条本身没有任何审批语义。审批只能在对话里的审批卡上给出，标签条保持纯导航。
2. 标签条关闭/中键关闭在 `agentBusy` 期间**不做限制**——关闭只是视图操作，`retainedTerminals` 保证终端会话不受影响，也不影响运行中的任务。
3. 操作行的既有状态着色（`workpages.css:59-60`）保持不变，不再额外加"进度"覆盖层。
4. 运行中切换左侧服务器仍被拒绝（`selectServer`/`syncServerSelection` 在 `agentBusy` 时 no-op）；标签行打开新页读的是当时选中的服务器，因此不会绕过这个限制去改变运行目标。

### 4.3 需要新管道，划出 v1

- 把"某标签正被 Agent 触碰"映射到具体工作页（需要 `tool_call` 携带 `serverId + kind` 并落到 store，`useWorkpageOperations.ts:35-55` 目前只有瞬态行）。
- 页内步骤/计划进度（需要 sidecar 侧计划模型）。
- 自动切换标签到"Agent 正在看的那一页"——这会夺走用户注意力，属于 YOLO 风格，明确不做。

## 5. 风险、测试与响应式

### 5.1 风险表

| 风险 | 触发条件 | 缓解 |
| --- | --- | --- |
| 双顶栏（原生 40px + React 标签条） | 标签条被抬进原生标题行 | v1 落在面板内部（2.3）；第 3 节四项完成前不改 `right` |
| 终端会话被误关 | 把标签可见性改成条件挂载 | 保持 `hidden` + `retainedTerminals` 不变（2.4） |
| 标签切换导致 `serverId` 漂移 | 读 `selectedServerId` 而非 `page.serverId` | 标签只读 `page`，显示 `names.get(page.serverId)`（`WorkpagesDock.tsx:57`） |
| 8 页上限导致"标签莫名消失" | 超过 8 页 | 保留 `recent` 入口；标签条溢出时滚动而非堆叠 |
| 焦点在关闭后丢失 | 关闭的是活动标签且无后继 | 现行回退链保留，垂直模式下回退目标需改为同轴邻居 |
| 垂直模式键盘轴向错乱 | 只改 CSS 未改按键映射 | `orientation` 同时驱动布局与方向键，二者同一处判定 |

### 5.2 测试计划

施工前预计会破、实际处理如下：

- `apps/desktop/tests/workpages.test.ts:48`——**未破**。关闭后继策略没改，断言原样通过。
- `apps/desktop/tests/team-chat.test.tsx:79,83-86`——**已改**。启动器搬出 `AgentHeader` 后，那条用例不再构造 `.workpages-launcher`，改为断言 `agent-workspace-controls` 里剩下的预览与连接设置按钮仍有明确可访问名。
- `apps/desktop/tests/ui-logic.test.ts:41-78`——**未破**。标签状态没有并进 workspace store。

实际新增的测试（沿用 `renderToStaticMarkup` + `node:test`，不引入 DOM 框架）：

| 文件 | 覆盖 |
| --- | --- |
| `tests/workpages.test.ts` | 8 页上限与 `recent` 前移；方向轴决定方向键而不决定排布标识；方向值持久化与清洗（含 `"diagonal"`、`null` 回落到 `horizontal`，以及工作页对象键集合不变） |
| `tests/workpages-strip.test.tsx` | 空态下启动入口仍渲染且不出现空 tablist；开标签后的 `tablist`/`tab`/`aria-controls`/roving `tabIndex`；关闭按钮独立命名且不进 Tab 序；`data-orientation` 与 `aria-orientation` 同源；无目标标签不画空 `<small>` |

未做的两项：面数据不受影响（`readCaptionSurface` 的几何回归）与 `pendingApprovalIds` 提示按钮 —— 前者因为本轮没碰原生契约，后者因为 v1 明确不做按标签的状态提示（见 4.2）。

### 5.3 可访问性与键盘清单（v1 实际状态）

- [x] 标签条 `role="tablist"` + `aria-label="已打开的工作页"`，标签 `role="tab"`、`aria-selected`、roving `tabIndex`。
- [x] 面板 `role="tabpanel"` + `aria-controls`/`aria-labelledby` 双向对应；被关闭页不渲染（`pages` 里已移除），不残留可聚焦元素。
- [x] `ArrowLeft/Right`（水平）或 `ArrowUp/Down`（垂直）、`Home`/`End`；关闭用 `Delete`；鼠标中键关闭。
- [x] 关闭后焦点回退到后继标签，无邻居时回退到启动器（`tab-navigation.ts` 的 `focusWorkpageTab`）。
- [x] 关闭按钮具备 `aria-label`（含工具名），且与内容框标题栏的"关闭 X 工作页"用不同名字，避免同功能两处读成同一个按钮。
- [x] 关闭按钮 `tabIndex={-1}`：tablist 里只保留一个 Tab 停点。键盘关闭走 `Delete`，鼠标点击不受影响。
- [ ] **未做**："更多标签"溢出入口。当前靠标签行自身横向滚动；8 页上限下最坏情况仍需滚动，作为 v2 待办。
- [x] 焦点可见样式沿用 `ui-foundation.css`，未新增局部 outline。
- [x] 垂直模式下 `role="tablist"` 用 `aria-orientation="vertical"`（DOM 已按值输出，v1 不渲染该值）。

### 5.4 响应式规则

| 视口 | 标签条 | 内容舞台 |
| --- | --- | --- |
| 1440px（默认设计基准） | 单行、可滚动，全部页可见 | 两列（左/右停靠） |
| 1100px | 单行、可滚动；启动按钮文字折叠为图标（`styles/workpages-strip.css` 的 1000px 容器规则） | 仍两列，但窗口已退出原生融合（`caption-surface.ts:15`），保留完整原生标题 |
| 680px | 单行、横向滚动；提示文案隐藏（780px）、服务器名隐藏（560px） | ≤720px 只显示活动页 |
| 垂直方向（`orientation: "vertical"`） | 固定宽度侧列，纵向滚动，`aria-orientation="vertical"` | 舞台宽度让出侧列；`≤720px` 时垂直列折叠为带溢出菜单的窄条 |

断点全部挂在 `@container agent-workspace` 上（标签行在 AI 面板内部，跟随面板宽度而不是窗口宽度）。另有 `@media (max-height: 620px)` 把行高压到 34px。

垂直模式与 `dock: left/right` 正交：垂直标签列**不**替代左右停靠，只改变标签栏自身占位；两者同时出现时，标签列在内容左侧、停靠分栏在其右。

## 6. 待确认

1. 标签条是否允许出现在 `AgentHeader` 之外、占据"面板顶部整行"的位置（会触及第 3 节契约）——v1 未做，见 6.1。
2. 垂直标签的用户入口放在设置页还是标签条自身的溢出菜单——v1 只留数据位，不暴露 UI。
3. `close` 的后继策略是否从"最后追加页"改为"同轴邻居"——v1 未改，`workpages.test.ts` 原断言保持通过。

### 6.1 v1 施工后仍然成立的三条结论

1. **不做满宽 40px 标签行。** 第 3 节的四项契约升级没有做，`readCaptionSurface`、`update_caption_surface` 的 7 个字段与两侧校验一字未改；标签行保持在面板内部。
2. **不做方向切换入口。** `workpagesOrientation` 已持久化但只渲染 `horizontal`；设置页不新增外观调整入口。
3. **不改关闭后继策略。** `workpages.test.ts` 原有断言（关闭活动页后落到最后追加页）保持原样，因此无需为标签化改行为。

### 6.2 本轮验证

- `pnpm --filter @yukinal/desktop typecheck`：通过。
- `pnpm --filter @yukinal/desktop test`：333 项，332 通过 1 跳过（跳过项为既有用例）。
- `pnpm --filter @yukinal/desktop build`：通过。
- `pnpm -r --if-present typecheck` 与 `pnpm -r --if-present test`：四个 workspace 全绿。
- `pnpm check`（文档链接、密钥扫描、契约、类型、测试、sidecar 冒烟、CTest）：`check: green`。
- **未做**：真实桌面宿主与真实供体的实机验收；原生对照（拖动、双击最大化、≥1181px 融合路径、`smoke` 非融合路径）。这些在合入前仍需按第 3.4 节单独验收。

