import type { ReactNode } from "react";

/**
 * The icon size scale.
 *
 * Every icon is drawn on a 24×24 viewBox, so a size is just the rendered
 * square in CSS pixels. The scale is deliberately short: a control should
 * pick the step that matches its own height, not invent a pixel value.
 *
 *   xs  12  inline chevrons and the smallest affordances
 *   sm  14  icons that sit inside a control next to a text label
 *   md  16  standalone icon buttons and header affordances
 *   lg  18  primary actions and card headers
 *   xl  22  empty-state marks and panel-level illustrations
 *   xxl 26  full-page empty states
 */
export const ICON_SIZE = {
  xs: 12,
  sm: 14,
  md: 16,
  lg: 18,
  xl: 22,
  xxl: 26,
} as const;

export type IconSize = keyof typeof ICON_SIZE;

/**
 * Optical stroke weight.
 *
 * `strokeWidth` is expressed in viewBox units, so a fixed value renders
 * thinner as the icon shrinks. Solving for a constant on-screen stroke keeps
 * a 12px chevron and a 26px empty-state mark looking like the same drawing.
 *
 * The bounds are viewBox units, not pixels, so they have to be read against
 * the scale above. The ceiling of 3 only ever binds at `xs`, where the exact
 * correction would be 3.0 anyway; it is there to stop a future 8px step from
 * demanding a stroke so thick that the paths of a 24-unit drawing merge. The
 * floor of 1.5 only ever binds at `xxl`, where the exact correction would be
 * 1.385 and a hairline would start to shimmer.
 */
const OPTICAL_STROKE_PX = 1.5;
const VIEW_BOX = 24;
const MIN_STROKE = 1.5;
const MAX_STROKE = 3;

function opticalStrokeWidth(size: number): number {
  const corrected = (OPTICAL_STROKE_PX * VIEW_BOX) / size;
  return Math.min(MAX_STROKE, Math.max(MIN_STROKE, Math.round(corrected * 100) / 100));
}

export type IconProps = {
  name: IconName;
  /** A step on the size scale, or an explicit pixel size for one-off cases. */
  size?: IconSize | number;
  /** Override the optically corrected stroke weight. */
  strokeWidth?: number;
};

export function Icon({ name, size = "md", strokeWidth }: IconProps) {
  const px = typeof size === "number" ? size : ICON_SIZE[size];
  return (
    <svg
      aria-hidden="true"
      className="icon"
      width={px}
      height={px}
      viewBox={`0 0 ${VIEW_BOX} ${VIEW_BOX}`}
      fill="none"
      stroke="currentColor"
      strokeWidth={strokeWidth ?? opticalStrokeWidth(px)}
      strokeLinecap="round"
      strokeLinejoin="round"
      focusable="false"
    >
      {iconBody(name)}
    </svg>
  );
}

export type IconName =
  | "activity"
  | "agent"
  | "archive"
  | "arrowUp"
  | "chevronDown"
  | "chevronRight"
  | "chevronUp"
  | "close"
  | "connect"
  | "disconnect"
  | "edit"
  | "file"
  | "folder"
  | "logs"
  | "plus"
  | "projects"
  | "refresh"
  | "search"
  | "servers"
  | "services"
  | "shield"
  | "settings"
  | "sparkle"
  | "stop"
  | "terminal"
  | "trash"
  | "warning";

