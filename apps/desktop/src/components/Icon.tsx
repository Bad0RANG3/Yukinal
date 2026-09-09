import type { ReactNode } from "react";

export type IconName =
  | "activity"
  | "agent"
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
  | "settings"
  | "sparkle"
  | "stop"
  | "terminal"
  | "trash"
  | "warning";

export function Icon({ name, size = 16, strokeWidth = 1.8 }: { name: IconName; size?: number; strokeWidth?: number }) {
  return (
    <svg
      aria-hidden="true"
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth={strokeWidth}
      strokeLinecap="round"
      strokeLinejoin="round"
      focusable="false"
    >
      {iconBody(name)}
    </svg>
  );
}

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
    case "services":
      return <><rect x="4" y="5" width="16" height="5" rx="1" /><rect x="4" y="14" width="16" height="5" rx="1" /><path d="M8 7.5h.01M8 16.5h.01" /></>;
  }
}
