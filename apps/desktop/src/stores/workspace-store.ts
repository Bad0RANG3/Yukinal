/**
 * Main navigation skeleton:
 *
 *   Activity | Board | Teams | Settings <- primary nav
 *   Overview | Activity <- server inspector pages
 *   Files | Terminal | Logs | Services <- server-scoped workpages in the center
 *   AI workspace (persistent center) | Activity / selected server (right)
 *
 * Server selection is contextual; it does not need its own navigation entry.
 */

import { create } from "zustand";

import type { AgentPromptPart } from "@yukinal/shared";

export const PRIMARY_NAV = ["activity", "board", "agents", "tasks", "settings"] as const;
export type PrimaryNav = (typeof PRIMARY_NAV)[number] | "servers";

export const SERVER_PAGES = ["overview", "terminal", "files", "logs", "services", "activity"] as const;
export type ServerPage = (typeof SERVER_PAGES)[number];

export interface WorkspaceState {
  primary: PrimaryNav;
  serverPage: ServerPage;
  serverDetailOpen: boolean;
  selectedServerId: string | null;
  selectedProviderId: string | null;
  selectedModel: string | null;
  selectedTaskId: string | null;
  /** Kept for run/approval integration; the central AI workspace stays visible. */
  agentOpen: boolean;
  agentDraft: string;
  /** null routes each message automatically; an explicit role is a system persona. */
  agentContextEpoch: number;
  agentAttachments: AgentPromptPart[];
  agentBusy: boolean;
  /** Shared composition view, so preparing a role can reveal the existing editor. */
  agentHistoryOpen: boolean;
  /** The active chat's editability comes from useChatSessions, never from a role. */
  agentSessionArchived: boolean;
  setAgentDraft(draft: string): void;
  resetAgentContextView(): void;
  setAgentAttachments(attachments: AgentPromptPart[]): void;
  setAgentBusy(busy: boolean): void;
  setAgentHistoryOpen(open: boolean): void;
  setAgentSessionArchived(archived: boolean): void;
  setPrimary(primary: PrimaryNav): void;
  setServerPage(page: ServerPage): void;
  selectServer(serverId: string | null): void;
  syncServerSelection(serverIds: readonly string[]): void;
  selectProvider(providerId: string | null, model?: string | null): void;
  selectModel(model: string | null): void;
  selectTask(taskId: string | null): void;
  setAgentOpen(open: boolean): void;
  toggleAgent(): void;
}

export const useWorkspaceStore = create<WorkspaceState>((set) => ({
  primary: "activity",
  serverPage: "overview",
  serverDetailOpen: false,
  selectedServerId: null,
  selectedProviderId: null,
  selectedModel: null,
  selectedTaskId: null,
  agentOpen: true,
  agentDraft: "",
  agentContextEpoch: 0,
  agentAttachments: [],
  agentBusy: false,
  agentHistoryOpen: false,
  agentSessionArchived: false,
  setAgentDraft: (agentDraft) => set({ agentDraft }),
  resetAgentContextView: () => set((state) => ({ agentContextEpoch: state.agentContextEpoch + 1 })),
  setAgentAttachments: (agentAttachments) => set({ agentAttachments }),
  setAgentBusy: (agentBusy) => set({ agentBusy }),
  setAgentHistoryOpen: (agentHistoryOpen) => set({ agentHistoryOpen }),
  setAgentSessionArchived: (agentSessionArchived) => set({ agentSessionArchived }),
  setPrimary: (primary) => set(primary === "servers" || primary === "activity" ? { primary: "activity", serverDetailOpen: false } : { primary }),
  setServerPage: (serverPage) => set({ serverPage, primary: "servers", serverDetailOpen: true }),
  selectServer: (selectedServerId) => set((state) => state.agentBusy && selectedServerId !== state.selectedServerId ? state : ({
    selectedServerId,
    primary: selectedServerId ? "servers" : state.primary,
    serverDetailOpen: Boolean(selectedServerId),
    serverPage: selectedServerId === state.selectedServerId ? state.serverPage : "overview",
  })),
  // Background refreshes can reconcile a deleted selection without taking the
  // user away from Settings or resetting a still-valid server tab.
  syncServerSelection: (serverIds) => set((state) => {
    if (state.selectedServerId === null || state.agentBusy) return state;
    if (state.selectedServerId && serverIds.includes(state.selectedServerId)) return state;
    const selectedServerId = serverIds[0] ?? null;
    return selectedServerId === state.selectedServerId ? state : { selectedServerId, serverPage: "overview" };
  }),
  selectProvider: (selectedProviderId, selectedModel = null) => set({ selectedProviderId, selectedModel }),
  selectModel: (selectedModel) => set({ selectedModel }),
  selectTask: (selectedTaskId) => set({ selectedTaskId }),
  setAgentOpen: (agentOpen) => set({ agentOpen }),
  toggleAgent: () => set((state) => ({ agentOpen: !state.agentOpen })),
}));