function iconBody(name: IconName): ReactNode {
  switch (name) {
    case "servers":
      return <><rect x="4" y="4" width="6" height="6" rx="1" /><rect x="14" y="4" width="6" height="6" rx="1" /><rect x="4" y="14" width="6" height="6" rx="1" /><rect x="14" y="14" width="6" height="6" rx="1" /></>;
    case "projects":
      return <><path d="M5 6h5a3 3 0 0 1 3 3v6a3 3 0 0 0 3 3h3" /><path d="M7 4 5 6l2 2M17 16l2 2-2 2" /></>;
    case "activity":
      return <><circle cx="12" cy="12" r="8" /><path d="M12 8v4l3 2" /></>;
    case "settings":
      return <><circle cx="12" cy="12" r="3" /><path d="M19.4 15a1.7 1.7 0 0 0 .3 1.9l.1.1-1.4 1.4-.1-.1a1.7 1.7 0 0 0-1.9-.3 1.7 1.7 0 0 0-1 1.5v.2h-2v-.2a1.7 1.7 0 0 0-1-1.5 1.7 1.7 0 0 0-1.9.3l-.1.1L9 17l.1-.1a1.7 1.7 0 0 0 .3-1.9 1.7 1.7 0 0 0-1.5-1H7.7v-2h.2a1.7 1.7 0 0 0 1.5-1 1.7 1.7 0 0 0-.3-1.9L9 9l1.4-1.4.1.1a1.7 1.7 0 0 0 1.9.3 1.7 1.7 0 0 0 1-1.5v-.2h2v.2a1.7 1.7 0 0 0 1 1.5 1.7 1.7 0 0 0 1.9-.3l.1-.1L20 9l-.1.1a1.7 1.7 0 0 0-.3 1.9 1.7 1.7 0 0 0 1.5 1h.2v2h-.2a1.7 1.7 0 0 0-1.7 1Z" /></>;
    case "agent":
    case "sparkle":
      return <path d="m12 3 1.7 5.3L19 10l-5.3 1.7L12 17l-1.7-5.3L5 10l5.3-1.7L12 3Zm6.5 12.5.7 2.3 2.3.7-2.3.7-.7 2.3-.7-2.3-2.3-.7 2.3-.7.7-2.3Z" />;
    case "plus":
      return <><path d="M12 5v14M5 12h14" /></>;
    /* 归档：对话记录里「收起来但还留着」的动作。用箱盖 + 箱体，而不是一个向下的箭头 ——
       箭头在这套界面里已经表示「折叠/展开」（chevronDown），两者同时出现会互相抵消。 */
    case "archive":
      return <><rect x="3.5" y="5" width="17" height="4" rx="1" /><path d="M5.5 9v10.2a1.3 1.3 0 0 0 1.3 1.3h10.4a1.3 1.3 0 0 0 1.3-1.3V9" /><path d="M10 13h4" /></>;
    case "search":
      return <><circle cx="10.8" cy="10.8" r="6.3" /><path d="m16 16 4 4" /></>;
    case "refresh":
      return <><path d="M20 11a8 8 0 0 0-14.8-4L4 9" /><path d="M4 4v5h5" /><path d="M4 13a8 8 0 0 0 14.8 4L20 15" /><path d="M20 20v-5h-5" /></>;
    case "close":
      return <><path d="m6 6 12 12M18 6 6 18" /></>;
    case "connect":
      return <><path d="M7 17 17 7" /><path d="M9 7h8v8" /></>;
    case "disconnect":
      return <><path d="M8 16 16 8" /><path d="M5 12h5M14 12h5" /></>;
    case "edit":
      return <><path d="m4 16-.8 4.8L8 20l11-11a2.1 2.1 0 0 0-3-3L5 17Z" /><path d="m14.5 7.5 2 2" /></>;
    case "trash":
      return <><path d="M5 7h14M10 11v5M14 11v5" /><path d="M8 7l.7-2h6.6l.7 2M7 7l.8 13h8.4L17 7" /></>;
    case "arrowUp":
      return <><path d="M12 19V5M6 11l6-6 6 6" /></>;
    case "chevronUp":
      return <path d="m6 14 6-6 6 6" />;
    case "chevronDown":
      return <path d="m6 10 6 6 6-6" />;
    case "chevronRight":
      return <path d="m10 6 6 6-6 6" />;
    case "warning":
      return <><path d="m12 4 9 16H3L12 4Z" /><path d="M12 9v5M12 17h.01" /></>;
    case "terminal":
      return <><path d="m5 7 5 5-5 5M13 17h6" /></>;
    case "stop":
      return <rect x="6" y="6" width="12" height="12" rx="1.5" />;
    case "folder":
      return <path d="M3.5 7.5h6l1.7 2H20.5v8.8a1.7 1.7 0 0 1-1.7 1.7H5.2a1.7 1.7 0 0 1-1.7-1.7V7.5Z" />;
    case "file":
      return <><path d="M7 3.8h6l4 4v12.4H7z" /><path d="M13 3.8v4h4M9.5 12h5M9.5 15h5" /></>;
    case "logs":
      return <><path d="M5 5h14M5 10h14M5 15h9M5 20h7" /></>;
    /* `mic` 和 `waveform` 曾在这里，是给一个语音输入按钮画的。
       那个按钮没有做出来（见 AgentComposer 里关于它的说明），图标也一直没人引用：
       全仓库搜 `"mic"` / `"waveform"` 只命中过它们自己的类型联合与 case，
       tests/icon.test.tsx 的清单里也没有它们，所以连「忘了删」的提示都没有。
       需要时再加回来，比留着一对永远不出现的图标更好 —— 死掉的绘制代码会让人
       以为某个功能已经存在。 */
    case "shield":
      return <><path d="M12 3 19 6v5c0 4.7-2.9 8-7 10-4.1-2-7-5.3-7-10V6l7-3Z" /><path d="m9 12 2 2 4-4" /></>;
    case "services":
      return <><rect x="4" y="5" width="16" height="5" rx="1" /><rect x="4" y="14" width="16" height="5" rx="1" /><path d="M8 7.5h.01M8 16.5h.01" /></>;
  }
}
