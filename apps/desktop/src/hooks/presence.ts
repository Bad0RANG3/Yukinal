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
      // 从「正在退场」里被重新打开：撤消退场，原地复活，不重播入场动画。
      return state.mounted && state.closing ? { mounted: true, closing: false } : { mounted: true, closing: false };
    case "close":
      // 没挂载就没什么可退场的，保持原样，避免渲染出一个空节点。
      if (!state.mounted) return state;
      return { mounted: true, closing: true };
    case "settled":
      return { mounted: false, closing: false };
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
