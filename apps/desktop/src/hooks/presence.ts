/**
 * Presence：让「已经离开的元素」多留一会儿，好把退场动画播完。
 *
 * 为什么需要它：React 在条件为假的那一刻就把元素从 DOM 里摘掉了，CSS 根本没有
 * 机会播退场动画。于是全站只有 Agent 面板有收起动画（它手写了一套 isClosing
 * 状态），其余每一个弹层 —— 输入区的 `@` 候选、`+` 运行设置、模型列表 —— 都是
 * 凭空出现、凭空消失。
 *
 * 这里把那套手写状态提炼成一个纯函数 reducer。之所以是 reducer 而不是直接在
 * hook 里写 useState：这个模块的核心是一张状态机，而仓库里没有 DOM 测试环境
 * （测试用 node:test + react-dom/server，没有 jsdom），把状态机单独拆出来才测得到。
 * hook 部分只是「订阅 open → 派发事件 → 订阅 animationend 收尾」的薄适配层。
 *
 * 三态：
 *   - mounted=false          元素不在 DOM 里
 *   - mounted=true, closing=false  正常显示（或正在播入场动画）
 *   - mounted=true, closing=true   逻辑上已关闭，留在 DOM 里播退场动画
 *
 * ## 这里**不**管 Agent 面板，那是有意的
 *
 * `useAgentPanelShell` 里也有一套 `isClosing` / `hasBeenOpen` 状态，看起来是同一个
 * 状态机写了两遍。它不是：
 *
 *   1. 弹层退场结束后必须**离开 DOM** —— 这正是 `mounted` 这个维度的用途。
 *      Agent 面板相反，它永远留在 DOM 里，只在 `agent-panel-closing` 与
 *      `agent-panel-hidden`（`display: none`）之间切换。原因是外壳的
 *      `grid-template-columns` 按轨道数布局：面板一旦卸载，轨道数变化，整个工作区
 *      会跳一下。所以它的「隐藏」不能靠卸载表达。
 *   2. 面板退场动画 `panel-exit` 用 `animation-fill-mode: both`，结束时停在不透明度
 *      0 的姿态上，而且 `.agent-panel-closing` 自带 `pointer-events: none`。
 *      即使 `animationend` 永远不来，它也已不可见、不可点 —— 不构成缺陷。弹层这边
 *      同样用 `both`，但**必须**有兜底计时器：一旦有规则把动画改成
 *      `animation: none`（减少动效最自然的写法），填充模式随之失效，弹层会停在
 *      完全不透明、覆盖界面的状态。那才是这个计时器真正防的事。
 *
 * 把两者合并只会得到一个「有些调用方永远不解挂」的两用状态机，比现在更难读。
 * 所以这里记录差异，而不是抹平它。
 */

export type PresenceState = {
  /** 是否应该把元素渲染进 DOM。 */
  mounted: boolean;
  /** 是否正在播退场动画。渲染时加在 class 上。 */
  closing: boolean;
};

export type PresenceEvent =
  /** 逻辑上打开了。 */
  | { type: "open" }
  /** 逻辑上关闭了；有东西可退场时才进入 closing。 */
  | { type: "close" }
  /** 退场动画结束（或兜底计时器到点），可以真正卸载。 */
  | { type: "settled" };

/**
 * 初始状态。`open` 为真时直接是「已挂载且不关闭」—— 首次渲染就打开的面板
 * 不该再播一次入场动画，那会让它看起来像刚刚才出现。
 */
export function initialPresence(open: boolean): PresenceState {
  return { mounted: open, closing: false };
}

export function presenceReducer(state: PresenceState, event: PresenceEvent): PresenceState {
  switch (event.type) {
    case "open":
      // 从「正在退场」里被重新打开：撤消退场，原地复活。
      //
      // 这里曾经写成一个三元表达式，两个分支的返回值**逐字相同** —— 读起来像是
      // 「退场中途重开要特殊处理」，其实没有这回事，只是同一条赋值被写了两遍。
      // 真正要保证的是「重开不重播入场动画」，而这一点由 `mounted` 一直为 true
      // 保证（元素始终在 DOM 里，class 一变就接着原来的位置继续），跟这个三元无关。
      return { mounted: true, closing: false };
    case "close":
      // 没挂载就没什么可退场的，保持原样，避免渲染出一个空节点。
      if (!state.mounted) return state;
      return { mounted: true, closing: true };
    case "settled":
      // 只有「正在退场」才可能被收尾。这条判断是**兜底**，不是当前流程必需的：
      // hook 侧在重开时会清掉计时器，`animationend` 也用动画名挡了一道。但它值
      // 得写在状态机里，因为「收尾」的后果是把元素从 DOM 里摘掉 —— 一个迟到的
      // settled（计时器与 animationend 抢跑、或退场途中被重开）会让一个已经打开
      // 的弹层凭空消失，而且是那种极难复现的偶发。
      return state.closing ? { mounted: false, closing: false } : state;
    default:
      return state;
  }
}

/**
 * 退场动画的兜底时长。
 *
 * 为什么需要兜底：`prefers-reduced-motion` 与「减少动效」偏好会把
 * animation-duration 压到 0.001ms；这在大多数浏览器里仍会派发 animationend，
 * 但如果某条规则被写成 `animation: none`（这正是减少动效最该有的写法），
 * 事件就永远不会来 —— 弹层会永久留在 DOM 里挡住界面。所以计时器是权威，
 * animationend 只是用来让收尾更早发生。
 *
 * 160ms 与 styles.css 的 `--motion-fast` 对齐：退场比入场快，用户按下之后
 * 已经决定了，弹层该让开而不是拖沓。改动这里必须同时改那个 token。
 */
export const EXIT_FALLBACK_MS = 160;

/** 兜底计时器比动画本身多留一点余量，好让 animationend 先到。 */
export const EXIT_GRACE_MS = 40;
