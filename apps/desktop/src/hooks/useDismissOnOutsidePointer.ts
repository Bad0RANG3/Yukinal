/**
 * 弹层的「点到外面就收起」。
 *
 * `ModelPicker` 和 `AgentComposer` 各写了一遍，而且**注释也是各写一遍**：两处都用
 * `pointerdown` 而不是 `click`，两处都写下了同样的理由（在别处拖动选择文本时，
 * 浏览器会补一次 `click`，弹层会在用户还没做完动作时意外关闭）。
 *
 * 同一段理由出现两次，就意味着它可以只被改对一半：有人把其中一处换成 `click`，
 * 另一处仍然正确，而没有任何东西把两者连起来。所以这里收拢的不是代码量，是那条
 * 容易在评审里被当成风格问题放过的判断。
 *
 * 刻意*不*收拢方向键那一对：`ModelPicker` 的 `% choices.length` 依赖一条写在该
 * 文件里的不变量 —— 选择列表为空时组件提前 `return null`，带按键处理的按钮根本
 * 不存在，所以除零分支不可达。把它抽成一个通用的「取下一个下标」会抹掉那条
 * 不变量，而那正是这段代码唯一值得注意的地方。
 */

import { useEffect, useRef, type RefObject } from "react";

/**
 * 指针按在 `refs` 覆盖的区域之外时调用 `onDismiss`。
 *
 * `active` 为 false 时不挂监听（弹层没开就没有「点到外面」这回事）。
 *
 * `refs` 用数组是因为两处弹层的结构不同：`ModelPicker` 只有一个包裹触发器与面板
 * 的根节点，`AgentComposer` 的触发器与面板是兄弟节点，必须分别判断。把两种形状
 * 收敛到「若干区域，全都不含就算外面」，就不必再去争论哪种结构才对。
 *
 * 数组身份每次渲染都会变，所以真正的依赖只有 `active` 与 `onDismiss`；refs 存在
 * 一个 ref 里，每轮渲染刷新，避免 `[refs]` 这种永远成立的依赖把监听反复拆挂。
 */
export function useDismissOnOutsidePointer(
  active: boolean,
  refs: readonly RefObject<HTMLElement | null>[],
  onDismiss: () => void,
): void {
  const refsRef = useRef(refs);
  refsRef.current = refs;

  useEffect(() => {
    if (!active) return;
    const onPointerDown = (event: PointerEvent): void => {
      const target = event.target as Node;
      // 落在任意一个受管区域里都算「里面」。区域自身可能已经卸载（ref 为 null），
      // 那时 `contains` 拿到 null 会抛，所以逐个判空。
      for (const ref of refsRef.current) {
        if (ref.current?.contains(target)) return;
      }
      onDismiss();
    };
    document.addEventListener("pointerdown", onPointerDown);
    return () => document.removeEventListener("pointerdown", onPointerDown);
  }, [active, onDismiss]);
}
