import { create } from "zustand";
import { persist } from "zustand/middleware";
import { AGENT_RUN_MODES, AGENT_REASONING_EFFORTS } from "@yukinal/shared";
import type { AgentPermissionMode, AgentRunMode, AgentReasoningEffort } from "@yukinal/shared";
import { isPreviewFixture } from "../lib/preview-fixtures.js";

export const UI_PREFERENCES_STORAGE_KEY = "yukinal.ui-preferences.v1";

export type TerminalFont = "jetbrains-mono-nl" | "jetbrains-mono" | "jetbrains-nerd-mono";
export type TerminalFontSize = 12 | 13 | 14 | 15 | 16;
export type TerminalLineHeight = 1.2 | 1.35 | 1.5;
export type AgentDelivery = "async" | "sync";

/**
 * 工作页标签栏的方向。
 *
 * 这是一个真实的方向轴，不是占位：它同时决定标签行的排布方向（CSS 的
 * `[data-orientation]`）与方向键的轴向（`tab-navigation.ts`）。v1 只渲染
 * `"horizontal"`，也不提供切换入口 —— 但值已经进持久化和清洗表，v2 加一个设置项就能
 * 落地，不必再改存储结构。
 *
 * 不要把它做成每页一个：方向是标签栏的性质，不是工作页的性质。
 */
export type WorkpagesOrientation = "horizontal" | "vertical";

export interface UiPreferences {
  terminalFont: TerminalFont;
  terminalFontSize: TerminalFontSize;
  terminalLineHeight: TerminalLineHeight;
  terminalCursorBlink: boolean;
  /** How an Agent run delegates tool approvals; this is sent with every run. */
  agentPermissionMode: AgentPermissionMode;
  /**
   * How far a run may go (readonly / plan / goal). Sent with every run and
   * enforced by the sidecar's permission engine, not by the prompt.
   */
  agentRunMode: AgentRunMode;
  /** Whether Send returns immediately or waits for the terminal run result. */
  agentDelivery: AgentDelivery;
  agentReasoningEffort: AgentReasoningEffort | null;
  /** 标签栏方向：影响排布与键盘轴向，不影响任何工作页的身份或目标。 */
  workpagesOrientation: WorkpagesOrientation;
}

export interface PreferencesState extends UiPreferences {
  setPreferences(patch: Partial<UiPreferences>): void;
  resetPreferences(): void;
}

export const DEFAULT_UI_PREFERENCES: UiPreferences = {
  terminalFont: "jetbrains-mono-nl",
  terminalFontSize: 13,
  terminalLineHeight: 1.35,
  terminalCursorBlink: true,
  agentPermissionMode: "auto",
  agentRunMode: "goal",
  agentDelivery: "async",
  agentReasoningEffort: null,
  workpagesOrientation: "horizontal",
};

function isTerminalFontSize(value: unknown): value is TerminalFontSize {
  return value === 12 || value === 13 || value === 14 || value === 15 || value === 16;
}

function isTerminalLineHeight(value: unknown): value is TerminalLineHeight {
  return value === 1.2 || value === 1.35 || value === 1.5;
}

/** Narrowing against the shared list keeps the guard in step with the contract. */
function isAgentRunMode(value: unknown): value is AgentRunMode {
  return typeof value === "string" && (AGENT_RUN_MODES as readonly string[]).includes(value);
}

export function sanitizePreferences(value: unknown): UiPreferences {
  if (!value || typeof value !== "object") return DEFAULT_UI_PREFERENCES;
  const candidate = value as Partial<UiPreferences>;
  return {
    terminalFont:
      candidate.terminalFont === "jetbrains-mono" || candidate.terminalFont === "jetbrains-nerd-mono" || candidate.terminalFont === "jetbrains-mono-nl"
        ? candidate.terminalFont
        : DEFAULT_UI_PREFERENCES.terminalFont,
    terminalFontSize: isTerminalFontSize(candidate.terminalFontSize) ? candidate.terminalFontSize : DEFAULT_UI_PREFERENCES.terminalFontSize,
    terminalLineHeight: isTerminalLineHeight(candidate.terminalLineHeight) ? candidate.terminalLineHeight : DEFAULT_UI_PREFERENCES.terminalLineHeight,
    terminalCursorBlink: typeof candidate.terminalCursorBlink === "boolean" ? candidate.terminalCursorBlink : DEFAULT_UI_PREFERENCES.terminalCursorBlink,
    agentPermissionMode: candidate.agentPermissionMode === "auto" || candidate.agentPermissionMode === "ask" ? candidate.agentPermissionMode : DEFAULT_UI_PREFERENCES.agentPermissionMode,
    agentRunMode: isAgentRunMode(candidate.agentRunMode) ? candidate.agentRunMode : DEFAULT_UI_PREFERENCES.agentRunMode,
    agentDelivery: candidate.agentDelivery === "sync" ? "sync" : "async",
    agentReasoningEffort: typeof candidate.agentReasoningEffort === "string" && (AGENT_REASONING_EFFORTS as readonly string[]).includes(candidate.agentReasoningEffort) ? candidate.agentReasoningEffort : null,
    workpagesOrientation: candidate.workpagesOrientation === "vertical" ? "vertical" : "horizontal",
  };
}

export const usePreferencesStore = create<PreferencesState>()(
  persist(
    (set) => ({
      ...DEFAULT_UI_PREFERENCES,
      setPreferences: (patch) => set((current) => sanitizePreferences({ ...current, ...patch })),
      resetPreferences: () => set(DEFAULT_UI_PREFERENCES),
    }),
    {
      name: isPreviewFixture() ? `${UI_PREFERENCES_STORAGE_KEY}.preview` : UI_PREFERENCES_STORAGE_KEY,
      // Bump this only for a breaking shape change. It must never be used to
      // re-apply a default: v2 used it to force `agentPermissionMode` back to
      // "auto", so any operator who had deliberately chosen "每次询问" got a
      // weaker approval policy without being told. A migration may repair a
      // missing or unreadable field; it may not overwrite a valid choice.
      version: 3,
      migrate: (persisted) => sanitizePreferences(persisted),
      partialize: (state) => ({
        terminalFont: state.terminalFont,
        terminalFontSize: state.terminalFontSize,
        terminalLineHeight: state.terminalLineHeight,
        terminalCursorBlink: state.terminalCursorBlink,
        agentPermissionMode: state.agentPermissionMode,
        agentRunMode: state.agentRunMode,
        agentDelivery: state.agentDelivery,
        agentReasoningEffort: state.agentReasoningEffort,
        workpagesOrientation: state.workpagesOrientation,
      }),
      merge: (persisted, current) => ({ ...current, ...sanitizePreferences(persisted) }),
    },
  ),
);
