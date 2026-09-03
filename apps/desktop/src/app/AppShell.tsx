import { AgentPanel } from "../features/agent/AgentPanel.js";
import { RuntimeSettings } from "../features/settings/RuntimeSettings.js";
import { RuntimeStrip } from "../features/settings/RuntimeStrip.js";
import { ServerList } from "../features/servers/ServerList.js";
import { TerminalPane } from "../features/terminal/TerminalPane.js";
import { ServerOverview } from "../features/overview/ServerOverview.js";
import { PRIMARY_NAV, useWorkspaceStore, type ServerPage } from "../stores/workspace-store.js";

const SERVER_PAGE_LABELS: Record<ServerPage, string> = {
  overview: "Overview",
  terminal: "Terminal",
  files: "Files",
  logs: "Logs",
  services: "Services",
  activity: "Activity",
};

export function AppShell() {
  const primary = useWorkspaceStore((state) => state.primary);
  const serverPage = useWorkspaceStore((state) => state.serverPage);
  const setPrimary = useWorkspaceStore((state) => state.setPrimary);
  const setServerPage = useWorkspaceStore((state) => state.setServerPage);

  return (
    <div className="flex h-full text-sm text-zinc-100">
      <nav className="flex w-44 shrink-0 flex-col border-r border-zinc-800 bg-zinc-950/60 p-2">
        <div className="px-2 py-3 text-base font-semibold tracking-tight">Yukinal</div>
        {PRIMARY_NAV.map((item) => (
          <button
            key={item}
            type="button"
            onClick={() => setPrimary(item)}
            className={`rounded-md px-2 py-1.5 text-left capitalize ${
              primary === item ? "bg-zinc-800 text-zinc-100" : "text-zinc-400 hover:bg-zinc-900"
            }`}
          >
            {item}
          </button>
        ))}
        <RuntimeStrip />
      </nav>

      <ServerList />

      <main className="flex min-w-0 flex-1 flex-col border-r border-zinc-800">
        {primary !== "servers" ? (
          <header className="border-b border-zinc-800 px-4 py-2 text-xs uppercase tracking-wide text-zinc-500 capitalize">
            {primary}
          </header>
        ) : null}
        <header className="flex items-center gap-3 border-b border-zinc-800 px-4 py-2">
          {(Object.keys(SERVER_PAGE_LABELS) as ServerPage[]).map((page) => (
            <button
              key={page}
              type="button"
              onClick={() => setServerPage(page)}
              className={`rounded px-2 py-1 ${
                serverPage === page ? "bg-zinc-800 text-zinc-100" : "text-zinc-400 hover:text-zinc-200"
              }`}
            >
              {SERVER_PAGE_LABELS[page]}
            </button>
          ))}
        </header>

        <div className="min-h-0 flex-1 overflow-auto p-4">
          {primary === "settings" ? <RuntimeSettings /> : null}
          {primary === "projects" ? <ComingSoon what="Projects" /> : null}
          {primary === "activity" ? <ComingSoon what="Activity" /> : null}
          {primary === "servers" ? (
            <>
              {serverPage === "overview" ? <ServerOverview /> : null}
              {serverPage === "terminal" ? <TerminalPane /> : null}
              {serverPage === "files" ||
              serverPage === "logs" ||
              serverPage === "services" ||
              serverPage === "activity" ? (
                <ComingSoon what={SERVER_PAGE_LABELS[serverPage]} />
              ) : null}
            </>
          ) : null}
        </div>
      </main>

      <AgentPanel />
    </div>
  );
}

function ComingSoon({ what }: { what: string }) {
  return (
    <p className="text-zinc-400">{what} — not implemented yet.</p>
  );
}
