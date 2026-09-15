/**
 * Main navigation skeleton:
 *
 *   Servers | Projects | Activity | Settings      <- primary nav
 *   Overview | Terminal | Files | Logs | Services  <- server-scoped pages
 *   Agent (always visible right)
 *
 * Overview is the default page, not the terminal (Principle 1).
 */

import { create } from "zustand";

import type { AgentPromptPart } from "@yukinal/shared";

export const PRIMARY_NAV = ["servers", "projects", "activity", "settings"] as const;
export type PrimaryNav = (typeof PRIMARY_NAV)[number];

export const SERVER_PAGES = ["overview", "terminal", "files", "logs", "services", "activity"] as const;
export type ServerPage = (typeof SERVER_PAGES)[number];

export interface WorkspaceState {
  primary: PrimaryNav;
  serverPage: ServerPage;
  selectedServerId: string | null;
  selectedProviderId: string | null;
  selectedModel: string | null;
  /** Right panel is session-scoped and opens automatically for approvals. */
  agentOpen: boolean;
  agentDraft: string;
  agentAttachments: AgentPromptPart[];
  agentBusy: boolean;
  setAgentDraft(draft: string): void;
  setAgentAttachments(attachments: AgentPromptPart[]): void;
  setAgentBusy(busy: boolean): void;
  setPrimary(primary: PrimaryNav): void;
  setServerPage(page: ServerPage): void;
  selectServer(serverId: string | null): void;
  syncServerSelection(serverIds: readonly string[]): void;
  selectProvider(providerId: string | null, model?: string | null): void;
  selectModel(model: string | null): void;
  setAgentOpen(open: boolean): void;
  toggleAgent(): void;
}

export const useWorkspaceStore = create<WorkspaceState>((set) => ({
  primary: "servers",
  serverPage: "overview",
  selectedServerId: null,
  selectedProviderId: null,
  selectedModel: null,
  agentOpen: true,
  agentDraft: "",
  agentAttachments: [],
  agentBusy: false,
  setAgentDraft: (agentDraft) => set({ agentDraft }),
  setAgentAttachments: (agentAttachments) => set({ agentAttachments }),
  setAgentBusy: (agentBusy) => set({ agentBusy }),
  setPrimary: (primary) => set({ primary }),
  setServerPage: (serverPage) => set({ serverPage, primary: "servers" }),
  selectServer: (selectedServerId) => set((state) => ({
    selectedServerId,
    primary: selectedServerId ? "servers" : state.primary,
    serverPage: selectedServerId === state.selectedServerId ? state.serverPage : "overview",
  })),
  // Background refreshes can reconcile a deleted selection without taking the
  // user away from Settings or resetting a still-valid server tab.
  syncServerSelection: (serverIds) => set((state) => {
    if (state.selectedServerId && serverIds.includes(state.selectedServerId)) return state;
    const selectedServerId = serverIds[0] ?? null;
    return selectedServerId === state.selectedServerId ? state : { selectedServerId, serverPage: "overview" };
  }),
  selectProvider: (selectedProviderId, selectedModel = null) => set({ selectedProviderId, selectedModel }),
  selectModel: (selectedModel) => set({ selectedModel }),
  setAgentOpen: (agentOpen) => set({ agentOpen }),
  toggleAgent: () => set((state) => ({ agentOpen: !state.agentOpen })),
}));
