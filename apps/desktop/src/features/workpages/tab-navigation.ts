/**
 * 标签条的轴向与焦点规则，全部是纯函数 + 一个 DOM 收尾动作。
 *
 * 之所以单独成文件：垂直标签要在 v2 真正落地，方向键轴向必须和布局方向由**同一处**
 * 判定。若把 `ArrowRight` 硬编码在标签条 JSX 里、把 `flex-direction` 写死在 CSS 里，
 * 两者迟早会各说各话——屏幕上竖着排、键盘却左右走，这种缺陷不会抛错，只会让人
 * 觉得"键盘坏了"。
 */

import type { WorkpagesOrientation } from "../../stores/preferences-store.js";

/**
 * 返回方向键/Home/End 应该把焦点移到哪个下标，`null` 表示这个键不属于标签导航。
 *
 * 横向只认左右、纵向只认上下：交叉轴必须交还给浏览器（横向标签条用左右键滚动的
 * 行为、纵向标签列用上下键滚动内容区的行为，都不该被抢）。
 */
export function nextTabIndex(
  key: string,
  index: number,
  count: number,
  orientation: WorkpagesOrientation,
): number | null {
  if (count <= 0 || index < 0 || index >= count) return null;
  if (key === "Home") return 0;
  if (key === "End") return count - 1;
  const forward = orientation === "vertical" ? "ArrowDown" : "ArrowRight";
  const backward = orientation === "vertical" ? "ArrowUp" : "ArrowLeft";
  if (key === forward) return (index + 1) % count;
  if (key === backward) return (index + count - 1) % count;
  return null;
}

/**
 * 关闭标签后把焦点送到下一个可用的落点：同一条标签栏里的邻居，退无可退时回到
 * 启动器。焦点直接掉回 `<body>` 会让键盘用户瞬间失去位置感，所以这条路径必须
 * 显式收尾。
 *
 * `preventScroll` 默认开启：关闭/重开是"恢复"动作，不该顺带把页面或标签行滚一下。
 * 但方向键移动焦点时相反 —— 目标标签可能在横向滚动区之外，那一次必须让它滚进来，
 * 否则焦点跑到了看不见的地方。
 */
export function focusWorkpageTab(pageId: string | null, options: { preventScroll?: boolean } = {}): void {
  const tab = pageId ? document.getElementById(`workpage-tab-${pageId}`) : null;
  // 标签栏是启动按钮与标签并排的那一行；退到没有标签时落到第一个启动按钮上，
  // 也就是"新建一个工作页"这个最合理的下一步。
  const target = tab ?? document.querySelector<HTMLButtonElement>(".workspace-tabstrip-launch");
  target?.focus({ preventScroll: options.preventScroll ?? true });
}
