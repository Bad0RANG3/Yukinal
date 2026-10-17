import type { ReactNode } from "react";

/** Page identity belongs to the window/header; this row holds live context and actions. */
export function PageToolbar({ children, actions }: { children: ReactNode; actions: ReactNode }) {
  return <div className="page-toolbar">
    <div className="page-toolbar-context">{children}</div>
    <div className="page-toolbar-actions">{actions}</div>
  </div>;
}
