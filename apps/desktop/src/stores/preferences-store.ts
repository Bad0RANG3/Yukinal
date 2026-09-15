import { create } from "zustand";
import { persist } from "zustand/middleware";
import { AGENT_RUN_MODES } from "@yukinal/shared";
import type { AgentPermissionMode, AgentRunMode } from "@yukinal/shared";

export const UI_PREFERENCES_STORAGE_KEY = "yukinal.ui-preferences.v1";

export type UiDensity = "compact" | "comfortable";
export type TerminalFont = "jetbrains-mono-nl" | "jetbrains-mono" | "jetbrains-nerd-mono";
export type UiFontSize = 12 | 13 | 14;
export type TerminalFontSize = 12 | 13 | 14 | 15 | 16;
export type TerminalLineHeight = 1.2 | 1.35 | 1.5;
export type AgentDelivery = "async" | "sync";

export interface UiPreferences {
  uiFontSize: UiFontSize;
  density: UiDensity;
  terminalFont: TerminalFont;
  terminalFontSize: TerminalFontSize;
  terminalLineHeight: TerminalLineHeight;
  terminalCursorBlink: boolean;
  reduceMotion: boolean;
  /** How an Agent run delegates tool approvals; this is sent with every run. */
  agentPermissionMode: AgentPermissionMode;
  /**
   * How far a run may go (readonly / plan / goal). Sent with every run and
   * enforced by the sidecar's permission engine, not by the prompt.
   */
  agentRunMode: AgentRunMode;
  /** Whether Send returns immediately or waits for the terminal run result. */
  agentDelivery: AgentDelivery;
}

export interface PreferencesState extends UiPreferences {
  setPreferences(patch: Partial<UiPreferences>): void;
  resetPreferences(): void;
}

export const DEFAULT_UI_PREFERENCES: UiPreferences = {
  uiFontSize: 13,
  density: "comfortable",
  terminalFont: "jetbrains-mono-nl",
  terminalFontSize: 13,
  terminalLineHeight: 1.35,
  terminalCursorBlink: true,
  reduceMotion: false,
  agentPermissionMode: "ask",
  agentRunMode: "goal",
  agentDelivery: "async",
};

function isUiFontSize(value: unknown): value is UiFontSize {
  return value === 12 || value === 13 || value === 14;
}

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

function sanitizePreferences(value: unknown): UiPreferences {
  if (!value || typeof value !== "object") return DEFAULT_UI_PREFERENCES;
  const candidate = value as Partial<UiPreferences>;
  return {
    uiFontSize: isUiFontSize(candidate.uiFontSize) ? candidate.uiFontSize : DEFAULT_UI_PREFERENCES.uiFontSize,
    density: candidate.density === "compact" || candidate.density === "comfortable" ? candidate.density : DEFAULT_UI_PREFERENCES.density,
    terminalFont:
      candidate.terminalFont === "jetbrains-mono" || candidate.terminalFont === "jetbrains-nerd-mono" || candidate.terminalFont === "jetbrains-mono-nl"
        ? candidate.terminalFont
        : DEFAULT_UI_PREFERENCES.terminalFont,
    terminalFontSize: isTerminalFontSize(candidate.terminalFontSize) ? candidate.terminalFontSize : DEFAULT_UI_PREFERENCES.terminalFontSize,
    terminalLineHeight: isTerminalLineHeight(candidate.terminalLineHeight) ? candidate.terminalLineHeight : DEFAULT_UI_PREFERENCES.terminalLineHeight,
    terminalCursorBlink: typeof candidate.terminalCursorBlink === "boolean" ? candidate.terminalCursorBlink : DEFAULT_UI_PREFERENCES.terminalCursorBlink,
    reduceMotion: typeof candidate.reduceMotion === "boolean" ? candidate.reduceMotion : DEFAULT_UI_PREFERENCES.reduceMotion,
    agentPermissionMode: candidate.agentPermissionMode === "auto" || candidate.agentPermissionMode === "ask" ? candidate.agentPermissionMode : DEFAULT_UI_PREFERENCES.agentPermissionMode,
    agentRunMode: isAgentRunMode(candidate.agentRunMode) ? candidate.agentRunMode : DEFAULT_UI_PREFERENCES.agentRunMode,
    agentDelivery: candidate.agentDelivery === "sync" ? "sync" : "async",
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
      name: UI_PREFERENCES_STORAGE_KEY,
      version: 1,
      partialize: (state) => ({
        uiFontSize: state.uiFontSize,
        density: state.density,
        terminalFont: state.terminalFont,
        terminalFontSize: state.terminalFontSize,
        terminalLineHeight: state.terminalLineHeight,
        terminalCursorBlink: state.terminalCursorBlink,
        reduceMotion: state.reduceMotion,
        agentPermissionMode: state.agentPermissionMode,
        agentRunMode: state.agentRunMode,
        agentDelivery: state.agentDelivery,
      }),
      merge: (persisted, current) => ({ ...current, ...sanitizePreferences(persisted) }),
    },
  ),
);
