import { create } from "zustand";
import { WORKPAGE_LABEL, type Workpage, type WorkpageArtifact, type WorkpageDock, type WorkpageEvidence, type WorkpageTool } from "./workpage-model.js";

interface WorkpagesState {
  pages: Workpage[];
  retainedTerminals: Workpage[];
  recent: Workpage[];
  activeId: string | null;
  expandedId: string | null;
  operationsOpen: boolean;
  openTool(kind: WorkpageTool, options?: { serverId?: string | null; dock?: WorkpageDock }): void;
  openEvidence(evidence: WorkpageEvidence): void;
  openArtifact(artifact: WorkpageArtifact): void;
  reopen(page: Workpage): void;
  activate(id: string): void;
  close(id: string): void;
  setDock(id: string, dock: WorkpageDock): void;
  toggleExpanded(id: string): void;
  setOperationsOpen(open: boolean): void;
}

function showPage(state: WorkpagesState, page: Workpage): Partial<WorkpagesState> {
  const existing = state.pages.find((item) => item.id === page.id);
  const pages = existing ? state.pages.map((item) => item.id === page.id ? { ...page, dock: item.dock } : item) : [...state.pages, page];
  const dropped = pages.slice(0, Math.max(0, pages.length - 8));
  return {
    pages: pages.slice(-8),
    recent: dropped.length ? [...dropped, ...state.recent.filter((item) => item.id !== page.id && !dropped.some((droppedPage) => droppedPage.id === item.id))].slice(0, 12) : state.recent.filter((item) => item.id !== page.id),
    retainedTerminals: page.kind === "terminal" && !state.retainedTerminals.some((item) => item.id === page.id) ? [...state.retainedTerminals, page] : state.retainedTerminals,
    activeId: page.id, expandedId: state.expandedId ? page.id : null,
  };
}

export const useWorkpagesStore = create<WorkpagesState>((set) => ({
  pages: [], retainedTerminals: [], recent: [], activeId: null, expandedId: null, operationsOpen: false,
  openTool: (kind, options = {}) => set((state) => showPage(state, {
    id: `tool:${kind}:${options.serverId ?? "none"}`, kind, title: WORKPAGE_LABEL[kind], serverId: options.serverId ?? null,
    dock: options.dock ?? (kind === "files" || kind === "terminal" ? "left" : "right"),
  })),
  openEvidence: (evidence) => set((state) => showPage(state, { id: `evidence:${evidence.id}`, kind: "evidence", title: evidence.title, evidence, serverId: evidence.execution.serverId ?? null, dock: "right" })),
  openArtifact: (artifact) => set((state) => showPage(state, { id: `artifact:${artifact.id}`, kind: "artifact", title: artifact.title, artifact, serverId: artifact.serverId ?? null, dock: "right" })),
  reopen: (page) => set((state) => showPage(state, page)),
  activate: (activeId) => set((state) => state.pages.some((page) => page.id === activeId) ? { activeId, expandedId: state.expandedId ? activeId : null } : {}),
  close: (id) => set((state) => {
    const page = state.pages.find((item) => item.id === id);
    const pages = state.pages.filter((item) => item.id !== id);
    return { pages, recent: page ? [page, ...state.recent.filter((item) => item.id !== id)].slice(0, 12) : state.recent, activeId: state.activeId === id ? pages.at(-1)?.id ?? null : state.activeId, expandedId: state.expandedId === id ? null : state.expandedId };
  }),
  setDock: (id, dock) => set((state) => ({ pages: state.pages.map((page) => page.id === id ? { ...page, dock } : page), activeId: id })),
  toggleExpanded: (id) => set((state) => ({ activeId: id, expandedId: state.expandedId === id ? null : id })),
  setOperationsOpen: (operationsOpen) => set({ operationsOpen }),
}));
