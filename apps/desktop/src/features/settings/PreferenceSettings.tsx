import type { ReactNode } from "react";
import type { AgentPermissionMode } from "@yukinal/shared";

import { APPROVAL_OPTIONS, APPROVAL_ORDER } from "../agent/run-mode.js";
import { TERMINAL_FONT_LABEL, TERMINAL_FONT_ORDER } from "../../lib/labels.js";
import {
  usePreferencesStore,
  DEFAULT_UI_PREFERENCES,
  type TerminalFont,
  type TerminalFontSize,
  type TerminalLineHeight,
} from "../../stores/preferences-store.js";
import { useWorkspaceStore } from "../../stores/workspace-store.js";

export function PreferenceSettings() {
  const preferences = usePreferencesStore();
  const setPreferences = preferences.setPreferences;
  const running = useWorkspaceStore((state) => state.agentBusy);
  const reset = () => {
    const { terminalFont, terminalFontSize, terminalLineHeight, terminalCursorBlink, agentPermissionMode } = DEFAULT_UI_PREFERENCES;
    setPreferences({ terminalFont, terminalFontSize, terminalLineHeight, terminalCursorBlink, agentPermissionMode });
  };

  return <div className="settings-stack settings-preferences">
    <Group title="终端">
      <div className="settings-form-grid">
        <Field label="终端字体">
          <select className="form-input" value={preferences.terminalFont} onChange={(event) => setPreferences({ terminalFont: event.target.value as TerminalFont })}>
            {TERMINAL_FONT_ORDER.map((font) => <option key={font} value={font}>{TERMINAL_FONT_LABEL[font]}</option>)}
          </select>
        </Field>
        <Field label="终端字号">
          <select className="form-input" value={preferences.terminalFontSize} onChange={(event) => setPreferences({ terminalFontSize: Number(event.target.value) as TerminalFontSize })}>
            {[12, 13, 14, 15, 16].map((size) => <option key={size} value={size}>{size}px</option>)}
          </select>
        </Field>
        <Field label="终端行高">
          <select className="form-input" value={preferences.terminalLineHeight} onChange={(event) => setPreferences({ terminalLineHeight: Number(event.target.value) as TerminalLineHeight })}>
            <option value={1.2}>紧凑 · 1.2</option><option value={1.35}>标准 · 1.35</option><option value={1.5}>宽松 · 1.5</option>
          </select>
        </Field>
      </div>
      <Toggle label="光标闪烁" checked={preferences.terminalCursorBlink} onChange={(terminalCursorBlink) => setPreferences({ terminalCursorBlink })} />
    </Group>

    <Group title="任务批准">
      <Field label="批准方式">
        <select className="form-input" disabled={running} value={preferences.agentPermissionMode} onChange={(event) => setPreferences({ agentPermissionMode: event.target.value as AgentPermissionMode })}>
          {APPROVAL_ORDER.map((value) => <option key={value} value={value}>{APPROVAL_OPTIONS[value].label}</option>)}
        </select>
      </Field>
      <p className="form-hint">{running ? "任务运行中，结束后可调整批准方式。" : APPROVAL_OPTIONS[preferences.agentPermissionMode].summary}</p>
    </Group>

    <footer className="settings-preferences-reset">
      <button type="button" className="text-button" disabled={running} onClick={reset}>恢复偏好默认值</button>
      <p className="form-hint">仅重置本页偏好，保留模型、工具和网络配置。</p>
    </footer>
  </div>;
}

function Group({ title, children }: { title: string; children: ReactNode }) {
  return <section className="settings-card" aria-label={title}>
    <header className="settings-card-header"><h2>{title}</h2></header>{children}
  </section>;
}

function Field({ label, children }: { label: string; children: ReactNode }) {
  return <label className="settings-field"><span className="field-label">{label}</span>{children}</label>;
}

function Toggle({ label, checked, onChange }: { label: string; checked: boolean; onChange: (checked: boolean) => void }) {
  return <label className="settings-check-row"><input type="checkbox" checked={checked} onChange={(event) => onChange(event.target.checked)} /><span>{label}</span></label>;
}
