import type { AnimationEventHandler, RefObject } from "react";
import type { AgentPermissionMode, AgentRunMode } from "@yukinal/shared";
import { Icon } from "../../components/Icon.js";
import { APPROVAL_ORDER, RUN_MODE_ORDER, approvalOptionSpec, runModeSpec } from "./run-mode.js";

export interface ConversationSettingsProps {
  id: string;
  open: boolean;
  closing: boolean;
  running: boolean;
  panelRef: RefObject<HTMLDivElement | null>;
  onAnimationEnd: AnimationEventHandler<HTMLDivElement>;
  onClose: () => void;
  runMode: AgentRunMode;
  onRunModeChange: (mode: AgentRunMode) => void;
  permissionMode: AgentPermissionMode;
  onPermissionModeChange: (mode: AgentPermissionMode) => void;
  delivery: "async" | "sync";
  onDeliveryChange: (value: "async" | "sync") => void;
}

/** Controlled presentation: closing the panel never changes the next request. */
export function ConversationSettings(props: ConversationSettingsProps) {
  const editable = props.open && !props.running;
  const mode = runModeSpec(props.runMode);
  const approval = approvalOptionSpec(props.permissionMode);
  const change = <T,>(callback: (value: T) => void, value: T) => { if (editable) callback(value); };
  return <div ref={props.panelRef} id={props.id}
    popover="manual" className={`agent-settings-panel anchored-popover${props.closing ? " is-closing" : ""}`}
    role="dialog" aria-labelledby={`${props.id}-title`} aria-hidden={!props.open} inert={!props.open}
    tabIndex={-1} onAnimationEnd={props.onAnimationEnd}>
    <div className="agent-settings-heading">
      <strong id={`${props.id}-title`}>对话设置</strong>
      <button type="button" className="icon-button" aria-label="关闭对话设置" disabled={!props.open} onClick={() => { if (props.open) props.onClose(); }}><Icon name="close" size="sm" /></button>
    </div>
    <section className="agent-settings-group">
      <label className="agent-settings-field"><span>运行方式</span>
        <select aria-label="运行方式" aria-describedby={`${props.id}-mode`} value={props.runMode} disabled={!editable}
          onChange={(event) => change(props.onRunModeChange, event.target.value as AgentRunMode)}>
          {RUN_MODE_ORDER.map((value) => <option key={value} value={value}>{runModeSpec(value).label}</option>)}
        </select>
      </label>
      <p className="agent-settings-description" id={`${props.id}-mode`}>{mode.summary}</p>
      <label className="agent-settings-field"><span>批准方式</span>
        <select aria-label="批准方式" aria-describedby={`${props.id}-approval`} value={props.permissionMode} disabled={!editable}
          onChange={(event) => change(props.onPermissionModeChange, event.target.value as AgentPermissionMode)}>
          {APPROVAL_ORDER.map((value) => <option key={value} value={value}>{approvalOptionSpec(value).label}</option>)}
        </select>
      </label>
      <p className="agent-settings-description" id={`${props.id}-approval`}>{approval.summary}</p>
    </section>
    <div className="agent-settings-extra">
      <div className="agent-settings-delivery" role="group" aria-label="投递方式">
        {([["async", "流式返回"], ["sync", "等待完成"]] as const).map(([value, label]) =>
          <button key={value} type="button" className={`agent-settings-option${value === props.delivery ? " is-selected" : ""}`}
            aria-pressed={value === props.delivery} disabled={!editable} onClick={() => change(props.onDeliveryChange, value)}>{label}</button>)}
      </div>
    </div>
  </div>;
}
