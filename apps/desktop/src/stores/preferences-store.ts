import { create } from "zustand";
import { persist } from "zustand/middleware";

export const UI_PREFERENCES_STORAGE_KEY = "yukinal.ui-preferences.v1";

export type UiDensity = "compact" | "comfortable";
export type TerminalFont = "jetbrains-mono-nl" | "jetbrains-mono" | "jetbrains-nerd-mono";
export type UiFontSize = 12 | 13 | 14;
export type TerminalFontSize = 12 | 13 | 14 | 15 | 16;
export type TerminalLineHeight = 1.2 | 1.35 | 1.5;

export interface UiPreferences {
  uiFontSize: UiFontSize;
  density: UiDensity;
  terminalFont: TerminalFont;
  terminalFontSize: TerminalFontSize;
  terminalLineHeight: TerminalLineHeight;
  terminalCursorBlink: boolean;
  reduceMotion: boolean;
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
      }),
      merge: (persisted, current) => ({ ...current, ...sanitizePreferences(persisted) }),
    },
  ),
);
