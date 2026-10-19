import { memo, useLayoutEffect, useState, useSyncExternalStore } from "react";

import { createCopyController } from "../lib/clipboard.js";
import { Icon } from "./Icon.js";

export interface CopyButtonProps {
  text: string;
  label?: string;
  ariaLabel?: string;
  className?: string;
  disabled?: boolean;
}

/** Feedback updates this control only, keeping streamed answer/code rendering independent. */
export const CopyButton = memo(function CopyButton({
  text, label = "复制", ariaLabel, className, disabled = false,
}: CopyButtonProps) {
  const [controller] = useState(() => createCopyController(text));
  const state = useSyncExternalStore(controller.subscribe, controller.getSnapshot, controller.getServerSnapshot);

  useLayoutEffect(() => {
    controller.activate();
    return () => controller.dispose();
  }, [controller]);
  useLayoutEffect(() => controller.setText(text), [controller, text]);

  const writePending = state.status === "copying" || state.writePending === true;

  return (
    <span className={["copy-control", className].filter(Boolean).join(" ")} data-copy-state={state.status}>
      <button
        type="button"
        className="copy-button"
        aria-label={ariaLabel ?? label}
        disabled={disabled || text.length === 0 || writePending}
        aria-busy={writePending || undefined}
        onClick={() => { void controller.copy(); }}
      >
        <Icon name={state.status === "copied" ? "check" : "copy"} size="sm" />
        <span>{state.writePending ? "等待系统响应…" : state.status === "copied" ? "已复制" : state.status === "copying" ? "复制中…" : label}</span>
      </button>
      <span className="copy-feedback" role="status" aria-live="polite" aria-atomic="true">
        {state.error ?? (state.status === "copied" ? "已复制" : "")}
      </span>
    </span>
  );
});
