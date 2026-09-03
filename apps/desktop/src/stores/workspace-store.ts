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

export const PRIMARY_NAV = ["servers", "projects", "activity", "settings"] as const;
export type PrimaryNav = (typeof PRIMARY_NAV)[number];

export const SERVER_PAGES = ["overview", "terminal", "files", "logs", "services", "activity"] as const;
export type ServerPage = (typeof SERVER_PAGES)[number];

export interface WorkspaceState {
  primary: PrimaryNav;
  serverPage: ServerPage;
  selectedServerId: string | null;
  /** Right panel is never dismissible in the MVP. */
  agentOpen: boolean;
  setPrimary(primary: PrimaryNav): void;
  setServerPage(page: ServerPage): void;
  selectServer(serverId: string | null): void;
}

export const useWorkspaceStore = create<WorkspaceState>((set) => ({
  primary: "servers",
  serverPage: "overview",
  selectedServerId: null,
  agentOpen: true,
  setPrimary: (primary) => set({ primary }),
  setServerPage: (serverPage) => set({ serverPage }),
  selectServer: (selectedServerId) => set({ selectedServerId, serverPage: "overview" }),
}));
